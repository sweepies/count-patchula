//! Repeated runs of the offline suite, to shake out order and timing flakes.

use crate::{Args, Result};
use std::{fs::File, path::PathBuf, process::Command};

/// Repeats the workspace tests with 1, 2 and 16 test threads per iteration,
/// and stops at the first failing run with its log.
pub fn stress(mut args: Args) -> Result {
    let runs: u32 = args.value("--runs")?.map_or(Ok(12), |r| r.parse())?;
    let filter = args.value("--filter")?.unwrap_or_default();
    let log_dir = args.value("--log-dir")?.map_or_else(
        || std::env::temp_dir().join("patchula-stress"),
        PathBuf::from,
    );
    args.positional(0, 0)?;
    std::fs::create_dir_all(&log_dir)?;
    for iteration in 1..=runs {
        for threads in [1, 2, 16] {
            let log = log_dir.join(format!("{iteration}-{threads}.log"));
            let file = File::create(&log)?;
            let mut cargo = Command::new("cargo");
            cargo
                .args(["test", "--workspace", "--locked", "--"])
                .arg(format!("--test-threads={threads}"));
            if !filter.is_empty() {
                cargo.arg(&filter);
            }
            let status = cargo.stdout(file.try_clone()?).stderr(file).status()?;
            if !status.success() {
                print!("{}", String::from_utf8_lossy(&std::fs::read(&log)?));
                return Err(format!("iteration {iteration} with {threads} threads failed").into());
            }
            println!("passed iteration={iteration} threads={threads}");
        }
    }
    Ok(())
}
