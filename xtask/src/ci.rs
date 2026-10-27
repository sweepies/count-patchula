//! The local CI run.

use crate::{Args, Result};
use std::{fs::File, path::PathBuf, process::Command};

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
