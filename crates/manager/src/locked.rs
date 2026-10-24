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
