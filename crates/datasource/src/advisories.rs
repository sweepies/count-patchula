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
    /// The advisory database's own rating: low, moderate, high or critical.
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
        // OSV says "medium" where the Count says "moderate".
        let severity = record
            .database_specific
            .as_ref()
            .and_then(|d| d["severity"].as_str())
            .and_then(Severity::advisory);
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
struct Stored {
    modified: String,
    advisory: Option<Advisory>,
}
#[derive(Serialize)]
struct AdvisoryScope {
    id: String,
}
impl State for Stored {
    const NAMESPACE: &'static str = "osv-advisory";
    const VERSION: u32 = 1;
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
        let wanted = latest;
        let records: BTreeMap<String, Option<Arc<Advisory>>> = stream::iter(wanted)
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
