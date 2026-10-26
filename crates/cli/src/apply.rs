//! `patchula apply`: look up the working tree here and write its updates into
//! the files, through the same engine as the TUI's apply.
use crate::{Services, cli, tui};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::{self, Write},
};

/// When several updates propose a change to one declaration, the earliest
/// type here is the one applied.
const RANK: [&str; 5] = ["major", "minor", "patch", "pin", "digest"];
/// Some selected updates are still waiting after the command.
const WAITING: u8 = 3;

/// Diffs go to `out` and the summary to `err`, so a dry run's output is a patch.
pub async fn command(
    app: &cli::App,
    options: &cli::Apply,
    services: &Services,
    out: &mut dyn Write,
    err: &mut dyn Write,
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
    let (report, checkout) = crate::report(app, &lookup, None, services).await?;
    let (dir, config) = checkout.ok_or_else(|| {
        (
            1,
            miette::miette!("the lookup did not read the working tree"),
        )
    })?;
    let local = tui::local(&dir, config).map_err(|e| (2, miette::miette!("{e}")))?;
    let mut warnings = Vec::new();
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
    chosen.sort_by_key(|u| RANK.iter().position(|k| *k == u.kind).unwrap_or(RANK.len()));
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
    let code = if waiting { WAITING } else { 0 };
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
            "warnings": warnings,
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
    for warning in &warnings {
        writeln!(err, "warning: {warning}").map_err(fail)?;
    }
    write!(
        err,
        "{}",
        summary(
            options,
            &applied,
            &skipped,
            diffs.len(),
            filtered,
            !warnings.is_empty()
        )
    )
    .map_err(fail)?;
    Ok(code)
}

/// What the Count did, or would do, in his own words.
fn summary(
    options: &cli::Apply,
    applied: &[&tui::Update],
    skipped: &[(&tui::Update, &str)],
    files: usize,
    filtered: usize,
    warned: bool,
) -> String {
    let line = |u: &tui::Update| {
        format!(
            "  {:<6} {} {} -> {}  ({})\n",
            u.kind,
            u.name,
            u.current,
            u.target(),
            u.file
        )
    };
    let count = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
    let mut out = String::new();
    if applied.is_empty() && skipped.is_empty() {
        out.push_str(if warned {
            "The Count found nothing to apply, but read the warnings first: a failed lookup finds nothing either.\n"
        } else {
            "The Count found nothing to apply. Rest easy.\n"
        });
    } else if !applied.is_empty() {
        let updates = count(applied.len(), "update");
        let files = count(files, "file");
        out.push_str(&if options.check {
            format!("{updates} await in {files}. The Count wrote nothing:\n")
        } else if options.dry_run {
            format!("The Count would apply {updates} to {files}, but wrote nothing:\n")
        } else {
            format!("The Count applied {updates} to {files}:\n")
        });
        for u in applied {
            out.push_str(&line(u));
        }
    }
    if !skipped.is_empty() {
        let what = count(skipped.len(), "update");
        out.push_str(&if options.check || options.dry_run {
            format!("He would leave {what} alone:\n")
        } else {
            format!("He left {what} alone:\n")
        });
        for (u, reason) in skipped {
            out.push_str(&line(u));
            out.push_str(&format!("         {reason}\n"));
        }
    }
    if filtered > 0 {
        out.push_str(&format!(
            "{} stayed out of it, by your filters.\n",
            count(filtered, "more update")
        ));
    }
    if applied.iter().any(|u| u.locked) {
        out.push_str("Lockfiles are not regenerated, so refresh them with your package manager.\n");
    }
    out
}
