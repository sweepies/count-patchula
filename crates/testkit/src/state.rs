//! `tests/state-versions.tsv`: the versions and hashed derivation sources
//! of every code-versioned `State`. Each state's crate owns the rows
//! whose first source lives under its `src/` and checks them in its
//! parity test.
use std::collections::BTreeSet;
use std::path::Path;

/// Every code-versioned state `crates/{krate}/src` declares: the
/// `NAMESPACE` of each `impl State` block that sets
/// `CODE_VERSIONED: bool = true`.
fn declared(krate: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    scan(
        &crate::versions::root().join(format!("crates/{krate}/src")),
        &mut names,
    );
    names
}

fn scan(dir: &Path, names: &mut BTreeSet<String>) {
    let block = regex::Regex::new(r"(?ms)^\s*impl State for [^{]+\{(.*?)^\s*\}").unwrap();
    let namespace = regex::Regex::new(r#"NAMESPACE: &'static str = "([^"]+)""#).unwrap();
    let versioned = regex::Regex::new(r"CODE_VERSIONED: bool = true").unwrap();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
                for block in block.captures_iter(&text) {
                    if versioned.is_match(&block[1])
                        && let Some(name) = namespace.captures(&block[1])
                    {
                        names.insert(name[1].to_owned());
                    }
                }
            }
        }
    }
}

/// Every registered state has a row whose version and source hash still
/// hold, every code-versioned state the crate's sources declare is
/// registered, and no row is stale. On a source or locked dependency
/// change the failure names the new hash; bump the state's `VERSION` and
/// update its row in the same commit.
pub fn check(krate: &str, registered: &std::collections::BTreeMap<&'static str, u32>) {
    crate::versions::check("state-versions.tsv", krate, registered, declared(krate));
}
