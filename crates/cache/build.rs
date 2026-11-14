//! Emits `fingerprints.rs`: for each row of `tests/state-versions.tsv` and
//! `tests/derived-versions.tsv`, its version and the fingerprint of its
//! listed crates' locked versions. The cache appends the fingerprint to
//! that state's or representation's key, and prunes the name's other keys
//! when it opens. A dependency bump then misses exactly the caches
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

/// A `const {name}: &[(&str, u32, Option<&str>)]` of one table's rows:
/// name, version and fingerprint.
fn rows(out: &mut String, constant: &str, lock: &str, table: &Path) {
    let rows = build_support::fingerprints(lock, &read(table))
        .unwrap_or_else(|error| panic!("{}: {error}", table.display()));
    writeln!(out, "const {constant}: &[(&str, u32, Option<&str>)] = &[").unwrap();
    for (name, version, fingerprint) in rows {
        writeln!(out, "    ({name:?}, {version}, {fingerprint:?}),").unwrap();
    }
    writeln!(out, "];").unwrap();
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
    rows(
        &mut out,
        "STATE_ROWS",
        &lock,
        &root.join("tests/state-versions.tsv"),
    );
    rows(
        &mut out,
        "DERIVED_ROWS",
        &lock,
        &root.join("tests/derived-versions.tsv"),
    );
    let dest =
        Path::new(&std::env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("fingerprints.rs");
    std::fs::write(dest, out).expect("write fingerprints.rs");
}
