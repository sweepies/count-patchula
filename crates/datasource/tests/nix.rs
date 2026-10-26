//! Flake inputs through a stand-in for `nix flake prefetch`: nothing here starts
//! nix or reaches the network.
use patchula_cache::Cache;
use patchula_config::Source;
use patchula_datasource::{Client, Error, Flakes};
use patchula_http::{Cached, Exchange, Transport};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

struct NoNetwork;
impl Transport for NoNetwork {
    fn send(&self, _: reqwest::Request) -> Exchange<'_> {
        panic!("flake lookups must not reach the network directly")
    }
}

/// Answers every prefetch with one output and records the references.
struct Prefetch {
    output: &'static str,
    calls: Mutex<Vec<(String, Option<String>)>>,
}
impl Flakes for Prefetch {
    fn prefetch<'a>(
        &'a self,
        reference: &'a str,
        token: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + 'a>> {
        self.calls
            .lock()
            .unwrap()
            .push((reference.into(), token.map(str::to_owned)));
        Box::pin(async move { Ok(self.output.as_bytes().to_vec()) })
    }
}

async fn client(flakes: Option<Arc<Prefetch>>) -> Client {
    let cache = Cache::open(":memory:", patchula_cache::Options::default())
        .await
        .unwrap();
    let transport = Arc::new(NoNetwork);
    let github = patchula_github::Client::with_transport(
        "token",
        cache.clone(),
        patchula_github::Options::default(),
        transport.clone(),
    )
    .unwrap();
    let client = Client::new(
        github,
        Cached::new(cache.clone(), transport, 2).unwrap(),
        cache,
    );
    match flakes {
        Some(flakes) => client.with_nix(flakes),
        None => client,
    }
}

/// The catalog is the one revision nix resolved, dated by `lastModified` so
/// release-age rules apply, and reused for an hour.
#[tokio::test]
async fn a_flake_reference_resolves_to_its_locked_revision() {
    let flakes = Arc::new(Prefetch {
        output: r#"{"hash":"sha256-x","locked":{"lastModified":1792773339,"owner":"NixOS","repo":"nixpkgs","rev":"3ec636362e1816bc2f0888fa0fa2c69f1bc9e504","type":"github"},"original":{"owner":"NixOS","ref":"nixos-unstable","repo":"nixpkgs","type":"github"}}"#,
        calls: Mutex::new(Vec::new()),
    });
    let client = client(Some(flakes.clone())).await;
    let reference = "git+https://example.com/nixpkgs.git?ref=nixos-unstable";
    for _ in 0..2 {
        let catalog = client.lookup(Source::Nix, reference).await.unwrap();
        let [version] = catalog.versions.as_slice() else {
            panic!("one version expected: {:?}", catalog.versions);
        };
        assert_eq!(version.version, "3ec636362e1816bc2f0888fa0fa2c69f1bc9e504");
        assert_eq!(version.digest.as_deref(), Some(version.version.as_str()));
        assert_eq!(
            version.published_at.as_deref(),
            Some("2026-10-23T16:35:39Z")
        );
    }
    assert_eq!(flakes.calls.lock().unwrap().len(), 1);
}

/// An input without a revision, such as a tarball, is identified by its NAR
/// hash; without a tool store there is no lookup at all.
#[tokio::test]
async fn tarballs_use_their_nar_hash_and_lookups_need_the_store() {
    let flakes = Arc::new(Prefetch {
        output: r#"{"hash":"sha256-tarball","locked":{"type":"tarball","url":"https://example.com/a.tar.gz"}}"#,
        calls: Mutex::new(Vec::new()),
    });
    let catalog = client(Some(flakes))
        .await
        .lookup(Source::Nix, "tarball+https://example.com/a.tar.gz")
        .await
        .unwrap();
    assert_eq!(catalog.versions[0].version, "sha256-tarball");
    assert!(catalog.versions[0].published_at.is_none());
    assert!(matches!(
        client(None)
            .await
            .lookup(Source::Nix, "github:NixOS/nixpkgs")
            .await,
        Err(Error::NoNix)
    ));
}
