//! Packages a lockfile records that no manifest declares. They are not
//! dependencies to update, only versions to check for advisories.
use crate::*;
use jsonc_parser::ast::Value;
use toml_edit::{Document, Item};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Locked {
    pub manager: Manager,
    /// The lockfile.
    pub file: String,
    pub package: String,
    pub version: String,
    pub location: Location,
}
impl Locked {
    /// The package as a `transitive` dependency of its lockfile.
    pub fn dependency(&self) -> Dependency {
        Dependency {
            manager: self.manager,
            file: self.file.clone(),
            location: self.location.clone(),
            name: self.package.clone(),
            package: self.package.clone(),
            kind: DependencyKind::Transitive,
            current: self.version.clone(),
            resolved: Some(self.version.clone()),
            digest: None,
            source: if self.manager == Manager::Cargo {
                Source::Crates
            } else {
                Source::Npm
            },
            scheme: Scheme::Semver,
            extract: None,
        }
    }
}

fn direct(
    dependencies: &[Dependency],
    manager: Manager,
    lock: &str,
    package: &str,
    version: &str,
) -> bool {
    let root = lock.rsplit_once('/').map_or("", |(dir, _)| dir);
    dependencies.iter().any(|d| {
        d.manager == manager
            && d.package == package
            && d.resolved.as_deref() == Some(version)
            && (root.is_empty() || d.file.starts_with(&format!("{root}/")))
    })
}

/// crates.io packages in each `Cargo.lock`.
pub(crate) fn cargo(
    files: &Files,
    documents: &BTreeMap<&str, Document<&str>>,
    dependencies: &[Dependency],
) -> Vec<Locked> {
    let mut out = Vec::new();
    for (path, doc) in documents.iter().filter(|(p, _)| p.ends_with("Cargo.lock")) {
        let Some(packages) = doc.get("package").and_then(Item::as_array_of_tables) else {
            continue;
        };
        for package in packages {
            let (Some(name), Some(version)) = (
                package.get("name").and_then(Item::as_str),
                package.get("version"),
            ) else {
                continue;
            };
            let registry = package
                .get("source")
                .and_then(Item::as_str)
                .is_some_and(|s| {
                    s.starts_with("registry+https://github.com/rust-lang/crates.io-index")
                        || s.starts_with("sparse+https://index.crates.io/")
                });
            let (Some(text), Some(span)) = (version.as_str(), version.span()) else {
                continue;
            };
            if !registry || direct(dependencies, Manager::Cargo, path, name, text) {
                continue;
            }
            out.push(Locked {
                manager: Manager::Cargo,
                file: (*path).into(),
                package: name.into(),
                version: text.into(),
                location: Location::new(
                    &files[*path],
                    span.start,
                    span.end,
                    format!("/package/{name}@{text}"),
                ),
            });
        }
    }
    out
}

/// Registry packages under `node_modules` in each `package-lock.json`.
pub(crate) fn npm(
    files: &Files,
    locks: &BTreeMap<&str, Value<'_>>,
    dependencies: &[Dependency],
) -> Vec<Locked> {
    let mut out = Vec::new();
    for (path, root) in locks
        .iter()
        .filter(|(p, _)| p.ends_with("package-lock.json"))
    {
        let Some(packages) = root.as_object().and_then(|v| v.get_object("packages")) else {
            continue;
        };
        for entry in &packages.properties {
            let key = entry.name.as_str();
            let Some((_, name)) = key.rsplit_once("node_modules/") else {
                continue;
            };
            let Some(object) = entry.value.as_object() else {
                continue;
            };
            let Some(version) = object.get_string("version") else {
                continue;
            };
            let from_registry = object
                .get_string("resolved")
                .is_none_or(|r| r.value.starts_with("https://"));
            if object.get_boolean("link").is_some_and(|l| l.value)
                || !from_registry
                || direct(dependencies, Manager::Npm, path, name, &version.value)
                || out.iter().any(|l: &Locked| {
                    l.file == *path && l.package == name && l.version == version.value
                })
            {
                continue;
            }
            out.push(Locked {
                manager: Manager::Npm,
                file: (*path).into(),
                package: name.into(),
                version: version.value.to_string(),
                location: Location::new(
                    &files[*path],
                    version.range.start,
                    version.range.end,
                    format!("/packages/{}", key.replace('~', "~0").replace('/', "~1")),
                ),
            });
        }
    }
    out
}

/// What an `aube-lock.yaml` records: the version each importer's manifest
/// dependency resolved to, and the registry packages under it.
pub(crate) struct Aube {
    /// `(manifest, dependency name)` to the locked version.
    pub(crate) direct: BTreeMap<(String, String), String>,
    pub(crate) transitive: Vec<(String, String, String, Location)>,
}

/// The version in an importer entry or a `packages` key, without peer
/// suffixes such as `(astro@7.4.2)`, and the real version of an alias
/// (`string-width@4.2.3`).
fn plain(version: &str) -> &str {
    let version = version.split('(').next().unwrap_or(version);
    if version.starts_with(|c: char| c.is_ascii_digit()) {
        version
    } else {
        version.rsplit_once('@').map_or(version, |(_, v)| v)
    }
}

/// Reads every `aube-lock.yaml` (pnpm's v9 format: `importers` for each
/// manifest, `packages` for what they resolve to).
fn read(files: &Files) -> Result<Aube, String> {
    let mut out = Aube {
        direct: BTreeMap::new(),
        transitive: Vec::new(),
    };
    for (path, text) in files
        .iter()
        .filter(|(p, _)| p.rsplit('/').next() == Some("aube-lock.yaml"))
    {
        let lock: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(text).map_err(|e| format!("{path}: {e}"))?;
        let dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
        let join = |importer: &str| match (dir, importer) {
            ("", ".") => "package.json".to_owned(),
            ("", importer) => format!("{importer}/package.json"),
            (dir, ".") => format!("{dir}/package.json"),
            (dir, importer) => format!("{dir}/{importer}/package.json"),
        };
        if let Some(importers) = lock.get("importers").and_then(|v| v.as_mapping()) {
            for (importer, entry) in importers {
                let Some(importer) = importer.as_str() else {
                    continue;
                };
                for field in ["dependencies", "devDependencies", "optionalDependencies"] {
                    let Some(deps) = entry.get(field).and_then(|v| v.as_mapping()) else {
                        continue;
                    };
                    for (name, locked) in deps {
                        let version = locked
                            .get("version")
                            .or(Some(locked))
                            .and_then(|v| v.as_str());
                        if let (Some(name), Some(version)) = (name.as_str(), version) {
                            out.direct
                                .insert((join(importer), name.into()), plain(version).into());
                        }
                    }
                }
            }
        }
        if let Some(packages) = lock.get("packages").and_then(|v| v.as_mapping()) {
            for (key, entry) in packages {
                let Some(key) = key.as_str() else { continue };
                let registry = entry.get("resolution").is_some_and(|r| {
                    r.get("integrity").is_some()
                        && r.get("type").is_none()
                        && r.get("tarball").is_none()
                });
                // `name@version`, where a scoped name starts with `@`.
                let Some((name, version)) = key
                    .char_indices()
                    .skip(1)
                    .find(|(_, c)| *c == '@')
                    .map(|(n, _)| (&key[..n], &key[n + 1..]))
                else {
                    continue;
                };
                let Some(start) = text
                    .find(&format!("'{key}':"))
                    .map(|n| n + 1)
                    .or_else(|| text.find(&format!("\n  {key}:")).map(|n| n + 3))
                else {
                    continue;
                };
                if registry {
                    out.transitive.push((
                        (*path).clone(),
                        name.into(),
                        plain(version).into(),
                        Location::new(
                            text,
                            start,
                            start + key.len(),
                            format!("/packages/{}", key.replace('~', "~0").replace('/', "~1")),
                        ),
                    ));
                }
            }
        }
    }
    Ok(out)
}

/// Gives each manifest dependency the version an `aube-lock.yaml` beside it
/// locked, unless another lockfile already did, and lists the registry
/// packages the lock records that no manifest declares.
pub(crate) fn aube(
    files: &Files,
    dependencies: &mut [Dependency],
    manager: Manager,
    transitive: bool,
) -> Result<Vec<Locked>, String> {
    let lock = read(files)?;
    for dep in dependencies
        .iter_mut()
        .filter(|d| d.manager == manager && d.resolved.is_none())
    {
        dep.resolved = lock
            .direct
            .get(&(dep.file.clone(), dep.name.clone()))
            .cloned();
    }
    let mut out: Vec<Locked> = Vec::new();
    for (file, package, version, location) in lock.transitive {
        if !transitive
            || direct(dependencies, manager, &file, &package, &version)
            || out
                .iter()
                .any(|l| l.file == file && l.package == package && l.version == version)
        {
            continue;
        }
        out.push(Locked {
            manager,
            file,
            package,
            version,
            location,
        });
    }
    Ok(out)
}
