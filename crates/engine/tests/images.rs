//! Image SBOMs read by digest and compared through OSV.dev. The alpine
//! fixture is recorded from mirror.gcr.io and OSV.dev by this implementation;
//! replay is offline.
use patchula_cache::Cache;
use patchula_datasource::{Client, ImageSbom, SbomSource};
use patchula_http::{Cached, Network};
use patchula_lookup::images::{Side, compare, image_queries, image_state};
use std::{path::Path, sync::Arc};

const OLD: &str = "sha256:1e42bbe2508154c9126d48c2b8a75420c3544343bf86fd041fb7527e017a4b4a";
const NEW: &str = "sha256:d9e853e87e55526f6b2917df91a2115c36dd7c696a35be12163d44e6e2a4b6bc";

#[tokio::test]
async fn an_alpine_rebuild_fixes_what_osv_lists_for_the_older_digest() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/engine/sboms-alpine.json");
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
    let client = Client::new(github, http, cache).with_versions_host(false);
    let mut sides: Vec<Side> = Vec::new();
    for digest in [OLD, NEW] {
        let sbom = client
            .image_sbom("library/alpine", digest, "linux/amd64")
            .await
            .unwrap();
        let ImageSbom::Found(found) = &sbom else {
            panic!("{digest}: {sbom:?}");
        };
        assert_eq!(found.source, SbomSource::Attestation);
        assert!(
            found.generator.contains("docker-scout"),
            "{}",
            found.generator
        );
        assert!(found.cataloger && found.sources_known);
        assert_eq!(found.ecosystem.as_deref(), Some("Alpine:v3.20"));
        assert!(found.sources.iter().any(|(name, _)| name == "openssl"));
        sides.push(Side::from([("linux/amd64".to_owned(), Ok(sbom))]));
    }
    let queries = image_queries(&sides[0])
        .into_iter()
        .chain(image_queries(&sides[1]))
        .collect();
    let listing = client.advisory_listing(&queries).await.unwrap();
    let check = compare((OLD, &sides[0]), (NEW, &sides[1]), &listing).unwrap();
    assert_eq!(check.state, "checked");
    assert_eq!(check.platforms, ["linux/amd64"]);
    let ids: std::collections::BTreeSet<_> = check.fixed.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids.len(), 29, "{ids:?}");
    assert!(check.introduced.is_empty(), "{:?}", check.introduced);
    assert!(
        check
            .fixed
            .iter()
            .all(|f| f.new.as_deref() != Some(f.old.as_str()))
    );
}

/// Each way an image publishes its SBOM, or doesn't, recorded from the
/// registries that use it.
#[tokio::test]
async fn registries_publish_sboms_in_each_known_way() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/engine/sboms-registries.json");
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
    let client = Client::new(github, http, cache).with_versions_host(false);
    let mut seen = Vec::new();
    for (package, tag) in [
        ("cgr.dev/chainguard/static", "latest"),
        ("library/busybox", "latest"),
        ("quay.io/argoproj/argocd", "v3.1.8"),
    ] {
        let digest = client.registry_digest(package, tag).await.unwrap();
        let sbom = client
            .image_sbom(package, &digest, "linux/amd64")
            .await
            .unwrap();
        let state = image_state(&Side::from([("linux/amd64".to_owned(), Ok(sbom.clone()))]))
            .map(|check| check.state);
        seen.push(match sbom {
            ImageSbom::Found(sbom) => (
                package,
                Some((
                    sbom.source,
                    sbom.cataloger,
                    sbom.packages > 0,
                    sbom.ecosystem,
                )),
                state,
            ),
            _ => (package, None, state),
        });
    }
    assert_eq!(
        seen,
        [
            (
                "cgr.dev/chainguard/static",
                Some((SbomSource::CosignAtt, true, true, Some("Wolfi".to_owned()))),
                None
            ),
            (
                "library/busybox",
                Some((SbomSource::Attestation, true, false, None)),
                None
            ),
            ("quay.io/argoproj/argocd", None, Some("no-sbom".to_owned())),
        ]
    );
}
