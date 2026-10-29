//! Public registry v2, Terraform, PyPI and Helm endpoints recorded by this implementation; replay is offline.
use patchula_cache::Cache;
use patchula_config::Source;
use patchula_datasource::Client;
use patchula_http::{Cached, Network};
use std::{path::Path, sync::Arc};

#[tokio::test]
async fn public_registry_catalogs_and_index_digest() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/datasource/registries.json");
    let transport =
        patchula_testkit::from_env(&path, Default::default(), || Ok(Arc::new(Network::new()?)))
            .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::open(dir.path().join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let github = patchula_github::Client::with_transport(
        "offline",
        cache.clone(),
        Default::default(),
        transport.clone(),
    )
    .unwrap();
    let http = Cached::new(cache.clone(), transport, 4).unwrap();
    let client = Client::new(github, http.clone(), cache).with_versions_host(false);
    let packages = [
        (Source::Docker, "library/hello-world"),
        (Source::Oci, "ghcr.io/stefanprodan/charts/podinfo"),
        (Source::Terraform, "hashicorp/random"),
        (Source::Terraform, "terraform-aws-modules/iam/aws"),
        (Source::Pypi, "six"),
        (
            Source::Helm,
            "https://stefanprodan.github.io/podinfo|podinfo",
        ),
    ];
    let start = std::time::Instant::now();
    for (source, package) in packages {
        assert!(
            !client
                .lookup(source, package)
                .await
                .unwrap()
                .versions
                .is_empty(),
            "{package}"
        );
    }
    assert!(!client.pypi_simple("six").await.unwrap().versions.is_empty());
    let digest = client
        .registry_digest("library/hello-world", "latest")
        .await
        .unwrap();
    assert!(digest.starts_with("sha256:"));
    let oci = client
        .registry_digest("ghcr.io/stefanprodan/charts/podinfo", "6.7.1")
        .await
        .unwrap();
    assert!(oci.starts_with("sha256:"));
    let cold = start.elapsed();
    let before = http.stats();
    let start = std::time::Instant::now();
    for (source, package) in packages {
        assert!(
            !client
                .lookup(source, package)
                .await
                .unwrap()
                .versions
                .is_empty()
        );
    }
    assert_eq!(
        digest,
        client
            .registry_digest("library/hello-world", "latest")
            .await
            .unwrap()
    );
    eprintln!(
        "registries cold={cold:?} warm={:?} cold_requests={} warm_requests={} warm_304={}",
        start.elapsed(),
        before.requests,
        http.stats().requests - before.requests,
        http.stats().not_modified - before.not_modified
    );
    let cache_bytes: u64 = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum();
    eprintln!("registries cache_with_wal_bytes={cache_bytes}");
}
