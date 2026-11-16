//! A fixed token's cache scope: an installation token is scoped by the reach
//! GitHub reports for it, every other token by the token itself, and a cached
//! blob counts only when it hashes to its SHA.
#![forbid(unsafe_code)]

use patchula_cache::{Cache, Options as CacheOptions};
use patchula_github::{Client, Options};
use patchula_http::{Exchange, Response, Transport};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

// `printf 'hi\n' | git hash-object --stdin`
const HI: &str = "45b983be36b73c0788dc9cbcb76cbb80fc7bb057";
const INSTALLATION_TOKEN: &str = "ghs_privateInstallationToken1";

/// GitHub answering an installation token's reach: `installation/repositories`
/// pages (each but the last, or every one when `endless`, links the next),
/// the GraphQL viewer and the one-commit contents probe. Any other GET is a
/// blob that counts in `other`.
struct Installation {
    pages: Vec<(u16, Value)>,
    endless: bool,
    viewer: (u16, Value),
    contents: u16,
    probed: Mutex<Vec<String>>,
    requests: AtomicUsize,
    other: AtomicUsize,
}

impl Installation {
    fn new(pages: Vec<(u16, Value)>) -> Self {
        Self {
            pages,
            endless: false,
            viewer: (200, json!({"data":{"viewer":{"databaseId":41}}})),
            contents: 200,
            probed: Mutex::new(Vec::new()),
            requests: AtomicUsize::new(0),
            other: AtomicUsize::new(0),
        }
    }
}

impl Transport for Installation {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        Box::pin(async move {
            self.requests.fetch_add(1, Ordering::SeqCst);
            let url = request.url().clone();
            let path = url.path().to_owned();
            let mut headers = reqwest::header::HeaderMap::new();
            let (status, body) = if path == "/installation/repositories" {
                // The reach is never read from the cache.
                assert!(request.headers().get("if-none-match").is_none());
                let page: usize = url
                    .query_pairs()
                    .find(|(name, _)| name == "page")
                    .map_or(1, |(_, value)| value.parse().unwrap());
                if self.endless || page < self.pages.len() {
                    headers.insert(
                        "link",
                        format!(
                            "<https://api.github.com/installation/repositories?per_page=100&page={}>; rel=\"next\"",
                            page + 1
                        )
                        .parse()
                        .unwrap(),
                    );
                }
                self.pages[(page - 1).min(self.pages.len() - 1)].clone()
            } else if path == "/graphql" {
                self.viewer.clone()
            } else if let Some(repository) = path
                .strip_prefix("/repos/")
                .and_then(|rest| rest.strip_suffix("/commits"))
            {
                self.probed.lock().unwrap().push(repository.to_owned());
                (self.contents, json!([{ "sha": HI }]))
            } else {
                self.other.fetch_add(1, Ordering::SeqCst);
                (200, json!({"encoding":"base64","content":"aGkK"}))
            };
            Ok(Response {
                status: reqwest::StatusCode::from_u16(status).unwrap(),
                headers,
                body: serde_json::to_vec(&body).unwrap(),
                stale: None,
            })
        })
    }
}

fn listed(id: u64, owner: u64, private: bool) -> Value {
    json!({"id":id,"full_name":format!("o{owner}/r{id}"),"private":private,"owner":{"id":owner}})
}

fn listing(selection: Option<&str>, total: usize, repositories: Vec<Value>) -> (u16, Value) {
    let mut body = json!({"total_count": total, "repositories": repositories});
    if let Some(selection) = selection {
        body["repository_selection"] = selection.into();
    }
    (200, body)
}

fn sha256_hex(text: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The scope a client keeps when its token's reach is not used.
fn token_scope(token: &str) -> String {
    sha256_hex(&format!("Bearer {token}"))
}

async fn identified(token: &str, transport: Arc<Installation>, cache: Cache) -> Client {
    Client::with_transport(token, cache, Options::default(), transport)
        .unwrap()
        .identify()
        .await
}

async fn memory() -> Cache {
    Cache::open(":memory:", CacheOptions::default())
        .await
        .unwrap()
}

#[tokio::test]
async fn an_installation_token_for_a_whole_account_is_scoped_by_its_bot_and_owner() {
    let transport = Arc::new(Installation::new(vec![listing(
        Some("all"),
        3,
        vec![listed(1, 7, false), listed(3, 7, true), listed(2, 7, true)],
    )]));
    let client = identified(INSTALLATION_TOKEN, transport.clone(), memory().await).await;
    assert_eq!(
        client.credential_scope(),
        sha256_hex("https://api.github.com/|app:41/owner:7/all")
    );
    // The probe reads a private repository's contents, the lowest id first.
    assert_eq!(*transport.probed.lock().unwrap(), ["o7/r2"]);
    // One listing page, the viewer and the probe.
    assert_eq!(transport.requests.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn an_installation_token_for_selected_repositories_is_scoped_by_their_ids() {
    let transport = Arc::new(Installation::new(vec![
        listing(
            Some("selected"),
            3,
            vec![listed(30, 7, false), listed(10, 8, false)],
        ),
        listing(Some("selected"), 3, vec![listed(20, 7, false)]),
    ]));
    let client = identified(INSTALLATION_TOKEN, transport.clone(), memory().await).await;
    assert_eq!(
        client.credential_scope(),
        sha256_hex(&format!(
            "https://api.github.com/|app:41/repos:{}",
            sha256_hex("10,20,30")
        ))
    );
    // With no private repository, any one serves as the probe.
    assert_eq!(*transport.probed.lock().unwrap(), ["o8/r10"]);
}

#[tokio::test]
async fn tokens_other_than_installation_tokens_keep_the_token_hash_without_a_request() {
    for token in [
        "ghp_privateTestCredential123",
        "github_pat_privateTestCredential123",
        "gho_privateTestCredential123",
        "ghu_privateTestCredential123",
        "plainPrivateToken",
    ] {
        let transport = Arc::new(Installation::new(vec![listing(
            Some("all"),
            1,
            vec![listed(1, 7, true)],
        )]));
        let client = identified(token, transport.clone(), memory().await).await;
        assert_eq!(client.credential_scope(), token_scope(token), "{token}");
        assert_eq!(transport.requests.load(Ordering::SeqCst), 0, "{token}");
    }
}

#[tokio::test]
async fn an_installation_token_keeps_the_token_hash_on_anything_unexpected() {
    let all = || listing(Some("all"), 1, vec![listed(1, 7, true)]);
    let cases: Vec<(&str, Installation)> = vec![
        (
            "a refused listing",
            Installation::new(vec![(403, json!({"message":"Forbidden"}))]),
        ),
        (
            "no repository_selection",
            Installation::new(vec![listing(None, 1, vec![listed(1, 7, true)])]),
        ),
        (
            "an unknown repository_selection",
            Installation::new(vec![listing(Some("some"), 1, vec![listed(1, 7, true)])]),
        ),
        (
            "a selected listing past the page cap",
            Installation {
                endless: true,
                ..Installation::new(vec![listing(Some("selected"), 1, vec![listed(1, 7, true)])])
            },
        ),
        (
            "a selected count below total_count",
            Installation::new(vec![listing(Some("selected"), 2, vec![listed(1, 7, true)])]),
        ),
        (
            "a selected listing naming a repository twice",
            Installation::new(vec![listing(
                Some("selected"),
                2,
                vec![listed(1, 7, true), listed(1, 7, true)],
            )]),
        ),
        (
            "an empty selected listing",
            Installation::new(vec![listing(Some("selected"), 0, vec![])]),
        ),
        (
            "an all listing with no repositories",
            Installation::new(vec![listing(Some("all"), 0, vec![])]),
        ),
        (
            "an all listing under two owners",
            Installation::new(vec![listing(
                Some("all"),
                2,
                vec![listed(1, 7, true), listed(2, 8, true)],
            )]),
        ),
        (
            "selections that change between pages",
            Installation::new(vec![
                listing(Some("selected"), 2, vec![listed(1, 7, true)]),
                listing(Some("all"), 2, vec![listed(2, 7, true)]),
            ]),
        ),
        (
            "a refused viewer",
            Installation {
                viewer: (401, json!({"message":"Bad credentials"})),
                ..Installation::new(vec![all()])
            },
        ),
        (
            "a viewer without an id",
            Installation {
                viewer: (200, json!({"data":{"viewer":{"databaseId":null}}})),
                ..Installation::new(vec![all()])
            },
        ),
        (
            "a token that cannot read contents",
            Installation {
                contents: 403,
                ..Installation::new(vec![all()])
            },
        ),
    ];
    for (case, transport) in cases {
        let client = identified(INSTALLATION_TOKEN, Arc::new(transport), memory().await).await;
        assert_eq!(
            client.credential_scope(),
            token_scope(INSTALLATION_TOKEN),
            "{case}"
        );
    }
}

#[tokio::test]
async fn installation_tokens_share_entries_only_with_the_exact_same_reach() {
    let directory = tempfile::tempdir().unwrap();
    let cache = || async {
        Cache::open(
            directory.path().join("cache.sqlite"),
            CacheOptions::default(),
        )
        .await
        .unwrap()
    };
    let repositories = || vec![listed(1, 7, true), listed(2, 7, true)];
    let all = || {
        Arc::new(Installation::new(vec![listing(
            Some("all"),
            2,
            repositories(),
        )]))
    };
    let path = format!("repos/o7/r1/git/blobs/{HI}");
    // A token minted for the next run reads what the last run's kept.
    let first = all();
    identified("ghs_firstRunToken", first.clone(), cache().await)
        .await
        .get_immutable_response(&path)
        .await
        .unwrap();
    assert_eq!(first.other.load(Ordering::SeqCst), 1);
    let second = all();
    let next = identified("ghs_secondRunToken", second.clone(), cache().await).await;
    assert_ne!(next.credential_scope(), token_scope("ghs_secondRunToken"));
    next.get_immutable_response(&path).await.unwrap();
    assert_eq!(second.other.load(Ordering::SeqCst), 0);
    // A token narrowed to the same repositories is another reach.
    let narrowed = Arc::new(Installation::new(vec![listing(
        Some("selected"),
        2,
        repositories(),
    )]));
    let client = identified("ghs_narrowedToken", narrowed.clone(), cache().await).await;
    assert_ne!(client.credential_scope(), next.credential_scope());
    client.get_immutable_response(&path).await.unwrap();
    assert_eq!(narrowed.other.load(Ordering::SeqCst), 1);
    // So is another App's bot on the same account.
    let other_app = Arc::new(Installation {
        viewer: (200, json!({"data":{"viewer":{"databaseId":42}}})),
        ..Installation::new(vec![listing(Some("all"), 2, repositories())])
    });
    let client = identified("ghs_otherAppToken", other_app.clone(), cache().await).await;
    assert_ne!(client.credential_scope(), next.credential_scope());
    client.get_immutable_response(&path).await.unwrap();
    assert_eq!(other_app.other.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_shared_client_is_returned_unchanged() {
    let transport = Arc::new(Installation::new(vec![listing(
        Some("all"),
        1,
        vec![listed(1, 7, true)],
    )]));
    let client = Client::with_transport(
        INSTALLATION_TOKEN,
        memory().await,
        Options::default(),
        transport,
    )
    .unwrap();
    let clone = client.clone();
    let client = client.identify().await;
    assert_eq!(client.credential_scope(), token_scope(INSTALLATION_TOKEN));
    assert_eq!(clone.credential_scope(), token_scope(INSTALLATION_TOKEN));
}

/// REST blobs whose first answer is `planted\n` and every later one the
/// real content of `HI`; GraphQL refuses every batch.
struct Planted {
    rest: AtomicUsize,
}
impl Transport for Planted {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        Box::pin(async move {
            // GraphQL refuses the batch, so blobs come through REST.
            let body = if request.url().path() == "/graphql" {
                json!({"data":null,"errors":[{"message":"Something went wrong"}]})
            } else if self.rest.fetch_add(1, Ordering::SeqCst) == 0 {
                json!({"encoding":"base64","content":"cGxhbnRlZAo="})
            } else {
                json!({"encoding":"base64","content":"aGkK"})
            };
            Ok(Response {
                status: reqwest::StatusCode::OK,
                headers: reqwest::header::HeaderMap::new(),
                body: serde_json::to_vec(&body).unwrap(),
                stale: None,
            })
        })
    }
}

#[tokio::test]
async fn a_cached_blob_that_does_not_hash_to_its_sha_is_read_again() {
    let transport = Arc::new(Planted {
        rest: AtomicUsize::new(0),
    });
    let client = Client::with_transport(
        "ghp_privateTestCredential123",
        memory().await,
        Options::default(),
        transport.clone(),
    )
    .unwrap();
    // The cache now holds `planted\n` under `HI`'s key, as a planted file
    // would.
    let path = format!("repos/acme/app/git/blobs/{HI}");
    client.get_immutable_response(&path).await.unwrap();
    let blobs = client.blobs("acme/app", &[HI.to_owned()]).await.unwrap();
    assert_eq!(blobs[HI], b"hi\n");
    assert_eq!(transport.rest.load(Ordering::SeqCst), 2);
    // The real blob replaced the planted one.
    client.blobs("acme/app", &[HI.to_owned()]).await.unwrap();
    assert_eq!(transport.rest.load(Ordering::SeqCst), 2);
}
