//! Versioned caller interface for lookup/run batches, independent of log output.
#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid report JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported report schemaVersion {0}; expected 1")]
    Version(u32),
    #[error("invalid report: {0}")]
    Invalid(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Report {
    pub schema_version: u32,
    pub repositories: Vec<Repository>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Complete,
    Skipped,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Repository {
    pub repository: String,
    pub status: Status,
    pub reason: Option<String>,
    pub package_files: Vec<PackageFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageFile {
    pub path: String,
    pub dependencies: Vec<Dependency>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Dependency {
    /// Canonical rustovate manager identifier; unknown upstream names stay literal.
    pub manager: String,
    pub name: String,
    /// Registry identity, which may differ from the name in the manifest.
    pub package: String,
    pub current: Current,
    pub updates: Vec<Update>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Current {
    pub value: Option<String>,
    pub version: Option<String>,
    pub digest: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Update {
    pub new_value: Option<String>,
    pub new_version: Option<String>,
    pub new_digest: Option<String>,
    /// major, minor, patch, digest, pin; comparison retains other upstream types.
    pub update_type: String,
    pub group: Option<String>,
}

impl Report {
    /// Check the version before deserializing its payload, including future schemas.
    pub fn from_json(bytes: &[u8]) -> Result<Self, Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Header {
            schema_version: u32,
        }
        let header: Header = serde_json::from_slice(bytes)?;
        if header.schema_version != SCHEMA_VERSION {
            return Err(Error::Version(header.schema_version));
        }
        let report: Self = serde_json::from_slice(bytes)?;
        report.validate()?;
        Ok(report)
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(Error::Version(self.schema_version));
        }
        let mut repos = BTreeSet::new();
        for repo in &self.repositories {
            if repo.repository.is_empty() || !repos.insert(&repo.repository) {
                return Err(Error::Invalid("empty or duplicate repository".into()));
            }
            if repo.status != Status::Complete && repo.reason.as_ref().is_none_or(String::is_empty)
            {
                return Err(Error::Invalid(
                    "skipped/error repository needs a reason".into(),
                ));
            }
            let mut files = BTreeSet::new();
            for file in &repo.package_files {
                if file.path.is_empty() || !files.insert(&file.path) {
                    return Err(Error::Invalid(
                        "empty or duplicate package-file path".into(),
                    ));
                }
                let mut dependencies = BTreeSet::new();
                for dep in &file.dependencies {
                    if dep.manager.is_empty()
                        || dep.name.is_empty()
                        || dep.package.is_empty()
                        || !dependencies.insert((&dep.manager, &dep.name))
                    {
                        return Err(Error::Invalid(
                            "empty dependency identity or ambiguous duplicate manager/name".into(),
                        ));
                    }
                    let mut updates = BTreeSet::new();
                    for update in &dep.updates {
                        if update.update_type.is_empty()
                            || (update.new_value.is_none()
                                && update.new_version.is_none()
                                && update.new_digest.is_none())
                            || !updates.insert(update)
                        {
                            return Err(Error::Invalid(
                                "empty or duplicate proposed update".into(),
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_future_schema_before_interpreting_payload() {
        assert!(matches!(
            Report::from_json(br#"{"schemaVersion":2,"future":true}"#),
            Err(Error::Version(2))
        ));
        assert!(
            Report::from_json(br#"{"schemaVersion":1,"repositories":[],"typo":true}"#).is_err()
        );
    }
}
