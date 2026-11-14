//! Dependency fingerprints for the code-versioned cache keys, shared by the
//! cache crate's build script (which emits them), its unit tests and the
//! testkit guard (which checks every listed crate is locked).
//!
//! A row of `tests/state-versions.tsv` or `tests/derived-versions.tsv` lists
//! the Cargo.lock packages whose locked versions its stored bytes depend on.
//! The fingerprint is the first 12 hex digits of the sha256 over each listed
//! package as `name\0versions\0`, in listed order, with a package's locked
//! versions sorted and joined by `,`. A row listing `-` has none.
//!
//! Only packages Cargo.lock gives a `source` (registry or git) may be
//! listed. A workspace or path crate is locked at the workspace version,
//! which moves every release and says nothing about its code: such a crate
//! belongs in the row's sources.
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// A Cargo.lock's packages by name: the versions it resolves from a
/// `source` (registry or git), sorted, and whether any entry of that name
/// has no source (a workspace or path crate).
struct Locked<'a> {
    versions: BTreeMap<&'a str, Vec<&'a str>>,
    local: BTreeSet<&'a str>,
}

fn locked(lock: &str) -> Locked<'_> {
    let mut locked = Locked {
        versions: BTreeMap::new(),
        local: BTreeSet::new(),
    };
    // Each `[[package]]` entry: its name, version and whether it has a
    // source. Another table (`[[patch.unused]]`, `[metadata]`) ends it.
    let mut entries: Vec<(Option<&str>, Option<&str>, bool)> = Vec::new();
    let mut in_package = false;
    for line in lock.lines() {
        if line.starts_with('[') {
            in_package = line == "[[package]]";
            if in_package {
                entries.push((None, None, false));
            }
        } else if in_package && let Some(entry) = entries.last_mut() {
            if let Some(rest) = line.strip_prefix("name = ") {
                entry.0 = Some(rest.trim_matches('"'));
            } else if let Some(rest) = line.strip_prefix("version = ") {
                entry.1 = Some(rest.trim_matches('"'));
            } else if line.starts_with("source = ") {
                entry.2 = true;
            }
        }
    }
    for (name, version, sourced) in entries {
        let (Some(name), Some(version)) = (name, version) else {
            continue;
        };
        if sourced {
            locked.versions.entry(name).or_default().push(version);
        } else {
            locked.local.insert(name);
        }
    }
    for versions in locked.versions.values_mut() {
        versions.sort_unstable();
    }
    locked
}

/// One table row as the cache keys it: the name, the version the parity
/// guard holds equal to the code's, and the fingerprint (`None` for a row
/// listing no packages).
pub type Keyed = (String, u32, Option<String>);

/// Every row of a versions table (`table`, its text). Fails on a malformed
/// row, a name listed twice, or a listed package the lock does not resolve.
pub fn fingerprints(lock: &str, table: &str) -> Result<Vec<Keyed>, String> {
    let locked = locked(lock);
    let mut seen = BTreeSet::new();
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
            if locked.local.contains(dep) {
                return Err(format!(
                    "{name}: listed dependency {dep} is a workspace or path crate, locked at \
                     the workspace version rather than by its code — list its sources instead"
                ));
            }
            let versions = locked
                .versions
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
