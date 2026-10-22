//! Comparison CLI; lookup/run themselves remain Phase 1 work.
#![forbid(unsafe_code)]

use patchula_conformance::{Error, Rules, compare, parse_logs_with_diagnostics, run_renovate};
use patchula_report::Report;
use std::{path::PathBuf, process::ExitCode};
use usage_rs::Cli;

#[derive(Cli)]
#[usage(bin = "patchula-conformance", version)]
struct App {
    /// Versioned patchula report JSON (required).
    #[usage(long)]
    report: PathBuf,
    /// Existing Renovate debug JSON-lines log; omit to run pinned Renovate.
    #[usage(long)]
    renovate_log: Option<PathBuf>,
    /// Departure mapping JSON; defaults to the bundled conservative rules.
    #[usage(long)]
    rules: Option<PathBuf>,
    /// Directory for triage.md and triage.json; links assume the repository root.
    #[usage(long, default = "conformance-results")]
    output: PathBuf,
    /// owner/repo arguments; live targets or an exact subset of an existing log.
    #[usage(required = false)]
    repositories: Vec<String>,
}

fn run(app: App) -> Result<(), Error> {
    let logs = match &app.renovate_log {
        Some(path) => std::fs::read_to_string(path)?,
        None => run_renovate(&app.repositories)?,
    };
    let parsed = parse_logs_with_diagnostics(&logs)?;
    let mut renovate = parsed.report;
    let mut report = Report::from_json(&std::fs::read(&app.report)?)?;
    if !app.repositories.is_empty() {
        for name in &app.repositories {
            if !renovate.repositories.iter().any(|r| &r.repository == name)
                && !report.repositories.iter().any(|r| &r.repository == name)
            {
                return Err(Error::Input(format!(
                    "requested repository {name} is absent from both inputs"
                )));
            }
        }
        renovate
            .repositories
            .retain(|r| app.repositories.contains(&r.repository));
        report
            .repositories
            .retain(|r| app.repositories.contains(&r.repository));
    }
    let bytes = match app.rules {
        Some(path) => std::fs::read(path)?,
        None => include_bytes!("../departures.json").to_vec(),
    };
    let rules = Rules::from_json(&bytes)?;
    let mut triage = compare(&renovate, &report, &rules)?;
    triage.warnings = parsed
        .warnings
        .into_iter()
        .filter(|w| {
            app.repositories.is_empty()
                || w.repository
                    .as_ref()
                    .is_none_or(|r| app.repositories.contains(r))
        })
        .collect();
    triage.exclusions.extend(
        parsed
            .exclusions
            .into_iter()
            .filter(|e| app.repositories.is_empty() || app.repositories.contains(&e.repository)),
    );
    std::fs::create_dir_all(&app.output)?;
    // Local reports may be written anywhere; use an absolute documentation link.
    let docs = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../docs/src/content/docs/renovate/departures.md")
        .canonicalize()?;
    let markdown = triage.markdown().replace(
        "(docs/src/content/docs/renovate/departures.md#",
        &format!("({}#", docs.display()),
    );
    std::fs::write(app.output.join("triage.md"), markdown)?;
    std::fs::write(
        app.output.join("triage.json"),
        serde_json::to_vec_pretty(&triage)?,
    )?;
    println!(
        "Compared {} repositories; {} without baseline: {} differences. Reports: {}",
        triage
            .renovate
            .repositories
            .iter()
            .filter(|r| r.status == patchula_report::Status::Complete)
            .count(),
        triage.no_baseline.len(),
        triage.differences.len(),
        app.output.display()
    );
    Ok(())
}

fn main() -> ExitCode {
    match run(App::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("comparison failed: {error}");
            ExitCode::FAILURE
        }
    }
}
