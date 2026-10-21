//! Offline large-packument replay. Inputs are public registry documents captured
//! separately; this executable never constructs a live transport or reads a token.
#![forbid(unsafe_code)]
use futures_util::{StreamExt, stream};
use patchula_cache::Cache;
use patchula_config::Source;
use patchula_datasource::Client;
use patchula_http::{Cached, Exchange, Response, Transport};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

const PACKAGES: [&str; 4] = ["vite", "typescript", "renovate", "effect"];
struct Replay {
    bodies: BTreeMap<String, Vec<u8>>,
    requests: AtomicUsize,
}
impl Transport for Replay {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        Box::pin(async move {
            assert_eq!(request.url().host_str(), Some("registry.npmjs.org"));
            assert_eq!(request.headers()["accept"], "application/json");
            assert!(!request.headers().contains_key("if-none-match"));
            self.requests.fetch_add(1, Ordering::Relaxed);
            Ok(Response {
                status: reqwest::StatusCode::OK,
                headers: reqwest::header::HeaderMap::from_iter([
                    (reqwest::header::ETAG, "offline-v1".parse().unwrap()),
                    (
                        reqwest::header::CACHE_CONTROL,
                        "max-age=3600".parse().unwrap(),
                    ),
                ]),
                body: self.bodies[request.url().path().trim_start_matches('/')].clone(),
            })
        })
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let input = PathBuf::from(args.next().ok_or("expected public document directory")?);
    let path = PathBuf::from(args.next().ok_or("expected initially absent cache path")?);
    if path.exists() {
        return Err("replay requires a cold cache path".into());
    }
    let transport = Arc::new(Replay {
        bodies: PACKAGES
            .into_iter()
            .map(|name| {
                Ok((
                    name.to_owned(),
                    std::fs::read(input.join(format!("{name}-full.json")))?,
                ))
            })
            .collect::<Result<_, std::io::Error>>()?,
        requests: AtomicUsize::new(0),
    });
    let cache = Cache::open(&path, Default::default()).await?;
    let client = Client::new(
        patchula_github::Client::with_transport(
            "offline",
            cache.clone(),
            Default::default(),
            transport.clone(),
        )?,
        Cached::new(cache.clone(), transport.clone(), 8)?,
        cache,
    );
    let mut expected = None;
    for state in ["cold", "warm1", "warm2"] {
        let start = Instant::now();
        let mut catalogs = stream::iter(PACKAGES)
            .map(|package| {
                let client = &client;
                async move {
                    Ok::<_, patchula_datasource::Error>((
                        package,
                        serde_json::to_value(client.lookup(Source::Npm, package).await?)?,
                    ))
                }
            })
            .buffer_unordered(4)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        let elapsed = start.elapsed();
        catalogs.sort_by_key(|(package, _)| *package);
        let fingerprint: String = Sha256::digest(serde_json::to_vec(&catalogs)?)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if let Some(expected) = &expected {
            assert_eq!(&catalogs, expected);
        } else {
            expected = Some(catalogs);
        }
        println!(
            "{state} lookup_ms={:.3} requests={} catalog_sha256={fingerprint}",
            elapsed.as_secs_f64() * 1000.0,
            transport.requests.load(Ordering::Relaxed)
        );
    }
    assert_eq!(transport.requests.load(Ordering::Relaxed), 4);
    Ok(())
}
