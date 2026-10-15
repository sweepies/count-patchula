use super::*;
use futures_util::{StreamExt, stream};
use rustovate_cache::ExtractionKey;
use rustovate_github::GraphqlPage;
use serde_json::json;
use sha2::{Digest, Sha256};

const RELEASES: &str = "query($owner:String!,$name:String!,$cursor:String){repository(owner:$owner,name:$name){releases(first:100,after:$cursor,orderBy:{field:CREATED_AT,direction:DESC}){nodes{tagName publishedAt isDraft isPrerelease}pageInfo{hasNextPage endCursor}}}}";
const GIT_OBJECT_FIELDS: &str = "oid __typename ... on Tag{tagger{date} target{__typename ... on Commit{oid} ... on Tag{target{__typename ... on Commit{oid}}}}}";
pub type TagDigests = BTreeMap<(String, String), std::result::Result<String, String>>;

#[derive(Serialize, Deserialize)]
struct ReleaseHead {
    tag_name: String,
    published_at: Option<String>,
    draft: bool,
    prerelease: bool,
}
fn compact_release_head(body: &[u8]) -> std::result::Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&serde_json::from_slice::<Vec<ReleaseHead>>(body)?)
}

impl Client {
    pub(super) async fn github_versions(&self, source: Source, package: &str) -> Result<Catalog> {
        if source == Source::GithubTags {
            if let Some(catalog) = self.mirror_catalog(source, package).await? {
                return Ok(catalog);
            }
            return self.github_tags(package).await;
        }
        self.release_catalogs(&[package.to_owned()])
            .await?
            .remove(package)
            .ok_or(Error::Invalid)?
    }

    /// Batch cold release pages; revalidate warm REST heads concurrently.
    /// Only semantic release fields affect the refresh fingerprint, so asset
    /// download counters cannot cause history refetches.
    pub async fn release_catalogs(
        &self,
        packages: &[String],
    ) -> Result<BTreeMap<String, Result<Catalog>>> {
        if self.use_versions_host {
            return self.mirror_release_catalogs(packages).await;
        }
        self.api_release_catalogs(packages).await
    }
    pub(super) async fn api_release_catalogs(
        &self,
        packages: &[String],
    ) -> Result<BTreeMap<String, Result<Catalog>>> {
        if packages.is_empty() {
            return Ok(BTreeMap::new());
        }
        let mut variables = serde_json::Map::new();
        let mut declarations = Vec::new();
        let mut fields = Vec::new();
        let mut pending = Vec::new();
        let mut warm = Vec::new();
        for package in packages {
            let (owner, name) = package
                .split_once('/')
                .filter(|(o, n)| !o.is_empty() && !n.is_empty() && !n.contains('/'))
                .ok_or(Error::Invalid)?;
            let key = ExtractionKey {
                repo: format!("source-head:github-releases:{package}"),
                default_branch_commit: String::new(),
                config_hash: "graphql-head-v1".into(),
            };
            let previous = self.cache.extraction_get(&key).await?;
            if previous.is_some() {
                warm.push((package.clone(), key, previous));
                continue;
            }
            let count = 100;
            let i = pending.len();
            declarations.push(format!("$o{i}:String!,$n{i}:String!"));
            variables.insert(format!("o{i}"), json!(owner));
            variables.insert(format!("n{i}"), json!(name));
            fields.push(format!("r{i}:repository(owner:$o{i},name:$n{i}){{releases(first:{count},orderBy:{{field:CREATED_AT,direction:DESC}}){{nodes{{tagName publishedAt isDraft isPrerelease}}pageInfo{{hasNextPage endCursor}}}}}}"));
            pending.push((package.clone(), key, previous, count));
        }
        let cold = async {
            if pending.is_empty() {
                return Ok::<BTreeMap<String, Result<Catalog>>, Error>(BTreeMap::new());
            }
            let query = format!("query({}){{{}}}", declarations.join(","), fields.join(" "));
            let data = self
                .github
                .graphql(&query, Value::Object(variables))
                .await?;
            Ok(stream::iter(pending.into_iter().enumerate())
                .map(|(i, (package, key, previous, count))| {
                    let data = &data;
                    async move {
                        let result = self
                            .release_catalog(
                                &package,
                                key,
                                previous,
                                count,
                                &data[format!("r{i}")]["releases"],
                            )
                            .await;
                        (package, result)
                    }
                })
                .buffer_unordered(self.github.concurrency())
                .collect()
                .await)
        };
        let warm = stream::iter(warm).map(|(package, key, previous)| async move {
            let result = async {
                let response = self.github.get_derived(&format!("repos/{package}/releases?per_page=1"), "github-release-head-v1", compact_release_head).await?;
                let nodes: Vec<Value> = serde_json::from_slice(&response.body)?;
                let nodes: Vec<_> = nodes.iter().map(|node| json!({"tagName":node["tag_name"],"publishedAt":node["published_at"],"isDraft":node["draft"],"isPrerelease":node["prerelease"]})).collect();
                self.release_catalog(&package, key, previous, 1, &json!({"nodes":nodes,"pageInfo":{"hasNextPage":true,"endCursor":null}})).await
            }.await;
            (package, result)
        }).buffer_unordered(self.github.concurrency()).collect::<BTreeMap<_,_>>();
        let (cold, warm) = tokio::join!(cold, warm);
        let mut result = cold?;
        result.extend(warm);
        Ok(result)
    }

    async fn release_catalog(
        &self,
        package: &str,
        key: ExtractionKey,
        previous: Option<Vec<u8>>,
        count: usize,
        connection: &Value,
    ) -> Result<Catalog> {
        let first = GraphqlPage::<Value>::from_connection(connection, "")?;
        let fingerprint = serde_json::to_vec(&first.nodes.first().map(|node| {
            json!([
                node["tagName"],
                node["publishedAt"],
                node["isDraft"],
                node["isPrerelease"]
            ])
        }))?;
        if previous.as_deref() != Some(fingerprint.as_slice()) {
            let (owner, name) = package.split_once('/').ok_or(Error::Invalid)?;
            let mut variables = json!({"owner":owner,"name":name,"cursor":null});
            let mut first_page = (count == 100).then_some(first);
            let mut after = None;
            let mut visited = std::collections::HashSet::new();
            let mut complete = false;
            for _ in 0..1000 {
                let page = if let Some(first) = first_page.take() {
                    first
                } else {
                    GraphqlPage::<Value>::from_connection(
                        &self.github.graphql(RELEASES, variables.clone()).await?,
                        "/repository/releases",
                    )?
                };
                let mut releases = Vec::with_capacity(page.nodes.len());
                for node in &page.nodes {
                    if node["isDraft"].as_bool() == Some(true) {
                        continue;
                    }
                    releases.push(Release {
                        version: node["tagName"].as_str().ok_or(Error::Invalid)?.into(),
                        published_at: node["publishedAt"].as_str().map(str::to_owned),
                        digest: None,
                    });
                }
                let stopped = self
                    .cache
                    .merge_release_page("github-releases", package, &releases, after.as_deref())
                    .await?;
                if stopped || !page.has_next_page {
                    complete = true;
                    break;
                }
                after = releases.last().map(|r| r.version.clone()).or(after);
                let cursor = page
                    .end_cursor
                    .filter(|c| !c.is_empty())
                    .ok_or(Error::Invalid)?;
                if !visited.insert(cursor.clone()) {
                    return Err(Error::Invalid);
                }
                variables["cursor"] = Value::String(cursor);
            }
            if !complete {
                return Err(Error::Invalid);
            }
            self.cache.extraction_put(&key, &fingerprint).await?;
        }
        let mut catalog = Catalog {
            versions: self
                .cache
                .releases("github-releases", package)
                .await?
                .releases
                .into_iter()
                .map(Version::from)
                .collect(),
            dist_tags: BTreeMap::new(),
        };
        if package == "actions/node-versions" {
            // Stable setup-node targets remain usable during a Node-index
            // outage. An explicit LTS policy fails closed without this evidence.
            if let Ok(node) = self.node().await {
                catalog.dist_tags = node.dist_tags;
            }
        }
        Ok(catalog)
    }

    async fn github_tags(&self, package: &str) -> Result<Catalog> {
        // Releases can be published after their tags. Revalidate release history
        // independently of the immutable object/namespace caches on every run.
        let releases = self
            .api_release_catalogs(&[package.to_owned()])
            .await?
            .remove(package)
            .ok_or(Error::Invalid)??;
        // Unlike a cursor connection, matching-refs returns the namespace in
        // one read. Hydrate new/retargeted refs through bounded parallel object
        // queries. This also notices changes below the newest tag.
        let mut refs: Vec<Value> = self
            .github
            .rest_pages(&format!("repos/{package}/git/matching-refs/tags/"), |_| {
                false
            })
            .await?;
        // Some repositories return an empty matching-refs namespace even while
        // the tags API exposes version tags. Do not cache that partial view as
        // a complete version catalog. Both APIs supply immutable object SHAs.
        if refs.is_empty() {
            refs = self
                .github
                .rest_pages(&format!("repos/{package}/tags?per_page=100"), |_| false)
                .await?
                .into_iter()
                .map(|tag: Value| {
                    let name = tag["name"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or(Error::Invalid)?;
                    let sha = tag["commit"]["sha"].as_str().ok_or(Error::Invalid)?;
                    Ok(json!({"ref": format!("refs/tags/{name}"), "object": {"sha": sha}}))
                })
                .collect::<Result<_>>()?;
        }
        let identity: Vec<_> = refs
            .iter()
            .map(|reference| (&reference["ref"], &reference["object"]["sha"]))
            .collect();
        let fingerprint: String = Sha256::digest(serde_json::to_vec(&identity)?)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let catalog_key = ExtractionKey {
            repo: format!("source-tags:{package}"),
            default_branch_commit: fingerprint,
            config_hash: "tag-catalog-v2".into(),
        };
        if let Some(bytes) = self.cache.extraction_get(&catalog_key).await? {
            return Ok(with_release_dates(
                serde_json::from_slice(&bytes)?,
                &releases,
            ));
        }
        let mut versions = Vec::with_capacity(refs.len());
        let mut targets: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for reference in refs {
            let name = reference["ref"]
                .as_str()
                .and_then(|name| name.strip_prefix("refs/tags/"))
                .ok_or(Error::Invalid)?;
            let sha = reference["object"]["sha"].as_str().ok_or(Error::Invalid)?;
            targets.entry(sha.into()).or_default().push(name.into());
        }
        let mut missing = Vec::new();
        for (sha, names) in targets {
            let key = ExtractionKey {
                repo: format!("tag-object:{package}"),
                default_branch_commit: sha.clone(),
                config_hash: "tag-object-v2".into(),
            };
            if let Some(bytes) = self.cache.extraction_get(&key).await? {
                let target: Version = serde_json::from_slice(&bytes)?;
                versions.extend(names.into_iter().map(|version| Version {
                    version,
                    ..target.clone()
                }));
            } else {
                missing.push((sha, names, key));
            }
        }
        let (owner, name) = package.split_once('/').ok_or(Error::Invalid)?;
        let pages = stream::iter(missing.chunks(100).map(<[_]>::to_vec))
            .map(|chunk| async move {
                // Resolve immutable Git objects under one repository, rather than
                // global Ref nodes which repeat repository access/resolution work.
                let mut variables = serde_json::Map::from_iter([
                    ("owner".into(), json!(owner)),
                    ("name".into(), json!(name)),
                ]);
                let mut declarations = vec!["$owner:String!,$name:String!".to_owned()];
                let mut fields = Vec::new();
                for (i, (sha, _, _)) in chunk.iter().enumerate() {
                    declarations.push(format!("$s{i}:GitObjectID!"));
                    variables.insert(format!("s{i}"), json!(sha));
                    fields.push(format!("t{i}:object(oid:$s{i}){{{GIT_OBJECT_FIELDS}}}"));
                }
                let query = format!(
                    "query({}){{repository(owner:$owner,name:$name){{{}}}}}",
                    declarations.join(","),
                    fields.join(" ")
                );
                let data = self
                    .github
                    .graphql(&query, Value::Object(variables))
                    .await?;
                let mut result = Vec::new();
                for (i, (sha, names, key)) in chunk.iter().enumerate() {
                    let node = &data["repository"][format!("t{i}")];
                    if node["oid"] != *sha {
                        return Err(Error::Invalid);
                    }
                    let commit = peel(node).ok_or(Error::Invalid)?;
                    let target = Version::from(Release {
                        version: String::new(),
                        published_at: node["tagger"]["date"].as_str().map(str::to_owned),
                        digest: commit["oid"].as_str().map(str::to_owned),
                    });
                    self.cache
                        .extraction_put(key, &serde_json::to_vec(&target)?)
                        .await?;
                    result.extend(names.iter().map(|version| Version {
                        version: version.clone(),
                        ..target.clone()
                    }));
                }
                Ok::<_, Error>(result)
            })
            .buffer_unordered(self.github.concurrency())
            .collect::<Vec<_>>()
            .await;
        for page in pages {
            versions.extend(page?);
        }
        versions.sort_by(|a, b| (&b.published_at, &b.version).cmp(&(&a.published_at, &a.version)));
        let catalog = Catalog {
            versions,
            dist_tags: BTreeMap::new(),
        };
        self.cache
            .extraction_put(&catalog_key, &serde_json::to_vec(&catalog)?)
            .await?;
        Ok(with_release_dates(catalog, &releases))
    }
    /// Refresh only requested mutable tag targets while version lists are reused.
    /// One missing/inaccessible object falls back to its ordinary ref lookup.
    pub async fn fresh_tag_digests(&self, keys: &[(String, String)]) -> TagDigests {
        let mut results = BTreeMap::new();
        for chunk in keys.chunks(64) {
            let mut declarations = Vec::new();
            let mut fields = Vec::new();
            let mut variables = serde_json::Map::new();
            let mut repositories = BTreeMap::<&str, Vec<usize>>::new();
            let mut locations = BTreeMap::new();
            for (i, (package, _)) in chunk.iter().enumerate() {
                repositories.entry(package).or_default().push(i);
            }
            for (r, (package, indices)) in repositories.into_iter().enumerate() {
                let Some((owner, name)) = package.split_once('/') else {
                    continue;
                };
                declarations.push(format!("$o{r}:String!,$n{r}:String!"));
                variables.insert(format!("o{r}"), json!(owner));
                variables.insert(format!("n{r}"), json!(name));
                let mut objects = Vec::new();
                for i in indices {
                    declarations.push(format!("$e{i}:String!"));
                    variables.insert(format!("e{i}"), json!(format!("refs/tags/{}", chunk[i].1)));
                    objects.push(format!(
                        "t{i}:object(expression:$e{i}){{{GIT_OBJECT_FIELDS}}}"
                    ));
                    locations.insert(i, r);
                }
                fields.push(format!(
                    "r{r}:repository(owner:$o{r},name:$n{r}){{{}}}",
                    objects.join(" ")
                ));
            }
            let query = format!(
                "query FreshTagDigests({}){{{}}}",
                declarations.join(","),
                fields.join(" ")
            );
            let data = self.github.graphql(&query, Value::Object(variables)).await;
            for (i, (package, reference)) in chunk.iter().enumerate() {
                let sha = data
                    .as_ref()
                    .ok()
                    .and_then(|data| {
                        locations
                            .get(&i)
                            .and_then(|r| peel(&data[format!("r{r}")][format!("t{i}")]))
                    })
                    .and_then(|commit| commit["oid"].as_str())
                    .filter(|sha| {
                        sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
                    });
                let result = if let Some(sha) = sha {
                    Ok(sha.into())
                } else {
                    self.digest(package, reference)
                        .await
                        .map_err(|error| error.to_string())
                };
                results.insert((package.clone(), reference.clone()), result);
            }
        }
        results
    }
    /// Resolve moving refs separately: tags may be retargeted without a new head
    /// and branches such as main are not version releases. Conditional every run.
    pub async fn digest(&self, package: &str, reference: &str) -> Result<String> {
        if reference.is_empty() || reference.contains(['?', '#']) {
            return Err(Error::Invalid);
        }
        let mut url =
            reqwest::Url::parse(&format!("https://api.github.com/repos/{package}/commits/"))
                .map_err(|_| Error::Invalid)?;
        url.path_segments_mut()
            .map_err(|_| Error::Invalid)?
            .pop_if_empty()
            .push(reference);
        let response = self.github.get(url.as_str()).await?;
        let json: Value = serde_json::from_slice(&response.body)?;
        json["sha"]
            .as_str()
            .map(str::to_owned)
            .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or(Error::Invalid)
    }
}
fn with_release_dates(mut tags: Catalog, releases: &Catalog) -> Catalog {
    let dates: BTreeMap<_, _> = releases
        .versions
        .iter()
        .map(|v| (v.version.as_str(), &v.published_at))
        .collect();
    for tag in &mut tags.versions {
        if let Some(date) = dates.get(tag.version.as_str()) {
            tag.published_at = (*date).clone();
        }
    }
    tags
}
fn peel(value: &Value) -> Option<&Value> {
    let mut value = value;
    for _ in 0..10 {
        if value["__typename"] == "Commit" {
            return Some(value);
        }
        value = value.get("target")?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustovate_testkit::{
        Body, Cassette, Interaction, Options, RecordedRequest, RecordedResponse, Replay,
    };
    use std::{collections::BTreeMap, sync::Arc};

    #[tokio::test]
    async fn tag_age_uses_release_then_tagger_never_commit_and_refreshes_after_publication() {
        use rustovate_http::{Exchange, Response, Transport};
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Dates {
            phase: AtomicUsize,
            hydrations: AtomicUsize,
        }
        impl Transport for Dates {
            fn send(&self, request: reqwest::Request) -> Exchange<'_> {
                Box::pin(async move {
                    let phase = self.phase.load(Ordering::SeqCst);
                    let released = if phase == 0 {
                        vec![]
                    } else {
                        vec![
                            json!({"tag_name":"v1.0.0","published_at":if phase == 1 {"2026-10-14T00:00:00Z"}else{"2026-10-15T00:00:00Z"},"draft":false,"prerelease":false}),
                        ]
                    };
                    let connection = || json!({"nodes":released.iter().map(|r|json!({"tagName":r["tag_name"],"publishedAt":r["published_at"],"isDraft":false,"isPrerelease":false})).collect::<Vec<_>>(),"pageInfo":{"hasNextPage":false,"endCursor":null}});
                    let body = if request.url().path().ends_with("/releases") {
                        json!(released)
                    } else if request.url().path().contains("matching-refs") {
                        json!((0..3).map(|i|json!({"ref":format!("refs/tags/v{}.0.0",i+1),"object":{"sha":format!("{i:040x}")}})).collect::<Vec<_>>())
                    } else {
                        let body: Value =
                            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap())
                                .unwrap();
                        let query = body["query"].as_str().unwrap();
                        if query.contains("r0:repository") {
                            json!({"data":{"r0":{"releases":connection()}}})
                        } else if query.contains("releases(") {
                            json!({"data":{"repository":{"releases":connection()}}})
                        } else {
                            assert!(!query.contains("committedDate"));
                            assert!(query.contains("tagger{date}"));
                            self.hydrations.fetch_add(1, Ordering::SeqCst);
                            json!({"data":{"repository":{
                                "t0":{"oid":format!("{:040x}",0),"__typename":"Tag","tagger":{"date":"2026-10-10T00:00:00Z"},"target":{"__typename":"Commit","oid":"a".repeat(40),"committedDate":"2000-01-01T00:00:00Z"}},
                                "t1":{"oid":format!("{:040x}",1),"__typename":"Tag","tagger":{"date":"2026-10-11T00:00:00Z"},"target":{"__typename":"Tag","target":{"__typename":"Commit","oid":"b".repeat(40)}}},
                                "t2":{"oid":format!("{:040x}",2),"__typename":"Commit","committedDate":"2000-01-01T00:00:00Z"}
                            }}})
                        }
                    };
                    Ok(Response {
                        status: reqwest::StatusCode::OK,
                        headers: Default::default(),
                        body: serde_json::to_vec(&body).unwrap(),
                    })
                })
            }
        }
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        // Old immutable-object entries must not perpetuate commit-date ages.
        cache
            .extraction_put(
                &ExtractionKey {
                    repo: "tag-object:example/dates".into(),
                    default_branch_commit: format!("{:040x}", 0),
                    config_hash: "tag-object-v1".into(),
                },
                &serde_json::to_vec(&Version::from(Release {
                    version: String::new(),
                    published_at: Some("2000-01-01T00:00:00Z".into()),
                    digest: Some("a".repeat(40)),
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        let transport = Arc::new(Dates {
            phase: AtomicUsize::new(0),
            hydrations: AtomicUsize::new(0),
        });
        let github = rustovate_github::Client::with_transport(
            "offline",
            cache.clone(),
            Default::default(),
            transport.clone(),
        )
        .unwrap();
        let client = Client::new(
            github,
            Cached::new(cache.clone(), transport.clone(), 2).unwrap(),
            cache,
        );
        for (phase, date) in [
            (0, "2026-10-10T00:00:00Z"),
            (1, "2026-10-14T00:00:00Z"),
            (2, "2026-10-15T00:00:00Z"),
        ] {
            transport.phase.store(phase, Ordering::SeqCst);
            let tags = client
                .lookup(Source::GithubTags, "example/dates")
                .await
                .unwrap();
            assert_eq!(
                tags.versions
                    .iter()
                    .find(|v| v.version == "v1.0.0")
                    .unwrap()
                    .published_at
                    .as_deref(),
                Some(date)
            );
            assert_eq!(
                tags.versions
                    .iter()
                    .find(|v| v.version == "v2.0.0")
                    .unwrap()
                    .published_at
                    .as_deref(),
                Some("2026-10-11T00:00:00Z")
            );
            assert_eq!(
                tags.versions
                    .iter()
                    .find(|v| v.version == "v3.0.0")
                    .unwrap()
                    .published_at,
                None
            );
        }
        assert_eq!(transport.hydrations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn empty_matching_refs_falls_back_without_poisoning_warm_catalogs() {
        use rustovate_http::{Exchange, Response, Transport};
        struct Tags;
        impl Transport for Tags {
            fn send(&self, request: reqwest::Request) -> Exchange<'_> {
                Box::pin(async move {
                    let body = match request.url().path() {
                        "/repos/example/empty/git/matching-refs/tags/" => json!([]),
                        "/repos/example/empty/tags" => {
                            json!([{"name":"v2.0.0","commit":{"sha":"a".repeat(40)}}])
                        }
                        "/repos/example/empty/releases" => json!([]),
                        "/graphql"
                            if serde_json::from_slice::<Value>(
                                request.body().unwrap().as_bytes().unwrap(),
                            )
                            .unwrap()["query"]
                                .as_str()
                                .unwrap()
                                .contains("releases(") =>
                        {
                            json!({"data":{"r0":{"releases":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}})
                        }
                        "/graphql" => {
                            json!({"data":{"repository":{"t0":{"__typename":"Commit","oid":"a".repeat(40),"committedDate":"2026-10-01T00:00:00Z"}}}})
                        }
                        path => panic!("unexpected request {path}"),
                    };
                    Ok(Response {
                        status: reqwest::StatusCode::OK,
                        headers: Default::default(),
                        body: serde_json::to_vec(&body).unwrap(),
                    })
                })
            }
        }
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let transport = Arc::new(Tags);
        let github = rustovate_github::Client::with_transport(
            "offline",
            cache.clone(),
            Default::default(),
            transport.clone(),
        )
        .unwrap();
        let client = Client::new(
            github,
            Cached::new(cache.clone(), transport, 2).unwrap(),
            cache,
        );
        for _ in 0..2 {
            let catalog = client
                .lookup(Source::GithubTags, "example/empty")
                .await
                .unwrap();
            assert_eq!(catalog.versions.len(), 1);
            assert_eq!(catalog.versions[0].version, "v2.0.0");
            assert_eq!(
                catalog.versions[0].digest.as_deref(),
                Some("a".repeat(40).as_str())
            );
        }
    }

    #[tokio::test]
    async fn tag_hydration_is_parallel_bounded_and_only_refetches_changed_targets() {
        use rustovate_http::{Exchange, Response, Transport};
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Tags {
            step: AtomicUsize,
            active: AtomicUsize,
            peak: AtomicUsize,
            queries: AtomicUsize,
        }
        impl Transport for Tags {
            fn send(&self, request: reqwest::Request) -> Exchange<'_> {
                Box::pin(async move {
                    let step = self.step.load(Ordering::SeqCst);
                    let sha =
                        |i: usize| format!("{:040x}", if step >= 2 && i == 0 { 9999 } else { i });
                    let body = if request.url().path().ends_with("/releases") {
                        json!([])
                    } else if request.method() == reqwest::Method::GET {
                        assert_eq!(
                            request.url().path(),
                            "/repos/example/tags/git/matching-refs/tags/"
                        );
                        let count = if step == 3 { 200 } else { 201 };
                        json!((0..count).map(|i| json!({"ref":format!("refs/tags/v1.0.{i}"),"node_id":format!("ref-{i}"),"object":{"sha":sha(i)}})).collect::<Vec<_>>())
                    } else {
                        let body: Value =
                            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap())
                                .unwrap();
                        if body["query"].as_str().unwrap().contains("releases(") {
                            return Ok(Response { status: reqwest::StatusCode::OK, headers: Default::default(), body: serde_json::to_vec(&json!({"data":{"r0":{"releases":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}})).unwrap() });
                        }
                        assert!(body["query"].as_str().unwrap().contains("object(oid:$s0)"));
                        let targets: Vec<_> = body["variables"]
                            .as_object()
                            .unwrap()
                            .iter()
                            .filter(|(name, _)| name.starts_with('s'))
                            .collect();
                        assert!(targets.len() <= 100);
                        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                        self.peak.fetch_max(active, Ordering::SeqCst);
                        self.queries.fetch_add(1, Ordering::SeqCst);
                        tokio::task::yield_now().await;
                        self.active.fetch_sub(1, Ordering::SeqCst);
                        json!({"data":{"repository":targets.into_iter().map(|(name, oid)| {
                            (format!("t{}", name.strip_prefix('s').unwrap()), json!({"__typename":"Commit","oid":oid,"committedDate":"2026-10-01T00:00:00Z"}))
                        }).collect::<serde_json::Map<_,_>>()}})
                    };
                    Ok(Response {
                        status: reqwest::StatusCode::OK,
                        headers: Default::default(),
                        body: serde_json::to_vec(&body).unwrap(),
                    })
                })
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache.sqlite"), Default::default())
            .await
            .unwrap();
        let transport = Arc::new(Tags {
            step: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            queries: AtomicUsize::new(0),
        });
        let github = rustovate_github::Client::with_transport(
            "offline",
            cache.clone(),
            rustovate_github::Options {
                concurrency: 2,
                ..Default::default()
            },
            transport.clone(),
        )
        .unwrap();
        let client = Client::new(
            github,
            Cached::new(cache.clone(), transport.clone(), 2).unwrap(),
            cache,
        );
        let cold = client
            .lookup(Source::GithubTags, "example/tags")
            .await
            .unwrap();
        assert_eq!(cold.versions.len(), 201);
        assert_eq!(transport.queries.load(Ordering::SeqCst), 3);
        assert_eq!(transport.peak.load(Ordering::SeqCst), 2);
        transport.step.store(1, Ordering::SeqCst);
        let warm = client
            .lookup(Source::GithubTags, "example/tags")
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(cold).unwrap(),
            serde_json::to_value(warm).unwrap()
        );
        assert_eq!(
            transport.queries.load(Ordering::SeqCst),
            3,
            "even a 200 namespace needs no unchanged hydration"
        );
        transport.step.store(2, Ordering::SeqCst);
        let changed = client
            .lookup(Source::GithubTags, "example/tags")
            .await
            .unwrap();
        assert_eq!(transport.queries.load(Ordering::SeqCst), 4);
        assert_eq!(
            changed
                .versions
                .iter()
                .find(|v| v.version == "v1.0.0")
                .unwrap()
                .digest,
            Some(format!("{:040x}", 9999))
        );
        transport.step.store(3, Ordering::SeqCst);
        let deleted = client
            .lookup(Source::GithubTags, "example/tags")
            .await
            .unwrap();
        assert_eq!(deleted.versions.len(), 200);
        assert_eq!(transport.queries.load(Ordering::SeqCst), 4);
    }
    fn exchange(
        method: &str,
        url: &str,
        body: Body,
        headers: BTreeMap<String, Vec<String>>,
        status: u16,
        response: Value,
        etag: Option<&str>,
    ) -> Interaction {
        Interaction {
            request: RecordedRequest {
                method: method.into(),
                url: url.into(),
                headers,
                body,
            },
            response: RecordedResponse {
                status,
                headers: etag
                    .map(|v| BTreeMap::from([("etag".into(), vec![v.into()])]))
                    .unwrap_or_default(),
                body: Body::Json(response),
            },
        }
    }
    fn page(cursor: Option<&str>, tags: &[&str], next: Option<&str>) -> Interaction {
        let nodes:Vec<_>=tags.iter().map(|tag|json!({"tagName":tag,"publishedAt":"2026-01-01T00:00:00Z","isDraft":false,"isPrerelease":false})).collect();
        exchange(
            "POST",
            "https://api.github.com/graphql",
            Body::Json(
                json!({"query":RELEASES,"variables":{"owner":"example","name":"spec","cursor":cursor}}),
            ),
            BTreeMap::from([
                ("accept".into(), vec!["application/vnd.github+json".into()]),
                ("x-github-api-version".into(), vec!["2022-11-28".into()]),
                ("content-type".into(), vec!["application/json".into()]),
            ]),
            200,
            json!({"data":{"repository":{"releases":{"nodes":nodes,"pageInfo":{"hasNextPage":next.is_some(),"endCursor":next}}}}}),
            None,
        )
    }
    fn warm_head(tag: &str, etag: Option<&str>, response_etag: &str, status: u16) -> Interaction {
        let mut headers = BTreeMap::from([
            ("accept".into(), vec!["application/vnd.github+json".into()]),
            ("x-github-api-version".into(), vec!["2022-11-28".into()]),
        ]);
        if let Some(etag) = etag {
            headers.insert("if-none-match".into(), vec![etag.into()]);
        }
        exchange(
            "GET",
            "https://api.github.com/repos/example/spec/releases?per_page=1",
            Body::Text(String::new()),
            headers,
            status,
            json!([{"tag_name":tag,"published_at":"2026-01-01T00:00:00Z","draft":false,"prerelease":false,"assets":[{"download_count":response_etag.len()}]}]),
            Some(response_etag),
        )
    }
    fn head(count: usize, tags: &[&str], next: Option<&str>) -> Interaction {
        let mut interaction = page(None, tags, next);
        let query = format!(
            "query($o0:String!,$n0:String!){{r0:repository(owner:$o0,name:$n0){{releases(first:{count},orderBy:{{field:CREATED_AT,direction:DESC}}){{nodes{{tagName publishedAt isDraft isPrerelease}}pageInfo{{hasNextPage endCursor}}}}}}}} "
        );
        interaction.request.body =
            Body::Json(json!({"query":query.trim_end(),"variables":{"o0":"example","n0":"spec"}}));
        let Body::Json(ref mut value) = interaction.response.body else {
            unreachable!()
        };
        value["data"]["r0"] = value["data"]["repository"].take();
        value["data"].as_object_mut().unwrap().remove("repository");
        interaction
    }
    #[tokio::test]
    async fn changed_release_head_refreshes_once_and_warm_counters_do_not_fetch_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("simulated.json");
        let cassette = Cassette {
            schema_version: 1,
            provenance: "Simulated from GitHub connection specification; independently written"
                .into(),
            interactions: vec![
                head(100, &["v3"], Some("one")),
                page(Some("one"), &["v2"], Some("two")),
                page(Some("two"), &["v1"], None),
                warm_head("v3", None, "a", 200),
                warm_head("v3", Some("a"), "changed-counters", 200),
                warm_head("v4", Some("changed-counters"), "b", 200),
                page(None, &["v4", "v3"], Some("unused-history")),
                warm_head("v4", Some("b"), "b", 304),
            ],
        };
        rustovate_testkit::save(&path, &cassette, &Default::default()).unwrap();
        let replay = Arc::new(Replay::open(&path, Options::default()).unwrap());
        let cache = Cache::open(dir.path().join("cache.sqlite"), Default::default())
            .await
            .unwrap();
        let github = rustovate_github::Client::with_transport(
            "offline",
            cache.clone(),
            Default::default(),
            replay.clone(),
        )
        .unwrap();
        let client = Client::new(
            github.clone(),
            Cached::new(cache.clone(), replay.clone(), 4).unwrap(),
            cache,
        );
        assert_eq!(
            client
                .lookup(Source::GithubReleases, "example/spec")
                .await
                .unwrap()
                .versions
                .len(),
            3
        );
        let repeated = client
            .lookup(Source::GithubReleases, "example/spec")
            .await
            .unwrap();
        assert_eq!(repeated.versions.len(), 3);
        assert_eq!(
            github.stats().await.requests,
            4,
            "an unchanged semantic head must not fetch history"
        );
        assert_eq!(
            client
                .lookup(Source::GithubReleases, "example/spec")
                .await
                .unwrap()
                .versions
                .len(),
            3
        );
        assert_eq!(
            github.stats().await.requests,
            5,
            "changed download counters must not fetch history"
        );
        assert_eq!(
            client
                .lookup(Source::GithubReleases, "example/spec")
                .await
                .unwrap()
                .versions
                .len(),
            4
        );
        assert_eq!(
            client
                .lookup(Source::GithubReleases, "example/spec")
                .await
                .unwrap()
                .versions
                .len(),
            4
        );
        assert_eq!(github.stats().await.requests, 8);
        assert_eq!(github.stats().await.not_modified, 1);
        replay.assert_finished().unwrap();
    }
}
