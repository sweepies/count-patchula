use super::*;
use rustovate_cache::ExtractionKey;
use rustovate_github::GraphqlPage;
use serde_json::json;
use sha2::{Digest, Sha256};

const RELEASES: &str = "query($owner:String!,$name:String!,$cursor:String){repository(owner:$owner,name:$name){releases(first:100,after:$cursor,orderBy:{field:CREATED_AT,direction:DESC}){nodes{tagName publishedAt isDraft isPrerelease}pageInfo{hasNextPage endCursor}}}}";
const TAGS: &str = "query($owner:String!,$name:String!,$cursor:String){repository(owner:$owner,name:$name){refs(refPrefix:\"refs/tags/\",first:100,after:$cursor,orderBy:{field:TAG_COMMIT_DATE,direction:DESC}){nodes{name target{__typename ... on Commit{oid committedDate} ... on Tag{target{__typename ... on Commit{oid committedDate} ... on Tag{target{__typename ... on Commit{oid committedDate}}}}}}}pageInfo{hasNextPage endCursor}}}}";

impl Client {
    pub(super) async fn github_versions(&self, source: Source, package: &str) -> Result<Catalog> {
        let (owner, name) = package
            .split_once('/')
            .filter(|(o, n)| !o.is_empty() && !n.is_empty() && !n.contains('/'))
            .ok_or(Error::Invalid)?;
        let source_name = if source == Source::GithubReleases {
            "github-releases"
        } else {
            "github-tags"
        };
        // GitHub GraphQL POSTs have no HTTP validators. A conditional REST head
        // probe gates the GraphQL refresh. Only changed heads request history.
        let endpoint = if source == Source::GithubReleases {
            "releases"
        } else {
            "tags"
        };
        let probe = self
            .github
            .get(&format!("repos/{package}/{endpoint}?per_page=1"))
            .await?;
        let fingerprint: String = Sha256::digest(&probe.body)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let key = ExtractionKey {
            repo: format!("source-head:{source_name}:{package}"),
            default_branch_commit: fingerprint,
            config_hash: "head-v1".into(),
        };
        let known = self.cache.releases(source_name, package).await?;
        if self.cache.extraction_get(&key).await?.is_none() {
            let mut variables = json!({"owner":owner,"name":name,"cursor":null});
            let (query, pointer) = if source == Source::GithubReleases {
                (RELEASES, "/repository/releases")
            } else {
                (TAGS, "/repository/refs")
            };
            let mut after = None;
            let mut visited = std::collections::HashSet::new();
            let mut complete = false;
            for _ in 0..1000 {
                let data = self.github.graphql(query, variables.clone()).await?;
                let page = GraphqlPage::<Value>::from_connection(&data, pointer)?;
                let mut releases = Vec::with_capacity(page.nodes.len());
                for node in &page.nodes {
                    if source == Source::GithubReleases {
                        if node["isDraft"].as_bool() == Some(true) {
                            continue;
                        }
                        releases.push(Release {
                            version: node["tagName"].as_str().ok_or(Error::Invalid)?.into(),
                            published_at: node["publishedAt"].as_str().map(str::to_owned),
                            digest: None,
                        });
                    } else {
                        let commit = peel(&node["target"]);
                        releases.push(Release {
                            version: node["name"].as_str().ok_or(Error::Invalid)?.into(),
                            published_at: commit
                                .and_then(|v| v["committedDate"].as_str())
                                .map(str::to_owned),
                            digest: commit.and_then(|v| v["oid"].as_str()).map(str::to_owned),
                        });
                    }
                }
                let stopped = self
                    .cache
                    .merge_release_page(source_name, package, &releases, after.as_deref())
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
            self.cache.extraction_put(&key, b"refreshed").await?;
        } else if known.newest.is_none() && !known.releases.is_empty() {
            return Err(Error::Invalid);
        }
        Ok(Catalog {
            versions: self
                .cache
                .releases(source_name, package)
                .await?
                .releases
                .into_iter()
                .map(Version::from)
                .collect(),
            dist_tags: BTreeMap::new(),
        })
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
    fn get(etag: Option<&str>, status: u16, value: Value, new_etag: &str) -> Interaction {
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
            value,
            Some(new_etag),
        )
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
    #[tokio::test]
    async fn changed_head_stops_at_known_item_then_warm_304_has_no_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("simulated.json");
        let cassette = Cassette {
            schema_version: 1,
            provenance: "Simulated from GitHub connection specification; independently written"
                .into(),
            interactions: vec![
                get(None, 200, json!([{"tag_name":"v3"}]), "a"),
                page(None, &["v3"], Some("one")),
                page(Some("one"), &["v2"], Some("two")),
                page(Some("two"), &["v1"], None),
                get(Some("a"), 200, json!([{"tag_name":"v4"}]), "b"),
                page(None, &["v4", "v3"], Some("unused-history")),
                get(Some("b"), 304, json!(null), "b"),
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
        assert_eq!(github.stats().await.requests, 7);
        assert_eq!(github.stats().await.not_modified, 1);
        replay.assert_finished().unwrap();
    }
}
