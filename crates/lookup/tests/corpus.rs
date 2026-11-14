//! The proposal counterpart of the extraction snapshot: over the same corpus
//! in tests/corpus, one line per dependency (and one per `Locked` lockfile
//! entry) records what the evaluator's policy decides for it. No cassettes
//! and no recorded catalogs: the rules being pinned react to a handful of
//! catalog shapes, so each dependency's catalog is a synthetic ladder built
//! from its own declaration. A pull request's diff shows exactly which real
//! declarations a proposal-rule change touches. Regenerate with
//! `mise run corpus:snapshot`.
use patchula_datasource::{Catalog, Version};
use patchula_lockfiles::{Job, Scope};
use patchula_lookup::{Evaluator, advisory_query};
use patchula_manager::{Dependency, Files, Scanner, hash};
use patchula_report::Update as Proposal;
use patchula_words::{Source, VersionPolicy};
use std::collections::BTreeSet;
use std::path::Path;

/// The instant every evaluation sees, and the release date every rung
/// carries, so waits never gate and output never drifts.
const NOW: &str = "2026-01-01T00:00:00Z";
const PUBLISHED: &str = "2020-01-01T00:00:00Z";
/// A suffix no declaration's own suffix equals, standing in for the variants
/// Docker and Maven keep releases inside.
const FOREIGN: &str = "-foreign";

fn walk(root: &Path, dir: &Path, scanner: &Scanner, files: &mut Files, all: &mut Vec<String>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap())
        .collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, scanner, files, all);
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if scanner.needs(&relative)
            && let Ok(text) = std::fs::read_to_string(&path)
        {
            files.insert(relative.clone(), text.into());
        }
        all.push(relative);
    }
}

/// The `v` prefix, numeric fields and trailing `-suffix` of the first numeric
/// core in `text`, so `^1.0` gives the same ladder as `1.0` would.
fn numeric_core(text: &str) -> Option<(bool, Vec<u64>, String)> {
    let bytes = text.as_bytes();
    let start = bytes.iter().position(u8::is_ascii_digit)?;
    let v = start > 0
        && bytes[start - 1] == b'v'
        && (start < 2 || !bytes[start - 2].is_ascii_alphanumeric());
    let mut end = start;
    while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
        end += 1;
    }
    let fields: Option<Vec<u64>> = text[start..end]
        .split('.')
        .map(|f| f.parse().ok())
        .collect();
    let fields = fields?;
    let suffix_start = end;
    if end < bytes.len() && bytes[end] == b'-' {
        end += 1;
        while end < bytes.len()
            && (bytes[end].is_ascii_alphanumeric()
                || matches!(bytes[end], b'.' | b'-' | b'_' | b'+'))
        {
            end += 1;
        }
    }
    Some((v, fields, text[suffix_start..end].to_owned()))
}

/// `prefix`'s release line: `v4` covers every `4.x.y`, `3.22` every `3.22.x`.
fn numeric_prefix(prefix: &str, version: &str) -> bool {
    let prefix = prefix.trim_start_matches('v');
    let version = version.trim_start_matches('v');
    version == prefix
        || version
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// One catalog entry's shape before it is rendered.
struct Rung {
    fields: Vec<u64>,
    /// `-rc.1` makes a rung a prerelease; it goes before the suffix.
    pre: bool,
    yanked: bool,
}

/// The synthetic catalog a declaration answers to: the current release and
/// the rungs a real registry would list around it, so every manager-naming
/// rule sees the shapes it branches on.
fn ladder(dep: &Dependency) -> Catalog {
    if dep.source == Source::Nix {
        // A flake catalog is one entry: the revision the ref resolves to.
        return Catalog {
            versions: vec![Version {
                version: dep.current.clone(),
                published_at: Some(PUBLISHED.into()),
                digest: Some(hash(format!("{}:next-revision", dep.package).as_bytes())),
                deprecated: false,
                yanked: false,
            }],
            dist_tags: Default::default(),
        };
    }
    let text = dep.resolved.as_deref().unwrap_or(&dep.current);
    // A ref with no numeric core (`latest`, a branch, a bare SHA) still gets a
    // ladder: its registry would list real versions for the dist-tag and
    // opaque-ref lanes to react to.
    let (v, fields, suffix) = numeric_core(text).unwrap_or((false, vec![0, 0, 0], String::new()));
    let foreign = if suffix == FOREIGN { "-other" } else { FOREIGN };
    let declared = fields.len();
    let mut precisions = vec![declared];
    if declared < 3 {
        precisions.push(3);
    }
    let mut rungs: Vec<Rung> = Vec::new();
    for &precision in &precisions {
        let base = |extra: usize| {
            let mut at = fields[..declared.min(precision)].to_vec();
            at.resize(precision + extra, 0);
            at
        };
        rungs.push(Rung {
            fields: base(0),
            pre: false,
            yanked: false,
        });
        for position in 0..precision {
            let mut bumped = base(0);
            bumped[position] += 1;
            for field in &mut bumped[position + 1..] {
                *field = 0;
            }
            rungs.push(Rung {
                fields: bumped,
                pre: false,
                yanked: false,
            });
        }
        // A prerelease above every release rung.
        let mut pre = base(0);
        pre[0] += 2;
        for field in &mut pre[1..] {
            *field = 0;
        }
        rungs.push(Rung {
            fields: pre,
            pre: true,
            yanked: false,
        });
    }
    // One yanked release on the patch line.
    let mut yanked = fields[..fields.len().min(3)].to_vec();
    yanked.resize(3, 0);
    *yanked.last_mut().unwrap() += 2;
    rungs.push(Rung {
        fields: yanked,
        pre: false,
        yanked: true,
    });
    let render = |rung: &Rung, suffix: &str| {
        format!(
            "{}{}{}{}",
            if v { "v" } else { "" },
            rung.fields
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join("."),
            if rung.pre { "-rc.1" } else { "" },
            suffix
        )
    };
    let prefix = format!(
        "{}{}",
        if v { "v" } else { "" },
        fields
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(".")
    );
    // The newest rung on the current line is the release `dep.digest` names,
    // when a declaration pins one.
    let on_line =
        |rung: &Rung| !rung.pre && !rung.yanked && numeric_prefix(&prefix, &render(rung, ""));
    let current_line = rungs
        .iter()
        .filter(|r| on_line(r))
        .map(|r| {
            let mut padded = r.fields.clone();
            padded.resize(3, 0);
            padded
        })
        .max()
        .unwrap_or_default();
    let mut versions: Vec<Version> = Vec::new();
    for rung in &rungs {
        for shape in [suffix.as_str(), foreign] {
            let version = render(rung, shape);
            let mut padded = rung.fields.clone();
            padded.resize(3, 0);
            let digest = if dep.digest.is_some()
                && shape == suffix.as_str()
                && on_line(rung)
                && padded == current_line
            {
                dep.digest.clone().unwrap()
            } else {
                hash(version.as_bytes())
            };
            versions.push(Version {
                version,
                published_at: Some(PUBLISHED.into()),
                digest: Some(digest),
                deprecated: false,
                yanked: rung.yanked,
            });
        }
    }
    let mut catalog = Catalog {
        versions,
        dist_tags: Default::default(),
    };
    let newest = |major: u64| {
        rungs
            .iter()
            .rfind(|r| !r.pre && !r.yanked && r.fields[..1] == [major])
    };
    for (tag, rung) in [
        ("latest", newest(fields[0] + 1)),
        ("lts", newest(fields[0])),
    ] {
        if let Some(rung) = rung {
            catalog.dist_tags.insert(tag.into(), render(rung, &suffix));
        }
    }
    // Rungs a dependency's `extract` pattern needs by hand, when none of the
    // ladder's shapes carry a `version` it can capture.
    catalog
        .versions
        .extend(hand_rungs(dep).iter().map(|version| Version {
            version: (*version).into(),
            published_at: Some(PUBLISHED.into()),
            digest: Some(hash(version.as_bytes())),
            deprecated: false,
            yanked: false,
        }));
    catalog
}

/// Catalog entries a declaration's `extract` pattern accepts but the numeric
/// ladder never writes, keyed by the dependency that needs them.
fn hand_rungs(dep: &Dependency) -> &'static [&'static str] {
    let _ = dep;
    &[]
}

/// What one evaluation decided, for one TSV cell each.
struct Row {
    proposals: String,
    not_proposed: String,
    error: String,
    jobs: String,
}

fn proposal_text(
    eval: &Evaluator,
    dep: &Dependency,
    update: &Proposal,
    jobs: &mut Vec<String>,
    paths: &BTreeSet<String>,
) -> String {
    let mut text = format!(
        "{}:{}→{}",
        update.update_type,
        update.new_value.as_deref().unwrap_or("-"),
        update.new_version.as_deref().unwrap_or("-"),
    );
    if let Some(digest) = &update.new_digest {
        text.push('@');
        text.push_str(&digest[..digest.len().min(16)]);
    }
    if eval.needs_digest(dep, update) {
        text.push_str("[needs-digest]");
    }
    jobs.push(match patchula_lockfiles::job(dep, update, paths) {
        Ok(None) => "-".into(),
        Ok(Some(Job {
            ecosystem,
            directory,
            scope,
            exact_version,
            publish_age_allow: _,
            wait_hours: _,
        })) => {
            let scope = match &scope {
                Scope::Directory => "directory".into(),
                Scope::Package {
                    package,
                    current,
                    target,
                } => format!(
                    "{}:{}→{}",
                    package,
                    current.as_deref().unwrap_or("-"),
                    target.as_deref().unwrap_or("-")
                ),
            };
            format!(
                "{ecosystem:?}:{directory}:{scope}{}",
                if exact_version { "=lock" } else { "" }
            )
        }
        Err(error) => format!("err:{error}"),
    });
    text
}

fn evaluate(
    eval: &Evaluator,
    dep: &Dependency,
    catalog: &Catalog,
    now: chrono::DateTime<chrono::Utc>,
    paths: &BTreeSet<String>,
) -> Row {
    let evaluation = match eval.evaluate_at(dep, catalog, now) {
        Err(error) => {
            return Row {
                proposals: "-".into(),
                not_proposed: "-".into(),
                error,
                jobs: "-".into(),
            };
        }
        Ok(evaluation) => evaluation,
    };
    let mut jobs = Vec::new();
    let proposals: Vec<String> = evaluation
        .updates
        .iter()
        .map(|update| proposal_text(eval, dep, update, &mut jobs, paths))
        .collect();
    Row {
        proposals: if proposals.is_empty() {
            "-".into()
        } else {
            proposals.join(",")
        },
        not_proposed: if evaluation.not_proposed.is_empty() {
            "-".into()
        } else {
            evaluation
                .not_proposed
                .iter()
                .map(|m| format!("{}:{}", m.version, m.reason))
                .collect::<Vec<_>>()
                .join(",")
        },
        error: "-".into(),
        jobs: if jobs.is_empty() {
            "-".into()
        } else {
            jobs.join(",")
        },
    }
}

fn line(
    eval: &Evaluator,
    exact: &Evaluator,
    dep: &Dependency,
    catalog: &Catalog,
    now: chrono::DateTime<chrono::Utc>,
    paths: &BTreeSet<String>,
) -> String {
    let row = evaluate(eval, dep, catalog, now, paths);
    let under_exact = evaluate(exact, dep, catalog, now, paths);
    let differing = [
        under_exact.proposals.as_str(),
        under_exact.not_proposed.as_str(),
        under_exact.error.as_str(),
    ]
    .join("|");
    let same = [
        row.proposals.as_str(),
        row.not_proposed.as_str(),
        row.error.as_str(),
    ]
    .join("|");
    vec![
        serde_json::to_value(dep.manager)
            .unwrap()
            .as_str()
            .unwrap()
            .into(),
        dep.file.clone(),
        dep.name.clone(),
        dep.current.clone(),
        dep.resolved.clone().unwrap_or_else(|| "-".into()),
        bool_text(eval.needs_lookup(dep)),
        bool_text(eval.needs_forge_catalog(dep)),
        bool_text(eval.digest_policy_applies(dep)),
        match eval.disabled_updates(dep) {
            Ok(disabled) if disabled.is_empty() => "-".into(),
            Ok(disabled) => disabled
                .iter()
                .map(|u| u.as_str())
                .collect::<Vec<_>>()
                .join(","),
            Err(error) => format!("err:{error}"),
        },
        advisory_query(dep)
            .map(|q| q.ecosystem)
            .unwrap_or_else(|| "-".into()),
        match eval.resolved_current_version(dep, catalog) {
            Ok(Some(version)) => version,
            Ok(None) => "-".into(),
            Err(error) => format!("err:{error}"),
        },
        row.proposals,
        if differing == same {
            "-".into()
        } else {
            differing
        },
        row.not_proposed,
        row.error,
        match eval.digest_proposal(dep, &hash(b"corpus-probe-digest")) {
            Ok(Some(digest)) => format!(
                "{}:{}→{}@{}",
                digest.update_type,
                digest.new_value.as_deref().unwrap_or("-"),
                digest.new_version.as_deref().unwrap_or("-"),
                &digest.new_digest.as_deref().unwrap_or("-")[..16]
            ),
            Ok(None) => "-".into(),
            Err(error) => format!("err:{error}"),
        },
        row.jobs,
    ]
    .join("\t")
}

fn bool_text(value: bool) -> String {
    if value { "t".into() } else { "f".into() }
}

#[test]
fn corpus_proposals_match_their_snapshot() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus");
    let config = patchula_config::load(&root).unwrap();
    let scanner = Scanner::new(&config).unwrap();
    let mut files = Files::new();
    let mut all = Vec::new();
    walk(&root, &root, &scanner, &mut files, &mut all);
    loop {
        let more = scanner.more(&files, all.iter().map(String::as_str));
        if more.is_empty() {
            break;
        }
        for path in more {
            let text = std::fs::read_to_string(root.join(&path)).unwrap();
            files.insert(path, text.into());
        }
    }
    let extraction = scanner.extract(&files).unwrap();
    let now = chrono::DateTime::parse_from_rfc3339(NOW)
        .unwrap()
        .with_timezone(&chrono::Utc);
    let eval = Evaluator::new(&config, Default::default()).unwrap();
    let mut config_exact = config.clone();
    config_exact.rules.push(patchula_config::Rule {
        note: "corpus proposals: exact versions".into(),
        select: patchula_config::Selector {
            packages: Vec::new(),
            dependencies: Vec::new(),
            managers: Vec::new(),
            files: Vec::new(),
            updates: Vec::new(),
            sources: Vec::new(),
            kinds: Vec::new(),
            current: None,
        },
        apply: patchula_config::Overrides {
            active: None,
            versions: Some(VersionPolicy::Exact),
            channel: None,
            digest: None,
            merge: None,
            group: None,
            labels: None,
            commit_prefix: None,
            release_notes: None,
            body_notes: None,
            permitted: None,
            wait_hours: None,
            windows: None,
        },
    });
    let exact = Evaluator::new(&config_exact, Default::default()).unwrap();
    let paths: BTreeSet<String> = all.iter().cloned().collect();
    let render = || {
        let mut holes = Vec::new();
        let mut lines = Vec::new();
        for dep in extraction
            .dependencies
            .iter()
            .cloned()
            .chain(extraction.locked.iter().map(|l| l.dependency()))
        {
            let catalog = ladder(&dep);
            if let Some(pattern) = dep
                .extract
                .as_ref()
                .map(|pattern| regex::Regex::new(pattern).unwrap())
                && catalog.versions.iter().all(|v| {
                    pattern
                        .captures(&v.version)
                        .and_then(|c| c.name("version"))
                        .is_none()
                })
            {
                holes.push(format!(
                    "{}\t{}\t{}",
                    dep.file,
                    dep.name,
                    dep.extract.as_deref().unwrap_or_default()
                ));
            }
            lines.push(line(&eval, &exact, &dep, &catalog, now, &paths));
        }
        assert_eq!(
            holes.len(),
            0,
            "dependencies whose extract rejects every catalog rung (add hand rungs): {holes:#?}"
        );
        lines.sort();
        lines.join("\n") + "\n"
    };
    let actual = render();
    assert_eq!(
        actual,
        render(),
        "the snapshot render must be deterministic"
    );
    let snapshot = root.join("../corpus-proposals.tsv");
    if std::env::var_os("PATCHULA_UPDATE_SNAPSHOT").is_some() {
        std::fs::write(&snapshot, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&snapshot).unwrap_or_default();
    if actual != expected {
        let missing: Vec<_> = expected
            .lines()
            .filter(|l| !actual.contains(*l))
            .take(10)
            .collect();
        let extra: Vec<_> = actual
            .lines()
            .filter(|l| !expected.contains(*l))
            .take(10)
            .collect();
        panic!(
            "corpus proposals changed; run `mise run corpus:snapshot` if intended.\nmissing: {missing:#?}\nextra: {extra:#?}"
        );
    }
}
