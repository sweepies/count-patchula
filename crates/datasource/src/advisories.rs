//! Known vulnerabilities from OSV.dev: one batched query per run, and advisory
//! records kept in the cache until OSV reports them modified.
use crate::{Client, Error, Result};
use futures_util::{StreamExt, stream};
use patchula_cache::ExtractionKey;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const OSV: &str = "https://api.osv.dev/v1";
/// OSV accepts at most this many queries in one batch.
const BATCH: usize = 1000;

/// One package version to check, in OSV's ecosystem naming.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AdvisoryQuery {
    pub ecosystem: String,
    pub package: String,
    pub version: String,
}

/// The parts of an OSV record the Count uses.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Advisory {
    pub id: String,
    pub modified: String,
    pub aliases: Vec<String>,
    pub summary: Option<String>,
    /// The advisory database's own rating: low, moderate, high or critical.
    pub severity: Option<String>,
    pub affected: Vec<Affected>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Affected {
    pub ecosystem: String,
    pub package: String,
    pub ranges: Vec<Range>,
    pub versions: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Range {
    /// SEMVER, ECOSYSTEM or GIT.
    pub kind: String,
    pub events: Vec<Event>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Event {
    Introduced(String),
    Fixed(String),
    LastAffected(String),
    Limit(String),
}

#[derive(Deserialize)]
struct Record {
    id: String,
    #[serde(default)]
    modified: String,
    #[serde(default)]
    withdrawn: Option<String>,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    database_specific: Option<Value>,
    #[serde(default)]
    affected: Vec<RecordAffected>,
}
#[derive(Deserialize)]
struct RecordAffected {
    #[serde(default)]
    package: Option<RecordPackage>,
    #[serde(default)]
    ranges: Vec<RecordRange>,
    #[serde(default)]
    versions: Vec<String>,
}
#[derive(Deserialize)]
struct RecordPackage {
    ecosystem: String,
    name: String,
}
#[derive(Deserialize)]
struct RecordRange {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    events: Vec<BTreeMap<String, String>>,
}

impl From<Record> for Advisory {
    fn from(record: Record) -> Self {
        let severity = record
            .database_specific
            .as_ref()
            .and_then(|d| d["severity"].as_str())
            .map(str::to_ascii_lowercase)
            .and_then(|s| match s.as_str() {
                "low" | "moderate" | "high" | "critical" => Some(s),
                "medium" => Some("moderate".into()),
                _ => None,
            });
        let event = |(kind, version): (String, String)| match kind.as_str() {
            "introduced" => Some(Event::Introduced(version)),
            "fixed" => Some(Event::Fixed(version)),
            "last_affected" => Some(Event::LastAffected(version)),
            "limit" => Some(Event::Limit(version)),
            _ => None,
        };
        Self {
            id: record.id,
            modified: record.modified,
            aliases: record.aliases,
            summary: record.summary.filter(|s| !s.trim().is_empty()),
            severity,
            affected: record
                .affected
                .into_iter()
                .filter_map(|a| {
                    let package = a.package?;
                    Some(Affected {
                        ecosystem: package.ecosystem,
                        package: package.name,
                        ranges: a
                            .ranges
                            .into_iter()
                            .map(|r| Range {
                                kind: r.kind,
                                events: r
                                    .events
                                    .into_iter()
                                    .flat_map(|e| e.into_iter().filter_map(event))
                                    .collect(),
                            })
                            .collect(),
                        versions: a.versions,
                    })
                })
                .collect(),
        }
    }
}

#[derive(Deserialize)]
struct Batch {
    results: Vec<BatchResult>,
}
#[derive(Deserialize)]
struct BatchResult {
    #[serde(default)]
    vulns: Vec<Listed>,
    #[serde(default)]
    next_page_token: Option<String>,
}
#[derive(Deserialize)]
struct Listed {
    id: String,
    #[serde(default)]
    modified: String,
}
/// A cached record; withdrawn advisories are remembered as `None`.
#[derive(Serialize, Deserialize)]
struct Stored {
    modified: String,
    advisory: Option<Advisory>,
}

impl Client {
    /// Advisories affecting each queried version, without withdrawn ones.
    pub async fn advisories(
        &self,
        queries: &BTreeSet<AdvisoryQuery>,
    ) -> Result<BTreeMap<AdvisoryQuery, Vec<Arc<Advisory>>>> {
        let mut listed = BTreeMap::<&AdvisoryQuery, Vec<Listed>>::new();
        let mut pending: Vec<(&AdvisoryQuery, Option<String>)> =
            queries.iter().map(|q| (q, None)).collect();
        while !pending.is_empty() {
            let mut next = Vec::new();
            for chunk in pending.chunks(BATCH) {
                let queries: Vec<_> = chunk
                    .iter()
                    .map(|(q, token)| {
                        let mut query = json!({
                            "package": {"ecosystem": q.ecosystem, "name": q.package},
                            "version": q.version,
                        });
                        if let Some(token) = token {
                            query["page_token"] = json!(token);
                        }
                        query
                    })
                    .collect();
                let response = self
                    .http
                    .json(
                        Method::POST,
                        &format!("{OSV}/querybatch"),
                        Some(serde_json::to_vec(&json!({ "queries": queries }))?),
                    )
                    .await?;
                let batch: Batch = serde_json::from_slice(&response.body)?;
                if batch.results.len() != chunk.len() {
                    return Err(Error::Invalid);
                }
                for ((query, _), result) in chunk.iter().zip(batch.results) {
                    listed.entry(query).or_default().extend(result.vulns);
                    if let Some(token) = result.next_page_token {
                        next.push((*query, Some(token)));
                    }
                }
            }
            pending = next;
        }
        let wanted: BTreeMap<String, String> = listed
            .values()
            .flatten()
            .map(|l| (l.id.clone(), l.modified.clone()))
            .collect();
        let records: BTreeMap<String, Option<Arc<Advisory>>> = stream::iter(wanted)
            .map(|(id, modified)| async move {
                let advisory = self.advisory(&id, &modified).await?;
                Ok::<_, Error>((id, advisory))
            })
            .buffer_unordered(16)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<_>>()?;
        Ok(queries
            .iter()
            .map(|query| {
                let mut found: Vec<_> = listed
                    .get(query)
                    .into_iter()
                    .flatten()
                    .filter_map(|l| records.get(l.id.as_str()).cloned().flatten())
                    .collect();
                found.sort_by(|a, b| a.id.cmp(&b.id));
                found.dedup_by(|a, b| a.id == b.id);
                (query.clone(), found)
            })
            .collect())
    }

    async fn advisory(&self, id: &str, modified: &str) -> Result<Option<Arc<Advisory>>> {
        if id.is_empty()
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
        {
            return Err(Error::Invalid);
        }
        let key = ExtractionKey {
            repo: format!("osv:{id}"),
            default_branch_commit: String::new(),
            config_hash: "osv-advisory-v1".into(),
        };
        if let Some(bytes) = self.cache.extraction_get(&key).await?
            && let Ok(stored) = serde_json::from_slice::<Stored>(&bytes)
            && stored.modified == modified
        {
            return Ok(stored.advisory.map(Arc::new));
        }
        let response = self
            .http
            .json(Method::GET, &format!("{OSV}/vulns/{id}"), None)
            .await?;
        let record: Record = serde_json::from_slice(&response.body)?;
        let stored = Stored {
            modified: modified.into(),
            advisory: record.withdrawn.is_none().then(|| Advisory::from(record)),
        };
        self.cache
            .extraction_put(&key, &serde_json::to_vec(&stored)?)
            .await?;
        Ok(stored.advisory.map(Arc::new))
    }
}
