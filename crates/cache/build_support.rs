//! Dependency fingerprints for the code-versioned cache keys, shared by the
//! cache crate's build script (which emits them), its unit tests and the
//! testkit guard (which checks every listed crate is locked).
//!
//! A row of `tests/state-versions.tsv` or `tests/derived-versions.tsv` lists
//! the Cargo.lock packages whose locked versions its stored bytes depend on.
//! The fingerprint is the first 12 hex digits of the sha256 over each listed
//! package as `name\0versions\0`, in listed order, with a package's locked
//! versions sorted and joined by `,`. A row listing `-` has none.
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// The package versions `lock` (a Cargo.lock) resolves, by package name,
/// each package's versions sorted.
fn locked(lock: &str) -> BTreeMap<&str, Vec<&str>> {
    let mut packages: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut name = None;
    for line in lock.lines() {
        if line == "[[package]]" {
            name = None;
        } else if let Some(rest) = line.strip_prefix("name = ") {
            name = Some(rest.trim_matches('"'));
        } else if let Some(rest) = line.strip_prefix("version = ")
            && let Some(package) = name.take()
        {
            packages
                .entry(package)
                .or_default()
                .push(rest.trim_matches('"'));
        }
    }
    for versions in packages.values_mut() {
        versions.sort_unstable();
    }
    packages
}

/// One table row as the cache keys it: the name, the version the parity
/// guard holds equal to the code's, and the fingerprint (`None` for a row
/// listing no packages).
pub type Keyed = (String, u32, Option<String>);

/// Every row of a versions table (`table`, its text). Fails on a malformed
/// row, a name listed twice, or a listed package the lock does not resolve.
pub fn fingerprints(lock: &str, table: &str) -> Result<Vec<Keyed>, String> {
    let locked = locked(lock);
    let mut seen = std::collections::BTreeSet::new();
    let mut rows = Vec::new();
    for line in table
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let fields: Vec<&str> = line.split('\t').collect();
        let [name, version, _sources, deps, _sha256] = fields[..] else {
            return Err(format!("malformed row {line:?}"));
        };
        let version = version
            .parse()
            .map_err(|_| format!("{name}: bad version {version:?}"))?;
        if !seen.insert(name) {
            return Err(format!("{name} is listed twice"));
        }
        if deps == "-" {
            rows.push((name.to_owned(), version, None));
            continue;
        }
        let mut hasher = Sha256::new();
        for dep in deps.split(',') {
            let versions = locked
                .get(dep)
                .ok_or_else(|| format!("{name}: listed dependency {dep} is not in Cargo.lock"))?;
            hasher.update(dep.as_bytes());
            hasher.update(b"\0");
            hasher.update(versions.join(",").as_bytes());
            hasher.update(b"\0");
        }
        let hex: String = hasher
            .finalize()
            .iter()
            .take(6)
            .map(|b| format!("{b:02x}"))
            .collect();
        rows.push((name.to_owned(), version, Some(hex)));
    }
    Ok(rows)
}
