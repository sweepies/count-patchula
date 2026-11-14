//! Emits `fingerprints.rs`: for each row of `tests/state-versions.tsv` and
//! `tests/derived-versions.tsv`, the fingerprint of its listed crates'
//! locked versions, which the cache appends to that state's or
//! representation's key. A dependency bump then misses exactly the caches
//! that list it. The build needs the workspace's Cargo.lock and tables, so
//! it fails outside the workspace rather than key without them.
#[path = "build_support.rs"]
mod build_support;

use std::fmt::Write as _;
use std::path::Path;

fn read(path: &Path) -> String {
    println!("cargo:rerun-if-changed={}", path.display());
    std::fs::read_to_string(path).unwrap_or_else(|error| {
        panic!(
            "{}: {error} — patchula-cache builds only inside the count-patchula \
             workspace, whose Cargo.lock and tests/*-versions.tsv key its cache",
            path.display()
        )
    })
}

/// A `fn {name}(name: &str) -> Option<&'static str>` over one table's rows.
fn function(out: &mut String, function: &str, lock: &str, table: &Path) {
    let rows = build_support::fingerprints(lock, &read(table))
        .unwrap_or_else(|error| panic!("{}: {error}", table.display()));
    writeln!(out, "fn {function}(name: &str) -> Option<&'static str> {{").unwrap();
    writeln!(out, "    match name {{").unwrap();
    for (name, fingerprint) in rows {
        if let Some(fingerprint) = fingerprint {
            writeln!(out, "        {name:?} => Some({fingerprint:?}),").unwrap();
        }
    }
    writeln!(out, "        _ => None,\n    }}\n}}").unwrap();
}

fn main() {
    println!("cargo:rerun-if-changed=build_support.rs");
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let root = Path::new(&manifest)
        .parent()
        .and_then(Path::parent)
        .expect("crates/cache sits two levels under the workspace root");
    let lock = read(&root.join("Cargo.lock"));
    let mut out = String::new();
    function(
        &mut out,
        "state_fingerprint",
        &lock,
        &root.join("tests/state-versions.tsv"),
    );
    function(
        &mut out,
        "derived_fingerprint",
        &lock,
        &root.join("tests/derived-versions.tsv"),
    );
    let dest =
        Path::new(&std::env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("fingerprints.rs");
    std::fs::write(dest, out).expect("write fingerprints.rs");
}
