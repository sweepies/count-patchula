//! Byte-for-byte parity between a buffered body and one arriving in chunks:
//! every derive the crate registers reads the same document either way, over
//! every body the fixtures serve — and, when `PATCHULA_PARITY_DOCS` names a
//! directory, over the captured packuments `catalog_bench` replays.
use patchula_http::Derived;
use patchula_http::conditional::{self, Body};
use std::sync::Arc;
use tokio::sync::Semaphore;

/// A derive function with its error erased, so catalog derives and the
/// `serde_json::Error` ones share the harness.
type Erased =
    Arc<dyn Fn(&mut dyn std::io::Read) -> std::result::Result<Vec<u8>, ()> + Send + Sync + 'static>;

fn erase<E: 'static>(derived: Derived<E>) -> (&'static str, u32, Erased) {
    (
        derived.name,
        derived.version,
        Arc::new(move |body| (derived.derive)(body).map_err(|_| ())),
    )
}

/// Every `Derived` the crate registers, plus the representation the pull
/// token is stored under: its extraction code is its compaction.
fn derives() -> Vec<(&'static str, u32, Erased)> {
    vec![
        erase(crate::compact::NPM_CATALOG),
        erase(crate::compact::CRATES_CATALOG),
        erase(crate::compact::PYPI_CATALOG),
        erase(crate::registries::TERRAFORM_CATALOG),
        erase(crate::registries::HELM_INDEX),
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

fn bytes_of(body: &patchula_testkit::Body) -> Vec<u8> {
    match body {
        patchula_testkit::Body::Json(value) => serde_json::to_vec(value).unwrap(),
        patchula_testkit::Body::Text(text) => text.as_bytes().to_vec(),
    }
}

fn collect(dir: &std::path::Path, bodies: &mut Vec<Vec<u8>>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, bodies);
        } else if path.extension().is_some_and(|e| e == "json") {
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(cassette) = serde_json::from_slice::<patchula_testkit::Cassette>(&bytes) else {
                continue;
            };
            bodies.extend(
                cassette
                    .interactions
                    .iter()
                    .map(|interaction| bytes_of(&interaction.response.body)),
            );
        }
    }
}

/// Every body a recorded response served, plus the benchmark's captured
/// documents when their directory is pointed out: those are raw documents,
/// not cassettes.
fn bodies() -> Vec<Vec<u8>> {
    let mut bodies = Vec::new();
    collect(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures"),
        &mut bodies,
    );
    if let Ok(dir) = std::env::var("PATCHULA_PARITY_DOCS") {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                bodies.push(std::fs::read(path).unwrap());
            }
        }
    }
    bodies
}

fn chunks(body: &[u8], size: usize) -> patchula_http::BodyStream {
    let chunks: Vec<_> = body
        .chunks(size.max(1))
        .map(|chunk| Ok(bytes::Bytes::copy_from_slice(chunk)))
        .collect();
    Box::pin(futures_util::stream::iter(chunks))
}

async fn run(body: Body, f: Erased, decoding: &Arc<Semaphore>) -> Option<Vec<u8>> {
    conditional::derive(body, move |reader| f(reader), decoding)
        .await
        .unwrap()
        .ok()
}

/// Every fixture body derives the same bytes streamed as buffered — and
/// agrees to fail — however its chunks are sized.
#[tokio::test]
async fn every_derive_reads_a_streamed_body_the_same() {
    let decoding = Arc::new(Semaphore::new(4));
    let bodies = bodies();
    assert!(
        bodies.len() > 200,
        "the fixtures serve {} bodies",
        bodies.len()
    );
    for body in &bodies {
        for (name, _, f) in derives() {
            let buffered = run(Body::Buffered(body.clone()), f.clone(), &decoding).await;
            // An odd chunk size crosses token boundaries somewhere in every
            // document; one chunk is the fake-transport case.
            for size in [usize::MAX, 997] {
                let streamed = run(Body::Streaming(chunks(body, size)), f.clone(), &decoding).await;
                assert_eq!(
                    buffered, streamed,
                    "{name} differs on a {size}-chunk stream"
                );
            }
        }
    }
}

/// A derived body is versioned by its compaction code, not the release:
/// `tests/derived-versions.tsv` hashes the files that produce its bytes. A
/// change to one must bump its `Derived`'s version and update the row in the
/// same commit; this failure names the new hash.
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
