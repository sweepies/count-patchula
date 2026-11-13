//! `tests/derived-versions.tsv`: the versions and hashed compaction sources
//! of every derived representation. Each transform's crate owns the rows
//! whose first source lives under its `src/` and checks them in its
//! `derive_parity` test.
use std::collections::BTreeSet;
use std::path::Path;

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
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
                names.extend(new.captures_iter(&text).map(|c| c[1].to_owned()));
                names.extend(constant.captures_iter(&text).map(|c| c[1].to_owned()));
            }
        }
    }
}

/// Every registered representation has a row whose version and source
/// hash still hold, every representation the crate's sources declare is
/// registered, and no row is stale. On a source or locked dependency
/// change the failure names the new hash; bump the representation's
/// version and update its row in the same commit.
pub fn check(krate: &str, registered: &std::collections::BTreeMap<&'static str, u32>) {
    crate::versions::check("derived-versions.tsv", krate, registered, declared(krate));
}
