//! The machinery `derived` and `state` share: a TSV of names, versions,
//! the source files and Cargo.lock packages whose text and locked
//! versions produce the persisted value, and the sha256 of all of it.
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// One table row: a name, its version, the source files and trees whose
/// text defines the stored value, the Cargo.lock packages whose locked
/// versions also do, and the hash of all of it. A source ending `/`
/// names every file under that tree, so a file added to one changes the
/// hash too.
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
    let path = root().join("tests").join(file);
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

/// The package versions Cargo.lock resolves, by package name.
fn locked() -> BTreeMap<String, Vec<String>> {
    let path = root().join("Cargo.lock");
    let lock = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let mut packages: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut name = None;
    for line in lock.lines() {
        if line == "[[package]]" {
            name = None;
        } else if let Some(rest) = line.strip_prefix("name = ") {
            name = Some(rest.trim_matches('"').to_owned());
        } else if let Some(rest) = line.strip_prefix("version = ")
            && let Some(package) = name.take()
        {
            packages
                .entry(package)
                .or_default()
                .push(rest.trim_matches('"').to_owned());
        }
    }
    for versions in packages.values_mut() {
        versions.sort();
    }
    packages
}

/// The `(name, path)` files one source entry covers: itself, or every
/// file under it when it ends in `/`, sorted so the hash is stable.
fn covered(source: &str) -> Vec<(String, PathBuf)> {
    let path = root().join(source);
    if !source.ends_with('/') {
        return vec![(source.to_owned(), path)];
    }
    let mut found = Vec::new();
    let mut pending = vec![path];
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
        .into_iter()
        .map(|path| {
            let name = path
                .strip_prefix(root())
                .expect("under the workspace root")
                .to_str()
                .expect("a UTF-8 path")
                .replace('\\', "/");
            (name, path)
        })
        .collect()
}

/// The sha256 of the row's inputs: each covered file's text as
/// `path\0bytes\0`, then each listed dependency's locked versions as
/// `crate\0versions\0`. A covered file the tree no longer has, or a
/// listed crate the lock no longer resolves, says which is missing.
pub(crate) fn hash(row: &Row) -> String {
    let mut hasher = Sha256::new();
    for source in &row.sources {
        for (name, path) in covered(source) {
            let bytes = std::fs::read(&path).unwrap_or_else(|error| {
                panic!("{}: listed source {}: {error}", row.name, path.display())
            });
            hasher.update(name.as_bytes());
            hasher.update(b"\0");
            hasher.update(&bytes);
            hasher.update(b"\0");
        }
    }
    if !row.deps.is_empty() {
        let locked = locked();
        for dep in &row.deps {
            let versions = locked.get(dep).unwrap_or_else(|| {
                panic!("{}: listed dependency {dep} is not in Cargo.lock", row.name)
            });
            hasher.update(dep.as_bytes());
            hasher.update(b"\0");
            hasher.update(versions.join(",").as_bytes());
            hasher.update(b"\0");
        }
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The guard both tables run: every registered name has a row in
/// `tests/{file}` whose version and source hash still hold, every name
/// the crate's sources declare is registered, and no row is stale. A row
/// is owned by `krate` when its first source lives under
/// `crates/{krate}/`. On a source or locked dependency change the
/// failure names the new hash; bump the version and update the row in
/// the same commit.
pub(crate) fn check(
    file: &str,
    krate: &str,
    registered: &BTreeMap<&'static str, u32>,
    declared: BTreeSet<String>,
) {
    let mut seen = BTreeSet::new();
    for row in table(file) {
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
                let actual = hash(&row);
                assert_eq!(
                    actual,
                    row.sha256,
                    "{}: derivation source or locked dependency changed — bump its \
                     version and set the row to \
                     v<N+1>\\t{}\\t{}\\t{actual} in tests/{file}",
                    row.name,
                    row.sources.join(","),
                    row.deps.join(",")
                );
                seen.insert(row.name);
            }
            None if owned => {
                panic!("{}: a stale row in tests/{file}", row.name)
            }
            None => {}
        }
    }
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
