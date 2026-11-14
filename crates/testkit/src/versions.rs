//! The machinery `derived` and `state` share: a TSV of names, versions,
//! the source files whose text produces the persisted value, the Cargo.lock
//! packages whose locked versions key it, and the sha256 of the sources.
#[path = "../../cache/build_support.rs"]
mod build_support;

use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// One table row: a name, its version, the source files and trees whose
/// text defines the stored value, the Cargo.lock packages whose locked
/// versions the cache key fingerprints, and the hash of the sources. A
/// source ending `/` names every file under that tree, so a file added to
/// one changes the hash too.
pub(crate) struct Row {
    pub name: String,
    pub version: u32,
    pub sources: Vec<String>,
    pub deps: Vec<String>,
    pub sha256: String,
}

/// The workspace root: two directories up from this crate.
pub(crate) fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("a crate in the workspace")
}

/// Every row of `tests/{file}`, comments dropped.
pub(crate) fn table(file: &str) -> Vec<Row> {
    table_in(root(), file)
}

fn table_in(root: &Path, file: &str) -> Vec<Row> {
    let path = root.join("tests").join(file);
    let table = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    table
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let mut fields = line.split('\t');
            let (Some(name), Some(version), Some(sources), Some(deps), Some(sha256), None) = (
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
            ) else {
                panic!("tests/{file}: malformed row {line:?}");
            };
            Row {
                name: name.into(),
                version: version
                    .parse()
                    .unwrap_or_else(|_| panic!("{name}: bad version {version:?}")),
                sources: sources.split(',').map(str::to_owned).collect(),
                deps: if deps == "-" {
                    Vec::new()
                } else {
                    deps.split(',').map(str::to_owned).collect()
                },
                sha256: sha256.into(),
            }
        })
        .collect()
}

/// Every file under `dir`, recursively, in path order.
pub(crate) fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)
            .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
            .flatten()
        {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// The `(name, path)` files one source entry covers: itself, or every
/// file under it when it ends in `/`, sorted so the hash is stable.
fn covered(root: &Path, source: &str) -> Vec<(String, PathBuf)> {
    let path = root.join(source);
    if !source.ends_with('/') {
        return vec![(source.to_owned(), path)];
    }
    files_under(&path)
        .into_iter()
        .map(|path| {
            let name = path
                .strip_prefix(root)
                .expect("under the workspace root")
                .to_str()
                .expect("a UTF-8 path")
                .replace('\\', "/");
            (name, path)
        })
        .collect()
}

/// The sha256 of the row's sources: each covered file's text as
/// `path\0bytes\0`. A covered file the tree no longer has says which is
/// missing. Listed crates stay out: their locked versions key the cache
/// entry instead (`crates/cache/build_support.rs`), so a bump needs no
/// version bump.
fn hash(root: &Path, row: &Row) -> String {
    let mut hasher = Sha256::new();
    for source in &row.sources {
        for (name, path) in covered(root, source) {
            let bytes = std::fs::read(&path).unwrap_or_else(|error| {
                panic!("{}: listed source {}: {error}", row.name, path.display())
            });
            hasher.update(name.as_bytes());
            hasher.update(b"\0");
            hasher.update(&bytes);
            hasher.update(b"\0");
        }
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The guard both tables run: every listed crate is in Cargo.lock, every
/// registered name has a row in `tests/{file}` whose version and source
/// hash still hold, every name the crate's sources declare is registered,
/// and no row is stale. A row is owned by `krate` when its first source
/// lives under `crates/{krate}/`. On a source change the failure names
/// every changed row's new hash; bump each version and update its row in
/// the same commit. A locked version change needs neither: it changes the
/// cache key's fingerprint.
pub(crate) fn check(
    file: &str,
    krate: &str,
    registered: &BTreeMap<&'static str, u32>,
    declared: BTreeSet<String>,
) {
    check_in(root(), file, krate, registered, declared);
}

fn check_in(
    root: &Path,
    file: &str,
    krate: &str,
    registered: &BTreeMap<&'static str, u32>,
    declared: BTreeSet<String>,
) {
    let read = |path: PathBuf| {
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    };
    if let Err(error) = build_support::fingerprints(
        &read(root.join("Cargo.lock")),
        &read(root.join("tests").join(file)),
    ) {
        panic!("tests/{file}: {error}");
    }
    let mut seen = BTreeSet::new();
    let mut changed = Vec::new();
    for row in table_in(root, file) {
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
                let actual = hash(root, &row);
                if actual != row.sha256 {
                    changed.push(format!(
                        "{}: source changed, bump the version and set the row to \
                         {}\\t<N+1>\\t{}\\t{}\\t{actual}",
                        row.name,
                        row.name,
                        row.sources.join(","),
                        if row.deps.is_empty() {
                            "-".to_owned()
                        } else {
                            row.deps.join(",")
                        }
                    ));
                }
                seen.insert(row.name);
            }
            None if owned => {
                panic!("{}: a stale row in tests/{file}", row.name)
            }
            None => {}
        }
    }
    assert!(changed.is_empty(), "tests/{file}:\n{}", changed.join("\n"));
    for name in registered.keys() {
        assert!(
            seen.contains(*name),
            "{name}: no row in tests/{file} — add one"
        );
    }
    for name in declared {
        assert!(
            registered.contains_key(name.as_str()),
            "{name}: declared in crates/{krate}/src but not registered — list it in \
             the parity test and add a row to tests/{file}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch workspace: one listed source, a Cargo.lock locking `serde`
    /// at `serde`, and a table whose row lists the source and `deps`.
    fn workspace(source: &str, serde: &str, deps: &str) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("crates/demo/src")).unwrap();
        std::fs::create_dir_all(root.path().join("tests")).unwrap();
        std::fs::write(root.path().join("crates/demo/src/lib.rs"), source).unwrap();
        std::fs::write(
            root.path().join("Cargo.lock"),
            format!("version = 4\n\n[[package]]\nname = \"serde\"\nversion = \"{serde}\"\n"),
        )
        .unwrap();
        let mut row = Row {
            name: "demo".into(),
            version: 1,
            sources: vec!["crates/demo/src/lib.rs".into()],
            deps: Vec::new(),
            sha256: String::new(),
        };
        let sha256 = hash(root.path(), &row);
        row.sha256 = sha256;
        std::fs::write(
            root.path().join("tests/demo-versions.tsv"),
            format!(
                "# header\ndemo\t1\t{}\t{deps}\t{}\n",
                row.sources[0], row.sha256
            ),
        )
        .unwrap();
        root
    }

    fn guard(root: &Path) -> std::thread::Result<()> {
        let registered = BTreeMap::from([("demo", 1)]);
        let root = root.to_path_buf();
        std::panic::catch_unwind(move || {
            check_in(
                &root,
                "demo-versions.tsv",
                "demo",
                &registered,
                BTreeSet::from(["demo".to_owned()]),
            );
        })
    }

    /// A Cargo.lock bump of a listed crate passes the guard — the cache key
    /// carries it — while an edit to a listed source fails until the row is
    /// rehashed, and a listed crate the lock lacks fails as a typo.
    #[test]
    fn the_guard_covers_sources_not_locked_versions() {
        let root = workspace("fn a() {}\n", "1.0.0", "serde");
        guard(root.path()).expect("a fresh row holds");
        std::fs::write(
            root.path().join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.1\"\n",
        )
        .unwrap();
        guard(root.path()).expect("a locked version bump needs no version bump");
        std::fs::write(root.path().join("crates/demo/src/lib.rs"), "fn b() {}\n").unwrap();
        assert!(guard(root.path()).is_err(), "a source edit fails");
        let typo = workspace("fn a() {}\n", "1.0.0", "serd");
        assert!(guard(typo.path()).is_err(), "an unlocked crate fails");
        let none = workspace("fn a() {}\n", "1.0.0", "-");
        guard(none.path()).expect("a row may list no crates");
    }
}
