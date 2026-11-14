//! Known vulnerabilities from OSV.dev: batched queries, answered from stored
//! listings inside their freshness window, and advisory records kept in the
//! cache until OSV reports them modified.
use crate::{Client, Error, FetchedAt, Result};
use futures_util::{StreamExt, stream};
use patchula_cache::State;
use patchula_words::Severity;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Advisory records fetched at once.
const ADVISORY_FAN_OUT: usize = 16;

const OSV: &str = "https://api.osv.dev/v1";
/// OSV accepts at most this many queries in one batch.
const BATCH: usize = 1000;
/// Pages one query may take. OSV pages a package's advisories by the
/// thousand, so a listing this long is a broken server, not a real package.
const MAX_PAGES: usize = 100;

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
    /// The advisory database's own rating — low, moderate, high or
    /// critical — or, when it gives none, the qualitative rating of the
    /// record's CVSS vector.
    #[serde(default, deserialize_with = "patchula_words::severity")]
    pub severity: Option<Severity>,
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
    #[serde(default, deserialize_with = "severities")]
    severity: Vec<RecordSeverity>,
    #[serde(default)]
    affected: Vec<RecordAffected>,
}
/// One of OSV's `severity` entries: a CVSS vector under `CVSS_V2`,
/// `CVSS_V3` or `CVSS_V4`, or another scheme, such as `Ubuntu`, whose
/// score may be a rating word.
#[derive(Deserialize)]
struct RecordSeverity {
    #[serde(rename = "type")]
    kind: String,
    score: String,
}
/// OSV `severity` entries, leniently: `null` or a value that isn't a list is
/// none, and an entry that doesn't fit the schema is skipped, so one odd
/// entry can't fail the record and every lookup that needs it.
fn severities<'de, D>(deserializer: D) -> std::result::Result<Vec<RecordSeverity>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match Value::deserialize(deserializer)? {
        Value::Array(entries) => entries
            .into_iter()
            .filter_map(|e| serde_json::from_value(e).ok())
            .collect(),
        _ => Vec::new(),
    })
}
#[derive(Deserialize)]
struct RecordAffected {
    #[serde(default)]
    package: Option<RecordPackage>,
    #[serde(default)]
    ranges: Vec<RecordRange>,
    #[serde(default)]
    versions: Vec<String>,
    #[serde(default, deserialize_with = "severities")]
    severity: Vec<RecordSeverity>,
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
        // The database's own rating first: `database_specific.severity`,
        // then a rating word under a non-CVSS scheme such as Ubuntu's; the
        // CVSS vectors only when neither is there. OSV says "medium" where
        // the Count says "moderate".
        let entries = || {
            record
                .severity
                .iter()
                .chain(record.affected.iter().flat_map(|a| &a.severity))
        };
        let severity = record
            .database_specific
            .as_ref()
            .and_then(|d| d["severity"].as_str())
            .and_then(Severity::advisory)
            .or_else(|| {
                // Several labels (one per affected package) give the most
                // severe, which sorts first.
                entries()
                    .filter(|e| !is_cvss(e))
                    .filter_map(|e| Severity::advisory(e.score.trim()))
                    .min()
            })
            .or_else(|| cvss_severity(entries().filter(|e| is_cvss(e))));
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

fn is_cvss(entry: &RecordSeverity) -> bool {
    matches!(entry.kind.as_str(), "CVSS_V2" | "CVSS_V3" | "CVSS_V4")
}

/// The qualitative rating of the newest CVSS version among `entries`: v4,
/// then v3.1 or v3.0, then v2. Several vectors of that version (one per
/// affected package) give the most severe. A vector that doesn't parse is
/// skipped, so a record with none that does is unrated. A 0.0 score (CVSS's
/// "none"; v2 has no such band and would call it low) has no word in the
/// Count's vocabulary and is unrated too. v2 has no Critical band: its High
/// covers 7.0–10.0.
fn cvss_severity<'a>(entries: impl Iterator<Item = &'a RecordSeverity>) -> Option<Severity> {
    let generation = |vector: &cvss::Cvss| match vector {
        cvss::Cvss::CvssV40(_) => 3,
        cvss::Cvss::CvssV30(_) | cvss::Cvss::CvssV31(_) => 2,
        _ => 1,
    };
    let rating = |vector: &cvss::Cvss| {
        if vector.score() == 0.0 {
            return None;
        }
        match vector.severity() {
            cvss::Severity::None => None,
            cvss::Severity::Low => Some(Severity::Low),
            cvss::Severity::Medium => Some(Severity::Moderate),
            cvss::Severity::High => Some(Severity::High),
            cvss::Severity::Critical => Some(Severity::Critical),
        }
    };
    let vectors: Vec<cvss::Cvss> = entries
        .filter_map(|e| e.score.trim().parse().ok())
        .collect();
    let newest = vectors.iter().map(generation).max()?;
    // The most severe sorts first; an unrated vector only when all are.
    vectors
        .iter()
        .filter(|v| generation(v) == newest)
        .filter_map(rating)
        .min()
}

#[derive(Deserialize)]
struct Batch {
    results: Vec<BatchResult>,
}
/// Advisory IDs and modification times per query, before their records.
pub type AdvisoryListing = BTreeMap<AdvisoryQuery, Vec<(String, String)>>;
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
pub(crate) struct Stored {
    modified: String,
    advisory: Option<Advisory>,
}
#[derive(Serialize)]
pub(crate) struct AdvisoryScope {
    id: String,
}
impl State for Stored {
    const NAMESPACE: &'static str = "osv-advisory";
    const VERSION: u32 = 2;
    // Code-versioned: the stored advisory is this build's `Record`-to-
    // `Advisory` mapping of the record the scope's id names.
    const CODE_VERSIONED: bool = true;
    type Scope = AdvisoryScope;
}
/// The `(id, modified)` pairs the batch answered for one version, kept for
/// the advisory freshness window.
#[derive(Serialize, Deserialize)]
struct StoredListing {
    fetched_at: i64,
    vulns: Vec<(String, String)>,
}
#[derive(Serialize)]
struct ListingScope {
    ecosystem: String,
    package: String,
    version: String,
}
impl ListingScope {
    fn of(query: &AdvisoryQuery) -> Self {
        Self {
            ecosystem: query.ecosystem.clone(),
            package: query.package.clone(),
            version: query.version.clone(),
        }
    }
}
impl FetchedAt for StoredListing {
    fn fetched_at(&self) -> i64 {
        self.fetched_at
    }
}
impl State for StoredListing {
    const NAMESPACE: &'static str = "osv-listing";
    const VERSION: u32 = 1;
    type Scope = ListingScope;
}

impl Client {
    /// Advisories affecting each queried version, without withdrawn ones.
    pub async fn advisories(
        &self,
        queries: &BTreeSet<AdvisoryQuery>,
    ) -> Result<BTreeMap<AdvisoryQuery, Vec<Arc<Advisory>>>> {
        let listing = self.advisory_listing(queries).await?;
        self.advisory_records(&listing, |_| true).await
    }

    /// The IDs and modification times of the advisories OSV lists for each
    /// queried version. A listing still inside the advisory freshness window
    /// is answered from `osv-listing` state; the rest go in the batch, and a
    /// failed batch stores nothing. A revalidating client asks OSV again.
    pub async fn advisory_listing(
        &self,
        queries: &BTreeSet<AdvisoryQuery>,
    ) -> Result<AdvisoryListing> {
        let window = self.http.advisory_freshness();
        let mut answered = AdvisoryListing::new();
        let mut pending: Vec<(&AdvisoryQuery, Option<String>)> = Vec::new();
        for query in queries {
            if let Some(saved) = self
                .fresh_state_within::<StoredListing>(&ListingScope::of(query), window)
                .await?
            {
                answered.insert(query.clone(), saved.vulns);
            } else {
                pending.push((query, None));
            }
        }
        let mut listed = BTreeMap::<&AdvisoryQuery, Vec<Listed>>::new();
        // Every page token a query has been given: a repeat or too many pages
        // fails the listing rather than paging forever.
        let mut tokens = BTreeMap::<&AdvisoryQuery, BTreeSet<String>>::new();
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
                        let seen = tokens.entry(query).or_default();
                        if seen.len() + 1 >= MAX_PAGES || !seen.insert(token.clone()) {
                            return Err(Error::Invalid);
                        }
                        next.push((*query, Some(token)));
                    }
                }
            }
            pending = next;
        }
        let fetched_at = chrono::Utc::now().timestamp_millis();
        for (query, vulns) in listed {
            let vulns: Vec<(String, String)> =
                vulns.into_iter().map(|l| (l.id, l.modified)).collect();
            self.cache
                .set_state(
                    &ListingScope::of(query),
                    &StoredListing {
                        fetched_at,
                        vulns: vulns.clone(),
                    },
                )
                .await?;
            answered.insert(query.clone(), vulns);
        }
        Ok(answered)
    }

    /// The records of a listing's advisories that `wanted` selects, read from
    /// the cache unless OSV says they changed. Withdrawn ones are left out.
    pub async fn advisory_records(
        &self,
        listing: &AdvisoryListing,
        wanted: impl Fn(&str) -> bool,
    ) -> Result<BTreeMap<AdvisoryQuery, Vec<Arc<Advisory>>>> {
        // A listing reused inside its window and one fetched this run can
        // name the same advisory at different modification times: the latest
        // wins, so a record OSV says changed is read again.
        let mut latest = BTreeMap::<String, String>::new();
        for (id, modified) in listing.values().flatten().filter(|(id, _)| wanted(id)) {
            latest
                .entry(id.clone())
                .and_modify(|seen| {
                    if later(modified, seen) {
                        seen.clone_from(modified);
                    }
                })
                .or_insert_with(|| modified.clone());
        }
        let records: BTreeMap<String, Option<Arc<Advisory>>> = stream::iter(latest)
            .map(|(id, modified)| async move {
                let advisory = self.advisory(&id, &modified).await?;
                Ok::<_, Error>((id, advisory))
            })
            .buffer_unordered(ADVISORY_FAN_OUT)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<_>>()?;
        Ok(listing
            .iter()
            .map(|(query, ids)| {
                let mut found: Vec<_> = ids
                    .iter()
                    .filter_map(|(id, _)| records.get(id.as_str()).cloned().flatten())
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
        let scope = AdvisoryScope { id: id.into() };
        if let Some(stored) = self.cache.state::<Stored>(&scope).await?
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
        self.cache.set_state(&scope, &stored).await?;
        Ok(stored.advisory.map(Arc::new))
    }
}

/// Whether OSV modification time `a` is later than `b`. Both are RFC 3339;
/// one that does not parse loses to one that does, and two that do not
/// compare as text.
fn later(a: &str, b: &str) -> bool {
    use chrono::DateTime;
    match (
        DateTime::parse_from_rfc3339(a),
        DateTime::parse_from_rfc3339(b),
    ) {
        (Ok(a), Ok(b)) => a > b,
        (Ok(_), Err(_)) => true,
        (Err(_), Ok(_)) => false,
        (Err(_), Err(_)) => a > b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use patchula_cache::Cache;
    use patchula_http::{Exchange, Response, Transport};
    use std::sync::Mutex;

    fn query(ecosystem: &str, package: &str, version: &str) -> AdvisoryQuery {
        AdvisoryQuery {
            ecosystem: ecosystem.into(),
            package: package.into(),
            version: version.into(),
        }
    }
    fn now() -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
    /// An OSV batch endpoint: records the `(ecosystem, package, version)` of
    /// every query posted and answers each result with `vulns`.
    struct Osv {
        batches: Mutex<Vec<Vec<(String, String, String)>>>,
        vulns: Vec<(String, String)>,
    }
    impl Osv {
        fn asked(&self) -> Vec<Vec<(String, String, String)>> {
            self.batches.lock().unwrap().clone()
        }
    }
    impl Transport for Osv {
        fn send(&self, request: reqwest::Request) -> Exchange<'_> {
            assert_eq!(request.url().as_str(), "https://api.osv.dev/v1/querybatch");
            let body: Value =
                serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
            let queries: Vec<(String, String, String)> = body["queries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|q| {
                    (
                        q["package"]["ecosystem"].as_str().unwrap().to_owned(),
                        q["package"]["name"].as_str().unwrap().to_owned(),
                        q["version"].as_str().unwrap().to_owned(),
                    )
                })
                .collect();
            let vulns: Vec<Value> = self
                .vulns
                .iter()
                .map(|(id, modified)| json!({ "id": id, "modified": modified }))
                .collect();
            let results: Vec<Value> = queries.iter().map(|_| json!({ "vulns": vulns })).collect();
            self.batches.lock().unwrap().push(queries);
            let response = Response {
                status: reqwest::StatusCode::OK,
                headers: Default::default(),
                body: serde_json::to_vec(&json!({ "results": results })).unwrap(),
                stale: None,
            };
            Box::pin(async move { Ok(response) })
        }
    }
    async fn client(transport: Arc<Osv>, window_secs: u64) -> (Client, Cache) {
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let github = patchula_github::Client::with_transport(
            "offline",
            cache.clone(),
            Default::default(),
            transport.clone(),
        )
        .unwrap();
        let http = patchula_http::Cached::with_options(
            cache.clone(),
            transport,
            patchula_http::Options {
                advisory_freshness_secs: window_secs,
                ..crate::http_options()
            },
        )
        .unwrap();
        (Client::new(github, http, cache.clone()), cache)
    }
    async fn seed(cache: &Cache, query: &AdvisoryQuery, fetched_at: i64, vulns: &[(&str, &str)]) {
        cache
            .set_state(
                &ListingScope::of(query),
                &StoredListing {
                    fetched_at,
                    vulns: vulns
                        .iter()
                        .map(|(id, modified)| (id.to_string(), modified.to_string()))
                        .collect(),
                },
            )
            .await
            .unwrap();
    }

    /// The severity the Count gives an OSV record with this
    /// `database_specific` and these top-level `severity` entries.
    fn rated(database_specific: Value, entries: &[(&str, &str)]) -> Option<Severity> {
        let severity: Vec<Value> = entries
            .iter()
            .map(|(kind, score)| json!({"type": kind, "score": score}))
            .collect();
        let record: Record = serde_json::from_value(json!({
            "id": "TEST-1",
            "database_specific": database_specific,
            "severity": severity,
        }))
        .unwrap();
        Advisory::from(record).severity
    }
    fn v3(vector: &str) -> Option<Severity> {
        rated(Value::Null, &[("CVSS_V3", vector)])
    }

    #[test]
    fn the_rsa_timing_advisory_is_moderate() {
        // RUSTSEC-2023-0071 as OSV serves it: no label, a CVSS 3.1 vector at
        // the top level and again in the RustSec-specific `cvss`.
        let record: Record = serde_json::from_value(json!({
            "id": "RUSTSEC-2023-0071",
            "modified": "2024-01-01T00:00:00Z",
            "database_specific": {"license": "CC0-1.0"},
            "severity": [{"type": "CVSS_V3", "score": "CVSS:3.1/AV:N/AC:H/PR:N/UI:N/S:U/C:H/I:N/A:N"}],
            "affected": [{
                "package": {"ecosystem": "crates.io", "name": "rsa"},
                "database_specific": {"cvss": "CVSS:3.1/AV:N/AC:H/PR:N/UI:N/S:U/C:H/I:N/A:N"},
                "ranges": [{"type": "SEMVER", "events": [{"introduced": "0.0.0-0"}]}],
            }],
        }))
        .unwrap();
        assert_eq!(Advisory::from(record).severity, Some(Severity::Moderate));
    }

    #[test]
    fn cvss_3_vectors_rate_by_the_spec_bands() {
        // Scores from the FIRST CVSS v3.1 specification and user guide
        // examples, checked against the crate's base score.
        for (vector, score, severity) in [
            // Shellshock, CVE-2014-6271.
            (
                "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H",
                9.8,
                Some(Severity::Critical),
            ),
            (
                "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:C/C:H/I:H/A:H",
                10.0,
                Some(Severity::Critical),
            ),
            // Heartbleed, CVE-2014-0160.
            (
                "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:N/A:N",
                7.5,
                Some(Severity::High),
            ),
            // phpMyAdmin reflected XSS, CVE-2013-1937.
            (
                "CVSS:3.1/AV:N/AC:L/PR:N/UI:R/S:C/C:L/I:L/A:N",
                6.1,
                Some(Severity::Moderate),
            ),
            (
                "CVSS:3.1/AV:N/AC:H/PR:N/UI:N/S:U/C:H/I:N/A:N",
                5.9,
                Some(Severity::Moderate),
            ),
            (
                "CVSS:3.1/AV:P/AC:H/PR:H/UI:R/S:U/C:L/I:N/A:N",
                1.6,
                Some(Severity::Low),
            ),
            (
                "CVSS:3.0/AV:P/AC:H/PR:H/UI:R/S:U/C:L/I:N/A:N",
                1.6,
                Some(Severity::Low),
            ),
            // No impact scores 0.0, CVSS's "none": unrated.
            ("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:N/A:N", 0.0, None),
        ] {
            let parsed: cvss::Cvss = vector.parse().unwrap();
            assert_eq!(parsed.score(), score, "{vector}");
            assert_eq!(v3(vector), severity, "{vector}");
        }
    }

    #[test]
    fn a_label_wins_over_a_vector() {
        let critical = "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H";
        assert_eq!(
            rated(json!({"severity": "LOW"}), &[("CVSS_V3", critical)]),
            Some(Severity::Low)
        );
        // A word outside the vocabulary is no label: the vector rates it.
        assert_eq!(
            rated(json!({"severity": "UNKNOWN"}), &[("CVSS_V3", critical)]),
            Some(Severity::Critical)
        );
    }

    #[test]
    fn an_invalid_or_missing_vector_is_unrated() {
        assert_eq!(rated(Value::Null, &[]), None);
        assert_eq!(v3("CVSS:3.1/AV:N/AC:L"), None);
        assert_eq!(v3("CVSS:3.1/AV:X/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"), None);
        assert_eq!(v3("not a vector"), None);
        assert_eq!(v3(""), None);
        assert_eq!(rated(Value::Null, &[("Ubuntu", "not-a-word")]), None);
        // An invalid newer vector doesn't hide a valid older one.
        assert_eq!(
            rated(
                Value::Null,
                &[
                    ("CVSS_V4", "CVSS:4.0/AV:N/garbage"),
                    ("CVSS_V3", "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:N/A:N"),
                ]
            ),
            Some(Severity::High)
        );
    }

    #[test]
    fn a_zero_score_is_unrated_in_every_version() {
        assert_eq!(v3("CVSS:3.0/AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:N/A:N"), None);
        // cvss calls a v2 0.0 low; the Count leaves it unrated.
        assert_eq!(
            rated(Value::Null, &[("CVSS_V2", "AV:N/AC:L/Au:N/C:N/I:N/A:N")]),
            None
        );
        assert_eq!(
            rated(
                Value::Null,
                &[(
                    "CVSS_V4",
                    "CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:N/VI:N/VA:N/SC:N/SI:N/SA:N"
                )]
            ),
            None
        );
    }

    #[test]
    fn a_scheme_label_wins_over_a_vector() {
        // UBUNTU-CVE-2023-4863: Ubuntu rates it medium beside an 8.8 vector.
        let high = "CVSS:3.1/AV:N/AC:L/PR:N/UI:R/S:U/C:H/I:H/A:H";
        assert_eq!(
            rated(Value::Null, &[("CVSS_V3", high), ("Ubuntu", "medium")]),
            Some(Severity::Moderate)
        );
        // database_specific's label outranks it.
        assert_eq!(
            rated(
                json!({"severity": "LOW"}),
                &[("Ubuntu", "high"), ("CVSS_V3", high)]
            ),
            Some(Severity::Low)
        );
        // A word outside the vocabulary leaves the vector to rate it.
        assert_eq!(
            rated(Value::Null, &[("Ubuntu", "negligible"), ("CVSS_V3", high)]),
            Some(Severity::High)
        );
    }

    #[test]
    fn odd_severity_entries_are_skipped_not_fatal() {
        let record: Record = serde_json::from_value(json!({
            "id": "TEST-3",
            "severity": [
                {"type": "CVSS_V3"},
                {"score": "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"},
                {"type": "CVSS_V3", "score": 9.8},
                "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H",
                null,
                {"type": "CVSS_V3", "score": "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:N/A:N"},
            ],
            "affected": [{"package": {"ecosystem": "PyPI", "name": "a"}, "severity": null}],
        }))
        .unwrap();
        assert_eq!(Advisory::from(record).severity, Some(Severity::High));
        for severity in [json!(null), json!("high"), json!({"type": "CVSS_V3"})] {
            let record: Record =
                serde_json::from_value(json!({"id": "TEST-4", "severity": severity})).unwrap();
            assert_eq!(Advisory::from(record).severity, None);
        }
    }

    #[test]
    fn cvss_2_has_no_critical_band() {
        // NVD's v2 bands: Low 0.0–3.9, Medium 4.0–6.9, High 7.0–10.0.
        let v2 = |vector| rated(Value::Null, &[("CVSS_V2", vector)]);
        assert_eq!(v2("AV:N/AC:L/Au:N/C:C/I:C/A:C"), Some(Severity::High));
        assert_eq!(v2("AV:N/AC:M/Au:N/C:P/I:N/A:N"), Some(Severity::Moderate));
        assert_eq!(v2("AV:L/AC:H/Au:M/C:P/I:N/A:N"), Some(Severity::Low));
    }

    #[test]
    fn the_newest_cvss_version_wins() {
        let v4_critical = "CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:H/VI:H/VA:H/SC:N/SI:N/SA:N";
        let parsed: cvss::Cvss = v4_critical.parse().unwrap();
        assert_eq!(parsed.score(), 9.3);
        let v3_low = "CVSS:3.1/AV:P/AC:H/PR:H/UI:R/S:U/C:L/I:N/A:N";
        let v2_high = "AV:N/AC:L/Au:N/C:C/I:C/A:C";
        assert_eq!(
            rated(
                Value::Null,
                &[("CVSS_V3", v3_low), ("CVSS_V4", v4_critical)]
            ),
            Some(Severity::Critical)
        );
        assert_eq!(
            rated(Value::Null, &[("CVSS_V2", v2_high), ("CVSS_V3", v3_low)]),
            Some(Severity::Low)
        );
    }

    #[test]
    fn per_package_vectors_give_the_most_severe() {
        let record: Record = serde_json::from_value(json!({
            "id": "TEST-2",
            "affected": [
                {"package": {"ecosystem": "PyPI", "name": "a"},
                 "severity": [{"type": "CVSS_V3", "score": "CVSS:3.1/AV:P/AC:H/PR:H/UI:R/S:U/C:L/I:N/A:N"}]},
                {"package": {"ecosystem": "PyPI", "name": "b"},
                 "severity": [{"type": "CVSS_V3", "score": "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:N/A:N"}]},
            ],
        }))
        .unwrap();
        assert_eq!(Advisory::from(record).severity, Some(Severity::High));
    }

    /// The latest modification time wins; one that does not parse loses.
    #[test]
    fn the_later_modification_wins() {
        assert!(later("2026-11-12T00:00:00Z", "2026-11-01T00:00:00Z"));
        assert!(!later("2026-11-01T00:00:00Z", "2026-11-12T00:00:00Z"));
        assert!(later("2026-11-12T00:00:00.5Z", "2026-11-12T00:00:00Z"));
        assert!(later("2026-11-01T00:00:00Z", "not a time"));
        assert!(!later("", "2026-11-01T00:00:00Z"));
    }

    /// An OSV endpoint whose batch answers fewer results than queries.
    struct Short;
    impl Transport for Short {
        fn send(&self, _: reqwest::Request) -> Exchange<'_> {
            let response = Response {
                status: reqwest::StatusCode::OK,
                headers: Default::default(),
                body: serde_json::to_vec(&json!({ "results": [] })).unwrap(),
                stale: None,
            };
            Box::pin(async move { Ok(response) })
        }
    }

    /// A batch that does not answer every query fails the listing and stores
    /// nothing, so the next run asks again.
    #[tokio::test(start_paused = true)]
    async fn a_short_batch_stores_nothing() {
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let transport = Arc::new(Short);
        let github = patchula_github::Client::with_transport(
            "offline",
            cache.clone(),
            Default::default(),
            transport.clone(),
        )
        .unwrap();
        let http = patchula_http::Cached::with_options(
            cache.clone(),
            transport,
            patchula_http::Options {
                advisory_freshness_secs: 3600,
                ..crate::http_options()
            },
        )
        .unwrap();
        let client = Client::new(github, http, cache.clone());
        let q = query("npm", "lodash", "4.17.11");
        assert!(
            client
                .advisory_listing(&BTreeSet::from([q.clone()]))
                .await
                .is_err()
        );
        assert!(
            cache
                .state::<StoredListing>(&ListingScope::of(&q))
                .await
                .unwrap()
                .is_none()
        );
    }

    /// A listing inside its freshness window answers with no request.
    #[tokio::test(start_paused = true)]
    async fn a_listing_inside_its_window_is_reused() {
        let osv = Arc::new(Osv {
            batches: Mutex::new(vec![]),
            vulns: vec![],
        });
        let (client, cache) = client(osv.clone(), 3600).await;
        let q = query("npm", "lodash", "4.17.11");
        seed(&cache, &q, now(), &[("GHSA-old", "2026-01-01T00:00:00Z")]).await;
        let listing = client
            .advisory_listing(&BTreeSet::from([q.clone()]))
            .await
            .unwrap();
        assert_eq!(
            listing[&q],
            [("GHSA-old".to_owned(), "2026-01-01T00:00:00Z".to_owned())]
        );
        assert!(osv.asked().is_empty());
    }

    /// A query with nothing stored is posted, and its answer is stored for
    /// the next run.
    #[tokio::test(start_paused = true)]
    async fn a_missing_listing_posts_and_is_stored() {
        let osv = Arc::new(Osv {
            batches: Mutex::new(vec![]),
            vulns: vec![("GHSA-new".into(), "2026-01-02T00:00:00Z".into())],
        });
        let (client, _) = client(osv.clone(), 3600).await;
        let q = query("npm", "lodash", "4.17.11");
        let queries = || BTreeSet::from([q.clone()]);
        let listing = client.advisory_listing(&queries()).await.unwrap();
        assert_eq!(
            listing[&q],
            [("GHSA-new".to_owned(), "2026-01-02T00:00:00Z".to_owned())]
        );
        assert_eq!(
            osv.asked(),
            vec![vec![(
                "npm".to_owned(),
                "lodash".to_owned(),
                "4.17.11".to_owned()
            )]]
        );
        let listing = client.advisory_listing(&queries()).await.unwrap();
        assert_eq!(listing[&q][0].0, "GHSA-new");
        assert_eq!(osv.asked().len(), 1, "the second run must not post");
    }

    /// Fresh and missing queries split: only the missing one is posted, and
    /// the answer covers both.
    #[tokio::test(start_paused = true)]
    async fn only_the_queries_past_their_window_are_posted() {
        let osv = Arc::new(Osv {
            batches: Mutex::new(vec![]),
            vulns: vec![("GHSA-new".into(), "2026-01-02T00:00:00Z".into())],
        });
        let (client, cache) = client(osv.clone(), 3600).await;
        let fresh = query("npm", "lodash", "4.17.11");
        let stale = query("npm", "ms", "2.1.3");
        seed(
            &cache,
            &fresh,
            now(),
            &[("GHSA-old", "2026-01-01T00:00:00Z")],
        )
        .await;
        let listing = client
            .advisory_listing(&BTreeSet::from([fresh.clone(), stale.clone()]))
            .await
            .unwrap();
        assert_eq!(
            osv.asked(),
            vec![vec![(
                "npm".to_owned(),
                "ms".to_owned(),
                "2.1.3".to_owned()
            )]]
        );
        assert_eq!(listing[&fresh][0].0, "GHSA-old");
        assert_eq!(listing[&stale][0].0, "GHSA-new");
    }

    /// A listing past its window is asked again.
    #[tokio::test(start_paused = true)]
    async fn an_expired_listing_is_asked_again() {
        let osv = Arc::new(Osv {
            batches: Mutex::new(vec![]),
            vulns: vec![("GHSA-new".into(), "2026-01-02T00:00:00Z".into())],
        });
        let (client, cache) = client(osv.clone(), 3600).await;
        let q = query("npm", "lodash", "4.17.11");
        seed(
            &cache,
            &q,
            now() - 3_600_001,
            &[("GHSA-old", "2026-01-01T00:00:00Z")],
        )
        .await;
        let listing = client
            .advisory_listing(&BTreeSet::from([q.clone()]))
            .await
            .unwrap();
        assert_eq!(listing[&q][0].0, "GHSA-new");
        assert_eq!(osv.asked().len(), 1);
    }

    /// A scoped retry asks OSV however fresh the listing is.
    #[tokio::test(start_paused = true)]
    async fn revalidation_bypasses_the_window() {
        let osv = Arc::new(Osv {
            batches: Mutex::new(vec![]),
            vulns: vec![("GHSA-new".into(), "2026-01-02T00:00:00Z".into())],
        });
        let (client, cache) = client(osv.clone(), 3600).await;
        let q = query("npm", "lodash", "4.17.11");
        seed(&cache, &q, now(), &[("GHSA-old", "2026-01-01T00:00:00Z")]).await;
        let listing = client
            .revalidating()
            .advisory_listing(&BTreeSet::from([q.clone()]))
            .await
            .unwrap();
        assert_eq!(listing[&q][0].0, "GHSA-new");
        assert_eq!(osv.asked().len(), 1);
    }

    /// Zero is the behaviour before the window: every query posts every run.
    #[tokio::test(start_paused = true)]
    async fn a_zero_window_posts_every_query_every_run() {
        let osv = Arc::new(Osv {
            batches: Mutex::new(vec![]),
            vulns: vec![],
        });
        let (client, cache) = client(osv.clone(), 0).await;
        let q = query("npm", "lodash", "4.17.11");
        seed(&cache, &q, now(), &[]).await;
        client
            .advisory_listing(&BTreeSet::from([q.clone()]))
            .await
            .unwrap();
        client.advisory_listing(&BTreeSet::from([q])).await.unwrap();
        assert_eq!(osv.asked().len(), 2);
    }
}
