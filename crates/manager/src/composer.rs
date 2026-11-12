//! `composer.json` `require` and `require-dev`, resolved from `composer.lock`.
//! Platform packages (`php`, `ext-*`, `lib-*`, Composer's APIs) are not
//! updated.
//!
//! Where each package comes from follows `crate::origins`: what
//! `composer.lock` records, else Packagist only when `composer.json`
//! configures no other repository. `composer.json` cannot say which
//! repository holds a package, so its repositories only decide whether an
//! unrecorded package can be Packagist's. A stale lock is Composer's to
//! catch: the update stage resolves with the real `composer.json`.
use crate::origins::{Named, Origin, Origins, Public};
use crate::*;
use jsonc_parser::common::Ranged;
use std::collections::BTreeSet;

fn platform(name: &str) -> bool {
    !name.contains('/')
        || name.starts_with("ext-")
        || name.starts_with("lib-")
        || matches!(name, "composer-plugin-api" | "composer-runtime-api")
}

fn locked(lock: Option<&str>) -> BTreeMap<String, String> {
    let Some(value) = lock.and_then(|l| serde_json::from_str::<serde_json::Value>(l).ok()) else {
        return BTreeMap::new();
    };
    ["packages", "packages-dev"]
        .iter()
        .filter_map(|k| value[k].as_array())
        .flatten()
        .filter_map(|p| {
            let version = p["version"].as_str()?;
            Some((
                p["name"].as_str()?.to_ascii_lowercase(),
                version.to_string(),
            ))
        })
        .collect()
}

pub(super) fn extract(
    path: &str,
    text: &Lines<'_>,
    files: &Files,
    skipped: &mut Vec<Skipped>,
) -> Result<Vec<Dependency>, String> {
    if path.rsplit('/').next() != Some("composer.json") {
        return Ok(Vec::new());
    }
    let root = jsonc_parser::parse_to_ast(text, &Default::default(), &Default::default())
        .map_err(|e| e.to_string())?
        .value
        .ok_or("empty composer.json")?;
    let object = root.as_object().ok_or("composer.json must be an object")?;
    let lock_path = format!(
        "{}composer.lock",
        path.strip_suffix("composer.json").unwrap_or("")
    );
    let mut origins = Origins::default();
    let mut lock_text = files.get(&lock_path);
    match manifest(path, text) {
        Ok(manifest) => {
            origins.configured = manifest.configured;
            origins.outside = manifest.inline;
            if manifest.unlocked {
                lock_text = None;
            }
        }
        Err(unread) => origins.uncertain.push(unread),
    }
    let lock_versions = locked(lock_text.and_then(|l| l.read().ok()));
    // An unread lock reads as one the Count cannot parse: lookups it could
    // steer fail.
    match lock_text.map(|l| l.read().ok().and_then(lock)) {
        None => {}
        Some(Some(Lock { recorded, plugins })) => {
            origins.locks.push(recorded);
            if plugins {
                origins.configured.push(Named::Unusable(format!(
                    "Composer plugins in {lock_path}, which can add repositories"
                )));
            }
        }
        Some(None) => origins.uncertain.push(Named::Unusable(format!(
            "{lock_path}, which the Count cannot read"
        ))),
    }
    let mut out = Vec::new();
    for (field, kind) in [
        ("require", DependencyKind::Production),
        ("require-dev", DependencyKind::Development),
    ] {
        let Some(requirements) = object.get_object(field) else {
            continue;
        };
        for prop in &requirements.properties {
            let name = prop.name.as_str();
            let Some(value) = prop.value.as_string_lit() else {
                continue;
            };
            let (start, end) = (value.start(), value.end());
            // Escapes, local `dev-` branches and `*` are not version pins.
            if platform(name)
                || end - start != value.value.len() + 2
                || value.value.starts_with("dev-")
                || value.value.trim() == "*"
            {
                continue;
            }
            out.push(Dependency {
                manager: Manager::Composer,
                file: path.into(),
                location: Location::new(
                    text,
                    start + 1,
                    end - 1,
                    format!("/{field}/{}", pointer_token(name)),
                ),
                form: EditForm::Scalar(Syntax::Inner),
                name: name.into(),
                package: name.to_ascii_lowercase(),
                kind,
                current: value.value.to_string(),
                resolved: lock_versions.get(&name.to_ascii_lowercase()).cloned(),
                digest: None,
                source: Source::Packagist,
                scheme: Scheme::Composer,
                extract: None,
                included_by: None,
                registry: None,
            });
        }
    }
    skipped.extend(origins::skipped(
        Manager::Composer,
        origins.assign(&mut out, &PACKAGIST),
        NOT_FROM_PACKAGIST,
    ));
    Ok(out)
}

/// Packagist, for `crate::origins`.
pub(crate) const PACKAGIST: Public = Public {
    source: Source::Packagist,
    label: "https://packagist.org",
    is_public: |url| {
        crate::web_host(url)
            .is_some_and(|host| matches!(host.as_str(), "packagist.org" | "repo.packagist.org"))
    },
};

/// Why a package not from a Composer registry is skipped.
const NOT_FROM_PACKAGIST: &str = "comes from a repository composer.lock does not record as a Composer registry (a path, VCS, inline package or artifact repository, or a registry without download notifications), so it is not looked up on Packagist";

/// What `composer.json` says about repositories.
#[derive(Default)]
struct Manifest {
    /// The repositories it configures besides Packagist, for
    /// `Origins::configured`.
    configured: Vec<Named>,
    /// Packages its inline `package` repositories define.
    inline: BTreeSet<String>,
    /// `config.lock` is false, so Composer ignores `composer.lock`.
    unlocked: bool,
}

/// Read `composer.json` as Composer does, as strict JSON: a file only a
/// JSON-with-comments parser takes is unreadable. Every repository entry
/// counts as configured but one that only turns Packagist on or names
/// Packagist plainly, and so does the merge plugin, which adds repositories
/// from other files.
fn manifest(path: &str, text: &str) -> Result<Manifest, Named> {
    let unread = || Named::Unusable(format!("{path}, which the Count cannot read as JSON"));
    let json = serde_json::from_str::<serde_json::Value>(text).map_err(|_| unread())?;
    let mut out = Manifest {
        unlocked: json.pointer("/config/lock") == Some(&serde_json::Value::Bool(false)),
        ..Manifest::default()
    };
    if json.pointer("/extra/merge-plugin").is_some() {
        out.configured.push(Named::Unusable(format!(
            "the merge plugin's files in {path}"
        )));
    }
    let entries: Vec<(Option<&str>, &serde_json::Value)> = match json.get("repositories") {
        None => return Ok(out),
        Some(serde_json::Value::Array(entries)) => entries.iter().map(|e| (None, e)).collect(),
        Some(serde_json::Value::Object(entries)) => entries
            .iter()
            .map(|(key, e)| (Some(key.as_str()), e))
            .collect(),
        Some(_) => return Err(unread()),
    };
    let packagist = |key: &str| matches!(key, "packagist.org" | "packagist");
    for (key, entry) in entries {
        // `"packagist.org": false`, as a keyed entry or an object of its own.
        let switch = match (key, entry) {
            (Some(key), serde_json::Value::Bool(on)) if packagist(key) => Some(*on),
            (_, serde_json::Value::Object(map)) if map.len() == 1 => map
                .iter()
                .next()
                .filter(|(key, _)| packagist(key))
                .and_then(|(_, on)| on.as_bool()),
            _ => None,
        };
        match switch {
            Some(true) => continue,
            Some(false) => {
                out.configured
                    .push(Named::Unusable(format!("Packagist turned off in {path}")));
                continue;
            }
            None => {}
        }
        let map = entry.as_object();
        // Packagist named plainly, without filters such as `only` or
        // `exclude` that change which packages it serves.
        let plain =
            map.is_some_and(|map| map.keys().all(|key| matches!(key.as_str(), "type" | "url")));
        let named = match entry.get("url").and_then(|url| url.as_str()) {
            Some(url) => Named::parse(url, &PACKAGIST),
            None => Named::Unusable(format!("a repository in {path}")),
        };
        if named == Named::Public && plain {
            continue;
        }
        out.configured.push(match named {
            Named::Public => Named::Unusable(format!("Packagist with filters in {path}")),
            named => named,
        });
        if entry.get("type").and_then(|t| t.as_str()) == Some("package") {
            let packages = match entry.get("package") {
                Some(serde_json::Value::Array(packages)) => packages.iter().collect(),
                Some(package) => vec![package],
                None => Vec::new(),
            };
            for package in packages {
                match package.get("name").and_then(|n| n.as_str()) {
                    Some(name) => {
                        out.inline.insert(name.to_ascii_lowercase());
                    }
                    None => return Err(unread()),
                }
            }
        }
    }
    Ok(out)
}

/// What `composer.lock` records.
struct Lock {
    recorded: BTreeMap<String, Origin>,
    /// Whether it holds a Composer plugin, which can add repositories.
    plugins: bool,
}

/// Read `composer.lock`. Composer marks a package from a Composer registry
/// with that registry's `notification-url`; Packagist's is
/// `https://packagist.org/downloads/`. A path package, one downloaded from a
/// local file (an artifact), or one with no `notification-url` (from a VCS
/// or inline repository) does not come from a registry. None when the file
/// is not a lock the Count can read.
fn lock(lock: &str) -> Option<Lock> {
    let lock = serde_json::from_str::<serde_json::Value>(lock).ok()?;
    lock.as_object()?;
    let mut origins = BTreeMap::<String, Vec<Origin>>::new();
    let mut plugins = false;
    for key in ["packages", "packages-dev"] {
        // `composer update --no-dev` writes a null `packages-dev`.
        let packages = match lock.get(key) {
            None | Some(serde_json::Value::Null) => continue,
            Some(packages) => packages.as_array()?,
        };
        for package in packages {
            let name = package.get("name")?.as_str()?.to_ascii_lowercase();
            let text = |pointer: &str| package.pointer(pointer).and_then(|v| v.as_str());
            plugins |= text("/type") == Some("composer-plugin");
            let local = [text("/dist/type"), text("/source/type")].contains(&Some("path"))
                || text("/dist/url").is_some_and(|url| crate::web_host(url).is_none());
            let origin = if local {
                Origin::Other
            } else {
                match package.get("notification-url") {
                    None => Origin::Other,
                    Some(serde_json::Value::String(url)) if (PACKAGIST.is_public)(url) => {
                        Origin::Index(Named::Public)
                    }
                    Some(serde_json::Value::String(url)) => Origin::Index(Named::Unusable(
                        format!("the Composer repository of {}", crate::redacted(url)),
                    )),
                    Some(_) => return None,
                }
            };
            origins.entry(name).or_default().push(origin);
        }
    }
    let recorded = origins
        .into_iter()
        .filter_map(|(name, entries)| Some((name, Origin::of(entries)?)))
        .collect();
    Some(Lock { recorded, plugins })
}

/// Whether `text` parses as `composer.lock` and holds a Composer plugin
/// package, which can add repositories and run code the file does not
/// name.
pub(crate) fn lock_has_plugins(text: &str) -> Option<bool> {
    lock(text).map(|lock| lock.plugins)
}

pub(crate) static DESCRIPTOR: Descriptor = Descriptor;

pub(crate) struct Descriptor;

impl crate::Ecosystem for Descriptor {
    fn proposing(&self, dep: &Dependency) -> Proposing {
        Proposing {
            range: Range::Installed,
            runtime_toolchain: false,
            written: Written::Range,
            keeps_satisfied: false,
            unversioned: Unversioned {
                looked_up: false,
                opaque: false,
            },
            short_tags: ShortTags::Fixed,
            selector_resolved: false,
            resolved_via_listing: dep.kind == DependencyKind::Tool,
            pin: Pin {
                commit: false,
                needs_digest: matches!(dep.source, Source::Docker | Source::Oci)
                    && matches!(
                        dep.kind,
                        DependencyKind::Action
                            | DependencyKind::ReusableWorkflow
                            | DependencyKind::ContainerImage
                            | DependencyKind::Chart
                    ),
                from_catalog: false,
            },
            lock: crate::Lock {
                keeps_ranges: true,
                movable: dep.source == Source::Packagist,
                transitive_movable: dep.kind == DependencyKind::Transitive
                    && dep.source == Source::Crates,
                reported: !matches!(dep.kind, DependencyKind::Toolchain | DependencyKind::Tool)
                    && dep.resolved.is_some()
                    && dep.source == Source::Packagist,
            },
            edits: Edits::Manifest,
            advisories: Advisories::BySource,
        }
    }
    fn selected(&self, _config: &Config, path: &str) -> bool {
        matches!(
            path.rsplit('/').next().unwrap_or(path),
            "composer.json" | "composer.lock"
        )
    }
    fn registry_file(&self, path: &str) -> bool {
        crate::roles::eco_roles(Manager::Composer, path).registry
    }
    fn prepare<'a>(
        &self,
        _scanner: &'a Scanner,
        files: &'a Files,
        _shared: &'a Shared<'a>,
    ) -> Result<Prepared<'a>, String> {
        Ok(Prepared {
            extract: Some(Box::new(move |path, text, out| {
                let result = extract(path, text, files, &mut out.skipped);
                file_result(out, path, result);
                Ok(())
            })),
            finish: None,
        })
    }
}
