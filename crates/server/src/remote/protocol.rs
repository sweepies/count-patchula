//! The wire contract between `patchula --remote` and `patchula serve`, defined
//! once so both sides serialize the same JSON. Old and new CLIs and servers
//! interoperate: every message keeps the shape it always had.
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Repositories one `POST /api/v1/runs` may name.
pub const MAX_REPOSITORIES: usize = 20;
/// How long a verified user token is trusted before GitHub is asked again.
pub const USER_TTL_SECONDS: i64 = 300;
/// How long a finished job's report stays available to poll.
pub const JOB_TTL_SECONDS: i64 = 900;
/// Finished jobs the server keeps at once.
pub const MAX_JOBS: usize = 1000;
/// The longest a remote run may take: one batch's work. A run that outlives
/// this is lost, and the CLI stops polling.
pub const RUN_LIMIT_SECONDS: i64 = 21_600;
/// The CLI's total patience for one run: `RUN_LIMIT_SECONDS` for the run
/// itself, then `JOB_TTL_SECONDS` for its report to be collected.
pub const DEADLINE_SECONDS: i64 = RUN_LIMIT_SECONDS + JOB_TTL_SECONDS;

/// What a `POST /api/v1/runs` asks the server to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Lookup,
    DryRun,
    Run,
}

/// The `POST /api/v1/runs` request body.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Start {
    pub mode: Mode,
    pub repositories: Vec<String>,
}

/// The `POST /api/v1/runs` 202 body.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Accepted {
    pub id: String,
}

/// The `GET /api/v1/auth` body: the App's client ID and bot login.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthInfo {
    pub client_id: String,
    pub bot: String,
}

/// An error body: `errorKind` names the rejection for programs, `error` is
/// the message a person reads. The one declaration of the `{errorKind,
/// error}` shape both sides of the wire share.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectBody {
    pub error_kind: String,
    pub error: String,
}

/// The `GET /api/v1/runs/{id}` 200 body. Serializes exactly as the ad-hoc
/// `json!` responses always did; `decode` reads it back with the report's
/// schema version checked first, as `Report::from_json` does.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum JobStatus {
    Running,
    Complete { report: patchula_report::Report },
    Failed { error: String },
}

/// Why a status body could not be read.
#[derive(Debug, thiserror::Error)]
pub enum StatusError {
    #[error("{0}")]
    Report(#[from] patchula_report::Error),
    #[error("the server returned an unknown run status")]
    Unknown,
}

impl JobStatus {
    /// Read a `GET /api/v1/runs/{id}` body. A complete report goes through
    /// `Report::from_json`, so a schema mismatch reports the version, not a
    /// decode error.
    pub fn decode(value: &Value) -> Result<Self, StatusError> {
        match value["status"].as_str() {
            Some("running") => Ok(Self::Running),
            Some("complete") => {
                let bytes =
                    serde_json::to_vec(&value["report"]).map_err(|_| StatusError::Unknown)?;
                Ok(Self::Complete {
                    report: patchula_report::Report::from_json(&bytes)?,
                })
            }
            Some("failed") => Ok(Self::Failed {
                error: value["error"]
                    .as_str()
                    .unwrap_or("the run failed")
                    .to_owned(),
            }),
            _ => Err(StatusError::Unknown),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_request_serializes_as_the_wire_shape() {
        assert_eq!(
            serde_json::to_value(Start {
                mode: Mode::DryRun,
                repositories: vec!["acme/app".into(), "acme/api".into()],
            })
            .unwrap(),
            json!({"mode": "dry-run", "repositories": ["acme/app", "acme/api"]})
        );
        assert_eq!(
            serde_json::to_value(Start {
                mode: Mode::Run,
                repositories: vec![],
            })
            .unwrap(),
            json!({"mode": "run", "repositories": []})
        );
    }

    #[test]
    fn every_mode_is_kebab_case_on_the_wire() {
        for (mode, text) in [
            (Mode::Lookup, "lookup"),
            (Mode::DryRun, "dry-run"),
            (Mode::Run, "run"),
        ] {
            assert_eq!(serde_json::to_value(mode).unwrap(), json!(text));
            assert_eq!(serde_json::from_value::<Mode>(json!(text)).unwrap(), mode);
        }
    }

    #[test]
    fn job_statuses_serialize_as_the_wire_shapes() {
        assert_eq!(
            serde_json::to_value(JobStatus::Running).unwrap(),
            json!({"status": "running"})
        );
        assert_eq!(
            serde_json::to_value(JobStatus::Failed {
                error: "the run failed on the server".into(),
            })
            .unwrap(),
            json!({"status": "failed", "error": "the run failed on the server"})
        );
        let report = patchula_report::Report {
            schema_version: patchula_report::SCHEMA_VERSION,
            repositories: vec![],
            stats: None,
            actions: None,
        };
        assert_eq!(
            serde_json::to_value(JobStatus::Complete {
                report: report.clone(),
            })
            .unwrap(),
            json!({"status": "complete", "report": report})
        );
    }

    #[test]
    fn job_status_decodes_and_checks_the_report_version() {
        assert!(matches!(
            JobStatus::decode(&json!({"status": "running"})).unwrap(),
            JobStatus::Running
        ));
        assert!(matches!(
            JobStatus::decode(&json!({"status": "failed", "error": "boom"})).unwrap(),
            JobStatus::Failed { error } if error == "boom"
        ));
        let report = serde_json::to_value(patchula_report::Report {
            schema_version: patchula_report::SCHEMA_VERSION + 1,
            repositories: vec![],
            stats: None,
            actions: None,
        })
        .unwrap();
        assert!(matches!(
            JobStatus::decode(&json!({"status": "complete", "report": report})),
            Err(StatusError::Report(patchula_report::Error::Version(_)))
        ));
        assert!(matches!(
            JobStatus::decode(&json!({"status": "bogus"})),
            Err(StatusError::Unknown)
        ));
    }
}
