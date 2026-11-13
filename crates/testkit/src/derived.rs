//! `tests/derived-versions.tsv`: the versions and hashed compaction sources
//! of every derived representation. Each transform's crate owns the rows
//! whose first source lives under its `src/` and checks them in its
//! `derive_parity` test.
use sha2::{Digest, Sha256};
use std::path::Path;

/// One table row: a representation's name, its version, the source files
/// whose text defines its derived bytes, and their hash.
pub struct Row {
    pub name: String,
    pub version: u32,
    pub sources: Vec<String>,
    pub sha256: String,
}

/// The workspace root: two directories up from this crate.
fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("a crate in the workspace")
}

/// Every row of `tests/derived-versions.tsv`, comments dropped.
pub fn table() -> Vec<Row> {
    let path = root().join("tests/derived-versions.tsv");
    let table = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    table
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let mut fields = line.split('\t');
            let (Some(name), Some(version), Some(sources), Some(sha256), None) = (
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
            ) else {
                panic!("tests/derived-versions.tsv: malformed row {line:?}");
            };
            Row {
                name: name.into(),
                version: version
                    .parse()
                    .unwrap_or_else(|_| panic!("{name}: bad version {version:?}")),
                sources: sources.split(',').map(str::to_owned).collect(),
                sha256: sha256.into(),
            }
        })
        .collect()
}

/// The sha256 of the named workspace files' text, `path\0bytes` each, in
/// listed order.
pub fn hash(sources: &[String]) -> String {
    let mut hasher = Sha256::new();
    for source in sources {
        let path = root().join(source);
        let bytes =
            std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        hasher.update(source.as_bytes());
        hasher.update(b"\0");
        hasher.update(&bytes);
        hasher.update(b"\0");
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The guard: every registered representation has a row whose version and
/// source hash still hold, and no row is stale. A row is owned by `krate`
/// when its first source lives under `crates/{krate}/`. On a source change
/// the failure names the new hash; bump the representation's version and
/// update its row in the same commit.
pub fn check(krate: &str, registered: &std::collections::BTreeMap<&'static str, u32>) {
    let mut seen = std::collections::BTreeSet::new();
    for row in table() {
        let owned = row
            .sources
            .first()
            .is_some_and(|source| source.starts_with(&format!("crates/{krate}/")));
        match registered.get(row.name.as_str()) {
            Some(&version) => {
                assert_eq!(
                    version, row.version,
                    "{}: bump the version and update the row together",
                    row.name
                );
                let actual = hash(&row.sources);
                assert_eq!(
                    actual,
                    row.sha256,
                    "{}: compaction source changed — bump its version and set \
                     the row to v<N+1>\\t{}\\t{actual} in tests/derived-versions.tsv",
                    row.name,
                    row.sources.join(",")
                );
                seen.insert(row.name);
            }
            None if owned => {
                panic!("{}: a stale row in tests/derived-versions.tsv", row.name)
            }
            None => {}
        }
    }
    for name in registered.keys() {
        assert!(
            seen.contains(*name),
            "{name}: no row in tests/derived-versions.tsv — add one"
        );
    }
}
