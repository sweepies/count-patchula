//! `tests/derived-versions.tsv`: the versions and hashed compaction sources
//! of every derived representation. Each transform's crate owns the rows
//! whose first source lives under its `src/` and checks them in its
//! `derive_parity` test, which also runs the streaming parity harness here.
use patchula_http::{
    Derived,
    conditional::{self, Body},
};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Semaphore;

/// A derive function with its error erased, so derives with different error
/// types share the harness.
pub type Erased =
    Arc<dyn Fn(&mut dyn std::io::Read) -> std::result::Result<Vec<u8>, ()> + Send + Sync + 'static>;

/// A registered derive: its name, version and erased function.
pub type Registered = (&'static str, u32, Erased);

pub fn erase<E: 'static>(derived: Derived<E>) -> Registered {
    (
        derived.name,
        derived.version,
        Arc::new(move |body| (derived.derive)(body).map_err(|_| ())),
    )
}

fn bytes_of(body: &crate::Body) -> Vec<u8> {
    match body {
        crate::Body::Json(value) => serde_json::to_vec(value).unwrap(),
        crate::Body::Text(text) => text.as_bytes().to_vec(),
    }
}

/// The response bodies of every cassette under `dir`.
fn collect(dir: &Path, bodies: &mut Vec<Vec<u8>>) {
    for path in crate::versions::files_under(dir) {
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(cassette) = serde_json::from_slice::<crate::Cassette>(&bytes) else {
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

/// Every body a recorded response under `fixtures/` served, plus the raw
/// documents in the directory `PATCHULA_PARITY_DOCS` names, such as the
/// packuments `catalog_bench` replays.
pub fn bodies() -> Vec<Vec<u8>> {
    let mut bodies = Vec::new();
    collect(&crate::versions::root().join("fixtures"), &mut bodies);
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

/// `body` as a stream of `size`-byte chunks.
pub fn chunks(body: &[u8], size: usize) -> patchula_http::BodyStream {
    let chunks: Vec<_> = body
        .chunks(size.max(1))
        .map(|chunk| Ok(bytes::Bytes::copy_from_slice(chunk)))
        .collect();
    Box::pin(futures_util::stream::iter(chunks))
}

/// The derive's answer for `body`; `None` when it fails.
pub async fn run(body: Body, f: Erased, decoding: &Arc<Semaphore>) -> Option<Vec<u8>> {
    conditional::derive(body, move |reader| f(reader), decoding)
        .await
        .unwrap()
        .ok()
}

/// Every fixture body derives the same bytes streamed as buffered — and
/// agrees to fail — however its chunks are sized.
pub async fn streams_the_same(derives: &[Registered]) {
    let decoding = Arc::new(Semaphore::new(4));
    let bodies = bodies();
    assert!(
        bodies.len() > 200,
        "the fixtures serve {} bodies",
        bodies.len()
    );
    for body in &bodies {
        for (name, _, f) in derives {
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

/// Every representation name `crates/{krate}/src` declares: each
/// `Derived::new("<name>"` and each representation constant such as the
/// pull token's `("name", version)` pair.
fn declared(krate: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    scan(
        &crate::versions::root().join(format!("crates/{krate}/src")),
        &mut names,
    );
    names
}

fn scan(dir: &Path, names: &mut BTreeSet<String>) {
    let new = regex::Regex::new(r#"Derived::new\(\s*"([^"]+)""#).unwrap();
    let constant = regex::Regex::new(r#"REPRESENTATION[^=\n]*=\s*\(\s*"([^"]+)""#).unwrap();
    for path in crate::versions::files_under(dir) {
        if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            names.extend(new.captures_iter(&text).map(|c| c[1].to_owned()));
            names.extend(constant.captures_iter(&text).map(|c| c[1].to_owned()));
        }
    }
}

/// Every registered representation has a row whose version and source
/// hash still hold, every representation the crate's sources declare is
/// registered, and no row is stale. On a source change the failure names
/// the new hash; bump the representation's version and update its row in
/// the same commit. A listed crate must be in Cargo.lock; its locked
/// version keys the cache entry, not the row.
pub fn check(krate: &str, registered: &std::collections::BTreeMap<&'static str, u32>) {
    crate::versions::check("derived-versions.tsv", krate, registered, declared(krate));
}
