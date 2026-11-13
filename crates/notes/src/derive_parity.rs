//! Byte-for-byte parity between a buffered body and one arriving in chunks:
//! every derive the crate registers reads the same document either way, over
//! every body the fixtures serve.
use patchula_cache::State;
use patchula_http::Derived;
use patchula_http::conditional::{self, Body};
use std::sync::Arc;
use tokio::sync::Semaphore;

/// A derive function with its error erased, so the `CacheError` and
/// `serde_json::Error` derives share the harness.
type Erased =
    Arc<dyn Fn(&mut dyn std::io::Read) -> std::result::Result<Vec<u8>, ()> + Send + Sync + 'static>;

fn erase<E: 'static>(derived: Derived<E>) -> (&'static str, u32, Erased) {
    (
        derived.name,
        derived.version,
        Arc::new(move |body| (derived.derive)(body).map_err(|_| ())),
    )
}

/// Every `Derived` the crate registers.
fn derives() -> Vec<(&'static str, u32, Erased)> {
    vec![
        erase(crate::mirror::NOTES_MISE_VERSIONS),
        erase(crate::source::NOTES_SOURCE_NPM),
        erase(crate::source::NOTES_SOURCE_CRATES),
        erase(crate::source::NOTES_SOURCE_PYPI),
        erase(crate::forge::NOTES_GITLAB_RELEASES),
        erase(crate::forge::NOTES_GITEA_RELEASES),
        erase(crate::forge::NOTES_FORGE_TAGS),
        erase(crate::forge::NOTES_FORGE_FILES),
        erase(crate::forge::NOTES_FORGE_CHANGELOG),
        erase(crate::changelog::NOTES_CHANGELOG_DIRECTORY),
        erase(crate::changelog::PARSED_CHANGELOG),
        erase(crate::adapters::NOTES_ADAPTER_LISTING),
        erase(crate::adapters::PARSED_POSTGRES_SGML),
        erase(crate::adapters::PARSED_GO_HISTORY),
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

/// Every body a recorded response served.
fn bodies() -> Vec<Vec<u8>> {
    let mut bodies = Vec::new();
    collect(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures"),
        &mut bodies,
    );
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
/// `tests/derived-versions.tsv` hashes the files and locked dependency
/// versions that produce its bytes, and every `Derived::new` in the crate
/// must be registered in `derives()`. A change to one must bump its
/// `Derived`'s version and update the row in the same commit; this failure
/// names the new hash.
#[test]
fn every_derive_versions_its_compaction_source() {
    let registered: std::collections::BTreeMap<&'static str, u32> = derives()
        .into_iter()
        .map(|(name, version, _)| (name, version))
        .collect();
    patchula_testkit::derived::check("notes", &registered);
}

/// The crate's code-versioned states are pinned the same way:
/// `tests/state-versions.tsv` hashes the parsing sources the stored
/// sections are read through, and a change to one must bump the state's
/// `VERSION` and update the row in the same commit.
#[test]
fn every_code_versioned_state_versions_its_source() {
    let registered: std::collections::BTreeMap<&'static str, u32> = [
        (
            crate::ParsedChangelog::NAMESPACE,
            crate::ParsedChangelog::VERSION,
        ),
        (
            crate::adapters::AdapterSections::NAMESPACE,
            crate::adapters::AdapterSections::VERSION,
        ),
        (
            crate::releases::SavedRelease::NAMESPACE,
            crate::releases::SavedRelease::VERSION,
        ),
    ]
    .into_iter()
    .collect();
    patchula_testkit::state::check("notes", &registered);
}
