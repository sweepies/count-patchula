//! Compare only observable Renovate JSON log output with rustovate reports.
#![forbid(unsafe_code)]

mod diff;
mod renovate;
mod run;

pub use diff::{Classification, Difference, Rules, Triage, compare};
pub use renovate::{Exclusion, ParsedLogs, Warning, parse_logs, parse_logs_with_diagnostics};
pub use run::run_renovate;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Input(String),
    #[error("I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Report(#[from] rustovate_report::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
