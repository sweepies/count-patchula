//! Byte-for-byte parity between a buffered body and one arriving in chunks:
//! every derive the crate registers reads the same document either way, over
//! every body the fixtures serve — and, when `PATCHULA_PARITY_DOCS` names a
//! directory, over the captured packuments `catalog_bench` replays.
use patchula_cache::State;
use patchula_http::conditional::Body;
use patchula_testkit::derived::{Registered, bodies, chunks, erase, run, streams_the_same};
use std::sync::Arc;
use tokio::sync::Semaphore;

/// Every `Derived` the crate registers. The pull token's representation is
/// registered beside them in `every_derive_versions_its_compaction_source`.
fn derives() -> Vec<Registered> {
    vec![
        erase(crate::compact::NPM_CATALOG),
        erase(crate::compact::CRATES_CATALOG),
        erase(crate::compact::PYPI_CATALOG),
        erase(crate::registries::TERRAFORM_CATALOG),
        erase(crate::helm::HELM_INDEX),
        erase(crate::registries::PYPI_INDEX_CATALOG),
        erase(crate::registries::PYPI_SIMPLE_CATALOG),
        erase(crate::nuget::NUGET_FLAT),
        erase(crate::nuget::NUGET_REGISTRATION),
        erase(crate::nuget::NUGET_REGISTRATION_PAGE),
        erase(crate::rubygems::RUBYGEMS_VERSIONS),
        erase(crate::packagist::PACKAGIST_P2),
        erase(crate::maven::MAVEN_METADATA),
        erase(crate::docker::OCI_CONFIG_NOTES),
        erase(crate::docker::OCI_TAGS),
        erase(crate::images::IMAGE_PLATFORM),
        erase(crate::images::SBOM_OS),
        erase(crate::forge::FORGE_GITLAB_TAGS),
        erase(crate::forge::FORGE_GITEA_TAGS),
        erase(crate::github::GITHUB_RELEASE_HEAD),
    ]
}

/// Every fixture body derives the same bytes streamed as buffered — and
/// agrees to fail — however its chunks are sized.
#[tokio::test]
async fn every_derive_reads_a_streamed_body_the_same() {
    streams_the_same(&derives()).await;
}

/// A derived body is versioned by its compaction code, not the release:
/// `tests/derived-versions.tsv` hashes the files and locked dependency
/// versions that produce its bytes, and every `Derived::new` in the crate
/// must be registered in `derives()`. A change to one must bump its
/// `Derived`'s version and update the row in the same commit; this failure
/// names the new hash.
#[test]
fn every_derive_versions_its_compaction_source() {
    let mut registered: std::collections::BTreeMap<&'static str, u32> = derives()
        .into_iter()
        .map(|(name, version, _)| (name, version))
        .collect();
    registered.insert(
        crate::docker::PULL_TOKEN_REPRESENTATION.0,
        crate::docker::PULL_TOKEN_REPRESENTATION.1,
    );
    patchula_testkit::derived::check("datasource", &registered);
}

/// The crate's code-versioned states are pinned the same way:
/// `tests/state-versions.tsv` hashes the decoding and mapping sources the
/// stored values are read through, and a change to one must bump the
/// state's `VERSION` and update the row in the same commit.
#[test]
fn every_code_versioned_state_versions_its_source() {
    let registered: std::collections::BTreeMap<&'static str, u32> = [
        (
            crate::github::SourceHead::NAMESPACE,
            crate::github::SourceHead::VERSION,
        ),
        (
            crate::github::TagCatalog::NAMESPACE,
            crate::github::TagCatalog::VERSION,
        ),
        (
            crate::github::TagObject::NAMESPACE,
            crate::github::TagObject::VERSION,
        ),
        (
            crate::advisories::Stored::NAMESPACE,
            crate::advisories::Stored::VERSION,
        ),
    ]
    .into_iter()
    .collect();
    patchula_testkit::state::check("datasource", &registered);
}

/// The PyPI index's HTML fallback: a PEP 503 page reads its anchors the same
/// buffered or a byte at a time, tags with quoted `>` and `data-yanked`
/// included. The fixtures serve JSON pages, so this body is made here.
#[tokio::test]
async fn a_simple_page_scans_anchors_the_same() {
    let body = br#"<!DOCTYPE html><html><body>
<a href="https://x.test/pkg-1.0.tar.gz#sha256=aa">pkg-1.0.tar.gz</a><br />
<a href='a>b' data-yanked>pkg-1.1.tar.gz</a>
<a href="c">other-2.0.whl</a>&lt;text&gt;
<a
  href="d">pkg-2.1.tar.gz</a>
<p>none</p>
</body></html>"#
        .to_vec();
    let decoding = Arc::new(Semaphore::new(4));
    let (_, _, f) = derives()
        .into_iter()
        .find(|(name, _, _)| *name == "pypi-index-catalog")
        .unwrap();
    let buffered = run(Body::Buffered(body.clone()), f.clone(), &decoding)
        .await
        .unwrap();
    assert!(
        buffered.windows(5).any(|w| w == b"\"1.1\""),
        "the page's anchors did not parse"
    );
    for size in [usize::MAX, 997, 1] {
        let streamed = run(Body::Streaming(chunks(&body, size)), f.clone(), &decoding)
            .await
            .unwrap();
        assert_eq!(buffered, streamed, "pypi-index-catalog differs at {size}");
    }
}

/// A one-byte stream stresses the chunk reader where documents are small
/// enough for it to stay cheap.
#[tokio::test]
async fn a_byte_at_a_time_reads_the_same() {
    let decoding = Arc::new(Semaphore::new(4));
    for body in bodies().iter().filter(|body| body.len() <= 4096).take(200) {
        for (name, _, f) in derives() {
            let buffered = run(Body::Buffered(body.clone()), f.clone(), &decoding).await;
            let streamed = run(Body::Streaming(chunks(body, 1)), f.clone(), &decoding).await;
            assert_eq!(buffered, streamed, "{name} differs byte-by-byte");
        }
    }
}
