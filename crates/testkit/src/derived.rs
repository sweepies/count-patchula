//! `tests/derived-versions.tsv`: the versions and hashed compaction sources
//! of every derived representation. Each transform's crate owns the rows
//! whose first source lives under its `src/` and checks them in its
//! `derive_parity` test.
use sha2::{Digest, Sha256};
use std::path::Path;

/// One table row: a representation's name, its version, the source files
/// whose text defines its derived bytes, the Cargo.lock packages whose
/// locked versions also do, and the hash of all of it.
pub struct Row {
    pub name: String,
    pub version: u32,
    pub sources: Vec<String>,
    pub deps: Vec<String>,
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
            let (Some(name), Some(version), Some(sources), Some(deps), Some(sha256), None) = (
                fields.next(),
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
fn locked() -> std::collections::BTreeMap<String, Vec<String>> {
    let path = root().join("Cargo.lock");
    let lock = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let mut packages: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
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

/// The sha256 of the row's inputs: each listed workspace file's text as
/// `path\0bytes\0` in listed order, then each listed dependency's locked
/// versions as `crate\0versions\0`. A listed file the tree no longer has, or
/// a listed crate the lock no longer resolves, says which is missing.
pub fn hash(row: &Row) -> String {
    let mut hasher = Sha256::new();
    for source in &row.sources {
        let path = root().join(source);
        let bytes = std::fs::read(&path).unwrap_or_else(|error| {
            panic!("{}: listed source {}: {error}", row.name, path.display())
        });
        hasher.update(source.as_bytes());
        hasher.update(b"\0");
        hasher.update(&bytes);
        hasher.update(b"\0");
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

/// Every representation name `crates/{krate}/src` declares: each
/// `Derived::new("<name>"` and each representation constant such as the
/// pull token's `("name", version)` pair.
fn declared(krate: &str) -> std::collections::BTreeSet<String> {
    let mut names = std::collections::BTreeSet::new();
    scan(&root().join(format!("crates/{krate}/src")), &mut names);
    names
}

fn scan(dir: &Path, names: &mut std::collections::BTreeSet<String>) {
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

/// The guard: every registered representation has a row whose version and
/// source hash still hold, every representation the crate's sources declare
/// is registered, and no row is stale. A row is owned by `krate` when its
/// first source lives under `crates/{krate}/`. On a source or locked
/// dependency change the failure names the new hash; bump the
/// representation's version and update its row in the same commit.
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
                let actual = hash(&row);
                assert_eq!(
                    actual,
                    row.sha256,
                    "{}: compaction source or locked dependency changed — bump its \
                     version and set the row to \
                     v<N+1>\\t{}\\t{}\\t{actual} in tests/derived-versions.tsv",
                    row.name,
                    row.sources.join(","),
                    row.deps.join(",")
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
    for name in declared(krate) {
        assert!(
            registered.contains_key(name.as_str()),
            "{name}: declared in crates/{krate}/src but not registered — list its \
             Derived in derive_parity's derives() and add a row to \
             tests/derived-versions.tsv"
        );
    }
}
