//! `patchula apply`: look up the working tree here and write its updates into
//! the files, through the same engine as the TUI's apply.
use crate::output::{Paint, Style, count, kind_paint};
use crate::{Services, cli, tui};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::{self, IsTerminal, Write},
    path::Path,
};

/// When several updates propose a change to one declaration, the earliest
/// type here is the one applied.
const RANK: [&str; 5] = ["major", "minor", "patch", "pin", "digest"];
/// Some selected updates are still waiting after the command.
const WAITING: u8 = 3;

/// Diffs go to `out` and the summary to `err`, drawn in `style`, so a dry
/// run's output is a patch.
pub async fn command(
    app: &cli::App,
    options: &cli::Apply,
    services: &Services,
    out: &mut dyn Write,
    err: &mut dyn Write,
    style: &Style,
) -> Result<u8, (u8, miette::Report)> {
    let usage = |message: String| {
        (
            2,
            miette::miette!(
                help = "patchula apply only writes the working tree in the current directory",
                "{message}"
            ),
        )
    };
    if !options.repositories.is_empty() || options.all_repos {
        return Err(usage(format!(
            "patchula apply takes no {}; for GitHub repositories, `patchula run` opens pull requests",
            if options.all_repos {
                "--all-repos"
            } else {
                "repository"
            }
        )));
    }
    if options.copier && (options.check || app.json) {
        return Err(usage(format!(
            "--copier cannot run with {}: copier writes the tree and talks on the terminal",
            if options.check { "--check" } else { "--json" }
        )));
    }
    if options.copier_defaults && !options.copier {
        return Err(usage("--copier-defaults only goes with --copier".into()));
    }
    if options.remote {
        return Err(usage(
            "patchula apply runs on this machine, since a server cannot write your files; drop --remote".into(),
        ));
    }
    for manager in &options.manager {
        serde_json::from_value::<patchula_config::Manager>(json!(manager)).map_err(|_| {
            (
                2,
                miette::miette!(
                    help = "the managers are listed at https://count-patchula.sweepy.dev/guide/managers/",
                    "--manager {manager} is not a manager the Count knows"
                ),
            )
        })?;
    }
    let names = patchula_config::Patterns::compile(&options.dependency)
        .map_err(|e| (2, miette::miette!("--dependency: {e}")))?;
    let types: Vec<&str> = options
        .update_type
        .iter()
        .map(|kind| match kind {
            cli::UpdateType::Major => "major",
            cli::UpdateType::Minor => "minor",
            cli::UpdateType::Patch => "patch",
            cli::UpdateType::Pin => "pin",
            cli::UpdateType::Digest => "digest",
        })
        .collect();
    let lookup = cli::Lookup {
        repositories: Vec::new(),
        all_repos: false,
        scan: options.scan.clone(),
        config_dir: None,
        remote: false,
        here: true,
    };
    let (report, read) = crate::report(app, &lookup, None, services).await?;
    let crate::Read::Checkout(checkout) = read else {
        return Err((
            1,
            miette::miette!("the lookup did not read the working tree"),
        ));
    };
    let (dir, config) = *checkout;
    let local = tui::local(&dir, config).map_err(|e| (2, miette::miette!("{e}")))?;
    let mut warnings = Vec::new();
    let mut failures = Vec::new();
    for repo in &report.repositories {
        if repo.status == patchula_report::Status::Error {
            return Err((
                1,
                miette::miette!(
                    "{}: {}",
                    repo.repository,
                    repo.reason.as_deref().unwrap_or("the lookup failed")
                ),
            ));
        }
        warnings.extend(repo.reason.iter().chain(&repo.warnings).cloned());
        failures.extend(&repo.lookup_failures);
    }
    let all = tui::updates(&report);
    let mut chosen: Vec<&tui::Update> = all
        .iter()
        .filter(|u| {
            (options.manager.is_empty() || options.manager.contains(&u.manager))
                && names.matches_any(&u.name, &u.package)
                && (types.is_empty() || types.contains(&u.kind.as_str()))
        })
        .collect();
    let filtered = all.len() - chosen.len();
    let notices: Vec<&patchula_report::Notice> = report
        .repositories
        .iter()
        .flat_map(|r| &r.notices)
        .filter(|n| {
            n.is_template()
                && (options.manager.is_empty() || options.manager.contains(&n.manager))
                && names.matches_any(&n.name, &n.package)
                && types.is_empty()
        })
        .collect();
    // Copier needs a clean tree, so it runs first, and the other edits are
    // planned against what it left.
    let (templates, moved) = if options.copier {
        templates(
            &notices,
            &dir,
            "copier",
            options.copier_defaults,
            io::stdin().is_terminal(),
        )
    } else {
        (String::new(), true)
    };
    chosen.sort_by_key(|u| {
        RANK.iter()
            .position(|k| *k == u.kind)
            .unwrap_or_else(|| RANK.len())
    });
    let mut declarations = BTreeSet::new();
    chosen.retain(|u| declarations.insert((u.file.clone(), u.name.clone(), u.starts.clone())));
    chosen.sort_by(|a, b| (&a.file, &a.name).cmp(&(&b.file, &b.name)));
    let plan = tui::plan(&local, &chosen).map_err(|e| (1, miette::miette!("{e}")))?;
    let writes = !options.dry_run && !options.check;
    if writes {
        plan.write(&dir).map_err(|e| (1, miette::miette!("{e}")))?;
    }
    let waiting = if writes {
        !plan.skipped.is_empty()
    } else {
        !chosen.is_empty()
    };
    let code = if !failures.is_empty() {
        crate::LOOKUP_FAILED
    } else if waiting || !moved {
        WAITING
    } else {
        0
    };
    let applied: Vec<&tui::Update> = plan.applied.iter().map(|&i| chosen[i]).collect();
    let skipped: Vec<(&tui::Update, &str)> = plan
        .skipped
        .iter()
        .map(|(i, reason)| (chosen[*i], reason.as_str()))
        .collect();
    let diffs: Vec<(&String, String)> = plan
        .files
        .iter()
        .map(|(path, (old, new))| (path, patchula_update::unified(path, old, new)))
        .collect();
    let fail = |e: io::Error| (1, miette::miette!("{e}"));
    if app.json {
        let update = |u: &tui::Update| {
            json!({
                "manager": u.manager, "file": u.file, "name": u.name, "package": u.package,
                "current": u.current, "type": u.kind,
                "value": u.value, "version": u.version, "digest": u.digest,
            })
        };
        let result = json!({
            "schemaVersion": 1,
            "written": writes,
            "updates": applied.iter().map(|u| update(u)).collect::<Vec<_>>(),
            "skipped": skipped.iter().map(|(u, reason)| {
                let mut value = update(u);
                value["reason"] = json!(reason);
                value
            }).collect::<Vec<_>>(),
            "files": diffs.iter().map(|(path, diff)| json!({"path": path, "diff": diff})).collect::<Vec<Value>>(),
            "filtered": filtered,
            "notices": notices,
            "warnings": warnings,
            "lookupFailures": failures,
            "exitCode": code,
        });
        writeln!(out, "{result}").map_err(fail)?;
        return Ok(code);
    }
    if options.dry_run {
        for (_, diff) in &diffs {
            write!(out, "{diff}").map_err(fail)?;
        }
    }
    let mut text = summary(
        style,
        options,
        &applied,
        &skipped,
        diffs.len(),
        !warnings.is_empty(),
        failures.len(),
    );
    if options.copier {
        if !templates.is_empty() {
            style.heading(&mut text, 0, "Copier", notices.len(), Paint::Accent);
            for line in templates.lines() {
                style.wrap(&mut text, 2, line, Paint::Plain);
            }
        }
    } else {
        crate::output::notices(&mut text, style, 0, "Template updates", &notices);
    }
    crate::output::failures(&mut text, style, 0, &failures);
    crate::output::warnings(&mut text, style, 0, &warnings);
    let mut notes = Vec::new();
    if !options.copier && !notices.is_empty() {
        notes.push(
            "Only copier applies template updates; patchula apply --copier runs it for them."
                .to_owned(),
        );
    }
    if filtered > 0 {
        notes.push(format!(
            "{} stayed out of it, by your filters.",
            count(filtered, "more update", "more updates")
        ));
    }
    if !failures.is_empty() {
        notes.push(format!(
            "{} failed, so those dependencies may have updates the Count cannot see.",
            count(failures.len(), "lookup", "lookups")
        ));
    }
    if applied.iter().any(|u| u.locked) {
        notes.push(
            "Lockfiles are not regenerated, so refresh them with your package manager.".into(),
        );
    }
    if !notes.is_empty() {
        text.push('\n');
        for note in notes {
            style.wrap(&mut text, 0, &note, Paint::Dim);
        }
    }
    write!(err, "{text}").map_err(fail)?;
    Ok(code)
}

/// Run `program update` for each template in turn, and say what each did.
/// Returns the summary and whether every template moved.
fn templates(
    notices: &[&patchula_report::Notice],
    dir: &Path,
    program: &str,
    defaults: bool,
    terminal: bool,
) -> (String, bool) {
    let mut out = String::new();
    let mut moved = true;
    for notice in notices {
        let result = if terminal || defaults {
            crate::copier::update(program, dir, &notice.path, &notice.available, defaults)
        } else {
            Err("copier needs a terminal to ask the template's questions; run it in one, or pass --copier-defaults".into())
        };
        match result {
            Ok(outcome) => {
                out.push_str(&crate::copier::summary(
                    &notice.name,
                    &notice.available,
                    &outcome,
                ));
                out.push('\n');
            }
            Err(error) => {
                moved = false;
                out.push_str(&format!(
                    "Copier did not update {} ({}): {error}\n",
                    notice.name, notice.path
                ));
            }
        }
    }
    (out, moved)
}

/// What the Count did, or would do, in his own words: the headline with its
/// counts, then the updates applied and those left alone.
fn summary(
    style: &Style,
    options: &cli::Apply,
    applied: &[&tui::Update],
    skipped: &[(&tui::Update, &str)],
    files: usize,
    warned: bool,
    failed: usize,
) -> String {
    let updates = |n: usize| count(n, "update", "updates");
    let row = |u: &tui::Update| {
        vec![
            (u.kind.clone(), kind_paint(&u.kind)),
            (u.name.clone(), Paint::Bold),
            (format!("{} → {}", u.current, u.target()), Paint::Plain),
            (u.file.clone(), Paint::Dim),
        ]
    };
    let writes = !options.check && !options.dry_run;
    let headline = if applied.is_empty() && skipped.is_empty() && failed > 0 {
        "The Count found nothing to apply, but only because some lookups failed.".to_owned()
    } else if applied.is_empty() && skipped.is_empty() && warned {
        "The Count found nothing to apply, but read the warnings first: a failed lookup finds nothing either.".to_owned()
    } else if applied.is_empty() && skipped.is_empty() {
        "The Count found nothing to apply. Rest easy.".to_owned()
    } else if applied.is_empty() {
        format!(
            "The Count {} {} in place.",
            if writes {
                "could not write"
            } else {
                "would not write"
            },
            updates(skipped.len())
        )
    } else {
        let files = count(files, "file", "files");
        let n = updates(applied.len());
        if options.check {
            let verb = if applied.len() == 1 {
                "awaits"
            } else {
                "await"
            };
            format!("{n} {verb} in {files}. The Count wrote nothing.")
        } else if options.dry_run {
            format!("The Count would apply {n} to {files}, but wrote nothing.")
        } else {
            format!("The Count applied {n} to {files}.")
        }
    };
    let mut out = String::new();
    style.wrap(
        &mut out,
        0,
        &format!("{}{headline}", style.bat()),
        Paint::Bold,
    );
    let rows: Vec<_> = applied.iter().map(|u| row(u)).collect();
    style.table(&mut out, 2, &rows, 3);
    if !skipped.is_empty() {
        style.heading(&mut out, 0, "Left alone", skipped.len(), Paint::Warning);
        let rows: Vec<_> = skipped.iter().map(|(u, _)| row(u)).collect();
        for (line, (_, reason)) in style.lines(2, &rows, 3).iter().zip(skipped) {
            out.push_str(line);
            out.push('\n');
            style.wrap(&mut out, 4, reason, Paint::Dim);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn notice(path: &str, name: &str) -> patchula_report::Notice {
        serde_json::from_value(json!({
            "manager": "copier", "path": path, "name": name, "package": name,
            "current": "v1.0.0", "available": "v2.0.0"
        }))
        .unwrap()
    }

    #[test]
    fn templates_run_in_turn_and_need_a_terminal_or_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(tree.join("b")).unwrap();
        std::fs::write(tree.join(".copier-answers.yml"), "_commit: v1.0.0\n").unwrap();
        std::fs::write(tree.join("b/.copier-answers.yml"), "_commit: v1.0.0\n").unwrap();
        for args in [
            &["init", "-q"][..],
            &["add", "."],
            &["commit", "-qm", "init"],
        ] {
            let status = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
                .args(args)
                .current_dir(&tree)
                .status()
                .unwrap();
            assert!(status.success());
        }
        let fake = dir.path().join("copier");
        std::fs::write(
            &fake,
            "#!/bin/sh\necho \"$*\" >> ../args\nprintf '_commit: v2.0.0\\n' > .copier-answers.yml\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let fake = fake.to_string_lossy().into_owned();
        let notices = [
            notice(".copier-answers.yml", "gh:a/one"),
            notice("b/.copier-answers.yml", "gh:a/two"),
        ];
        let notices: Vec<_> = notices.iter().collect();

        let (text, moved) = templates(&notices, &tree, &fake, false, false);
        assert!(!moved);
        assert!(
            text.contains(
                "Copier did not update gh:a/one (.copier-answers.yml): copier needs a terminal"
            ),
            "{text}"
        );
        assert!(!dir.path().join("args").exists());

        let (text, moved) = templates(&notices, &tree, &fake, true, false);
        assert!(!moved);
        assert!(
            text.contains("Copier moved gh:a/one to v2.0.0: 1 file changed (.copier-answers.yml)."),
            "{text}"
        );
        assert!(text.contains("Copier did not update gh:a/two (b/.copier-answers.yml): the working tree has uncommitted changes"), "{text}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("args")).unwrap(),
            "update --defaults --vcs-ref v2.0.0 --skip-answered --answers-file .copier-answers.yml .\n"
        );
    }
}
