//! The corpus lookup CI runs, summarized for the job page.

use crate::{Args, Result, text};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs::File,
    io::Write,
    path::{Path, absolute},
    process::{Command, Stdio},
};

/// Looks up and plans every corpus dependency through `corpus:check`, then
/// appends a Markdown summary; fails when a branch fails to plan.
pub fn report(mut args: Args) -> Result {
    let cache = absolute(args.value("--cache")?.ok_or("--cache is required")?)?;
    let out = absolute(
        args.value("--out")?
            .unwrap_or_else(|| "corpus-report.json".into()),
    )?;
    let summary_path = args.value("--summary")?;
    args.positional(0, 0)?;
    std::fs::create_dir_all(&cache)?;
    let check = Command::new("mise")
        .args(["run", "corpus:check", "--cache"])
        .arg(&cache)
        .stdout(File::create(&out)?)
        .stderr(Stdio::piped())
        .output()?;
    let log = String::from_utf8_lossy(&check.stderr);
    if !check.status.success() {
        // Drop the diff lines a dry run prints, and keep the end of the rest.
        let lines: Vec<_> = log
            .lines()
            .filter(|line| !line.starts_with(['-', '+', '@', ' ']))
            .collect();
        for line in &lines[lines.len().saturating_sub(40)..] {
            eprintln!("{line}");
        }
        return Err(format!("corpus:check failed: {}", check.status).into());
    }
    let report: Value = serde_json::from_slice(&std::fs::read(&out)?)?;
    let summary = summarize(&report, &log);
    match summary_path {
        Some(path) => File::options()
            .create(true)
            .append(true)
            .open(Path::new(&path))?
            .write_all(summary.as_bytes())?,
        None => print!("{summary}"),
    }
    let errors = branches(&report)
        .filter(|branch| branch["status"] == "error")
        .count();
    if errors > 0 {
        println!("::error::{errors} corpus branches failed to plan");
        return Err(format!("{errors} corpus branches failed to plan").into());
    }
    Ok(())
}

fn branches(report: &Value) -> impl Iterator<Item = &Value> {
    report["actions"]["branches"].as_array().into_iter().flatten()
}

fn summarize(report: &Value, log: &str) -> String {
    let each = |value: &Value| value.as_array().cloned().unwrap_or_default();
    let mut lines = vec!["### Corpus".to_owned(), "```".to_owned()];
    lines.extend(log.lines().filter(|l| l.starts_with("patchula")).map(str::to_owned));
    for repository in each(&report["repositories"]) {
        let dependencies: Vec<Value> = each(&repository["packageFiles"])
            .iter()
            .flat_map(|file| each(&file["dependencies"]))
            .collect();
        let updates: usize = dependencies.iter().map(|d| each(&d["updates"]).len()).sum();
        lines.push(format!(
            "{}: {} dependencies, {updates} updates",
            text(&repository["repository"]),
            dependencies.len()
        ));
        for warning in each(&repository["warnings"]) {
            lines.push(format!("  warning: {}", text(&warning)));
        }
    }
    let mut statuses = BTreeMap::<String, usize>::new();
    for branch in branches(report) {
        *statuses.entry(text(&branch["status"])).or_default() += 1;
    }
    for (status, count) in statuses {
        lines.push(format!("{status}: {count} branches"));
    }
    for branch in branches(report).filter(|b| b["status"] == "skipped" || b["status"] == "error") {
        lines.push(format!(
            "{} {}: {}",
            text(&branch["status"]),
            text(&branch["branch"]),
            text(&branch["reason"])
        ));
    }
    lines.push("```".to_owned());
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_summary_counts_dependencies_updates_and_branches() {
        let report = json!({
            "repositories": [{
                "repository": "o/r",
                "packageFiles": [
                    {"dependencies": [{"updates": [{}, {}]}, {"updates": []}]},
                    {"dependencies": [{"updates": [{}]}]},
                ],
                "warnings": ["slow registry"],
            }],
            "actions": {"branches": [
                {"status": "planned", "branch": "a"},
                {"status": "skipped", "branch": "b", "reason": "pending"},
                {"status": "planned", "branch": "c"},
            ]},
        });
        assert_eq!(
            summarize(&report, "noise\npatchula 1.0 looked up 3\n"),
            "### Corpus\n```\npatchula 1.0 looked up 3\no/r: 3 dependencies, 3 updates\n  warning: slow registry\nplanned: 2 branches\nskipped: 1 branches\nskipped b: pending\n```\n"
        );
    }
}
