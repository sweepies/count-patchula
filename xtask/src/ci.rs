//! The local CI run, and the path filter that decides which GitHub jobs run.

use crate::{Args, Result, output};
use std::{fs::File, io::Write, path::PathBuf, process::Command};

/// The tasks GitHub runs as separate jobs, in the order a local run takes them.
const STAGES: &[&str] = &[
    "lint",
    "build",
    "fixtures:check",
    "test:unit",
    "test:tools",
    "docs:build",
];

/// Runs every stage, keeping each one's log, and stops at the first failure
/// with one line naming the stage, its failing tests and its log.
pub fn all(mut args: Args) -> Result {
    let log_dir = match args.value("--log-dir")? {
        Some(dir) => PathBuf::from(dir),
        None => std::env::temp_dir().join(format!("patchula-ci.{}", std::process::id())),
    };
    args.positional(0, 0)?;
    std::fs::create_dir_all(&log_dir)?;
    println!("CI full logs: {}", log_dir.display());
    for stage in STAGES {
        let log = log_dir.join(format!("{}.log", stage.replace(':', "-")));
        let file = File::create(&log)?;
        let status = Command::new("mise")
            .args(["run", stage])
            .stdout(file.try_clone()?)
            .stderr(file)
            .status()?;
        let text = String::from_utf8_lossy(&std::fs::read(&log)?).into_owned();
        print!("{text}");
        if !status.success() {
            let failed: String = text
                .lines()
                .filter_map(|line| line.strip_prefix("test ")?.strip_suffix(" ... FAILED"))
                .map(|name| format!("{name},"))
                .collect();
            let failed = if failed.is_empty() {
                "see-log"
            } else {
                &failed
            };
            // Keep the path and test names even when a caller filters for test result:.
            println!(
                "test result: FAILED; stage={stage}; failing-tests={failed}; full-log={}",
                log.display()
            );
            return Err(format!("{stage} failed").into());
        }
    }
    Ok(())
}

#[derive(Debug, Default, PartialEq)]
struct Jobs {
    rust: bool,
    actions: bool,
    docs: bool,
}

/// Reads GH_TOKEN, REPO, EVENT, PR, BEFORE and AFTER, and writes rust, actions
/// and docs to $GITHUB_OUTPUT. GitHub Actions only.
pub fn changes(args: Args) -> Result {
    args.positional(0, 0)?;
    let env = |name: &str| std::env::var(name).map_err(|_| format!("{name} is not set"));
    let repo = env("REPO")?;
    let before = env("BEFORE")?;
    let files = if env("EVENT")? == "pull_request" {
        let pr = env("PR")?;
        output(Command::new("gh").args([
            "api",
            &format!("repos/{repo}/pulls/{pr}/files"),
            "--paginate",
            "--jq",
            ".[].filename",
        ]))?
    } else if before == "0".repeat(40) {
        "unknown".into()
    } else {
        // The compare API lists at most 300 files; assume more means everything.
        output(Command::new("gh").args([
            "api",
            &format!("repos/{repo}/compare/{before}...{}", env("AFTER")?),
            "--jq",
            r#"if (.files | length) >= 300 then "unknown" else .files[].filename end"#,
        ]))?
    };
    let jobs = classify(files.lines());
    let mut outputs = File::options().append(true).open(env("GITHUB_OUTPUT")?)?;
    writeln!(
        outputs,
        "rust={}\nactions={}\ndocs={}",
        jobs.rust, jobs.actions, jobs.docs
    )?;
    let mut summary = File::options()
        .append(true)
        .open(env("GITHUB_STEP_SUMMARY")?)?;
    writeln!(
        summary,
        "Rust jobs: {}\nWorkflow lint: {}\nDocs build: {}",
        jobs.rust, jobs.actions, jobs.docs
    )?;
    Ok(())
}

/// The jobs a list of changed paths needs. An empty list is unexpected, so it
/// runs everything.
fn classify<'a>(files: impl IntoIterator<Item = &'a str>) -> Jobs {
    const IGNORED_DIRS: &[&str] = &["assets/", "skills/", ".claude/"];
    const IGNORED: &[&str] = &[
        "conformance/README.md",
        "README.md",
        "AGENTS.md",
        "CLAUDE.md",
        "LICENSE",
        ".gitignore",
        ".deepsource.toml",
        "socket.yml",
        "communique.toml",
        "tak.toml",
    ];
    let mut jobs = Jobs::default();
    let mut files = files.into_iter().peekable();
    if files.peek().is_none() {
        return classify(["unknown"]);
    }
    for file in files {
        match file {
            "unknown" | "mise.toml" | "mise.lock" | ".github/workflows/ci.yml" => {
                jobs = Jobs {
                    rust: true,
                    actions: true,
                    docs: true,
                }
            }
            // The conformance tests read the departures page.
            "docs/src/content/docs/renovate/departures.md" => {
                jobs.rust = true;
                jobs.docs = true;
            }
            _ if file.starts_with("docs/") => jobs.docs = true,
            _ if IGNORED.contains(&file) || IGNORED_DIRS.iter().any(|d| file.starts_with(d)) => {}
            _ if file == "action.yml" || file.starts_with(".github/workflows/") => {
                jobs.actions = true
            }
            // Anything else may change what the Rust jobs check.
            _ => {
                jobs.rust = true;
                jobs.actions = true;
            }
        }
    }
    jobs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jobs(rust: bool, actions: bool, docs: bool) -> Jobs {
        Jobs {
            rust,
            actions,
            docs,
        }
    }

    #[test]
    fn paths_choose_the_jobs_they_can_affect() {
        assert_eq!(
            classify(["docs/src/content/docs/guide/x.md"]),
            jobs(false, false, true)
        );
        assert_eq!(
            classify(["README.md", "skills/a/SKILL.md"]),
            jobs(false, false, false)
        );
        assert_eq!(
            classify([".github/workflows/release.yml"]),
            jobs(false, true, false)
        );
        assert_eq!(classify(["action.yml"]), jobs(false, true, false));
        assert_eq!(
            classify(["crates/cli/src/main.rs"]),
            jobs(true, true, false)
        );
        assert_eq!(classify(["xtask/src/ci.rs"]), jobs(true, true, false));
        assert_eq!(
            classify(["docs/src/content/docs/renovate/departures.md"]),
            jobs(true, false, true)
        );
        assert_eq!(classify(["README.md", "mise.toml"]), jobs(true, true, true));
        assert_eq!(classify(["unknown"]), jobs(true, true, true));
    }

    #[test]
    fn an_empty_list_runs_everything() {
        assert_eq!(classify("".lines()), jobs(true, true, true));
    }
}
