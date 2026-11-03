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

fn platform(name: &str) -> bool {
    !name.contains('/')
        || name.starts_with("ext-")
        || name.starts_with("lib-")
        || matches!(name, "composer-plugin-api" | "composer-runtime-api")
}

fn locked(lock: Option<&String>) -> BTreeMap<String, String> {
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
    text: &str,
    files: &Files,
    skipped: &mut Vec<Skipped>,
) -> Result<Vec<Dependency>, String> {
    if !path.ends_with("composer.json") {
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
    let lock_text = files.get(&lock_path);
    let lock = locked(lock_text);
    let mut origins = Origins::default();
    match configured(path, text) {
        Ok(configured) => origins.configured = configured,
        Err(unread) => origins.uncertain.push(unread),
    }
    match lock_text.map(|l| lock_origins(l)) {
        None => {}
        Some(Some(recorded)) => origins.locks.push(recorded),
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
                    format!("/{field}/{}", name.replace('/', "~1")),
                ),
                name: name.into(),
                package: name.to_ascii_lowercase(),
                kind,
                current: value.value.to_string(),
                resolved: lock.get(&name.to_ascii_lowercase()).cloned(),
                digest: None,
                source: Source::Packagist,
                scheme: Scheme::Composer,
                extract: None,
                included_by: None,
                registry: None,
            });
        }
    }
    for dep in origins.assign(&mut out, &PACKAGIST, |dep| dep.package.clone()) {
        skipped.push(Skipped {
            manager: Manager::Composer,
            file: dep.file,
            line: Some(dep.location.line),
            value: dep.name,
            reason: NOT_FROM_PACKAGIST.into(),
        });
    }
    Ok(out)
}

/// Packagist, for `crate::origins`.
const PACKAGIST: Public = Public {
    source: Source::Packagist,
    label: "https://packagist.org",
    is_public: |url| {
        crate::web_host(url)
            .is_some_and(|host| matches!(host.as_str(), "packagist.org" | "repo.packagist.org"))
    },
};

/// Why a package from a path, VCS or inline repository is skipped.
const NOT_FROM_PACKAGIST: &str = "comes from a path, VCS or other repository that composer.lock does not record as a Composer registry, so it is not looked up on Packagist";

/// The repositories `composer.json` configures besides Packagist, for
/// `Origins::configured`: every entry but one that only turns Packagist on.
/// Composer reads strict JSON, so a file only a JSON-with-comments parser
/// takes is unreadable.
fn configured(path: &str, text: &str) -> Result<Vec<Named>, Named> {
    let unread = || Named::Unusable(format!("{path}, which the Count cannot read as JSON"));
    let json = serde_json::from_str::<serde_json::Value>(text).map_err(|_| unread())?;
    let entries: Vec<(Option<&str>, &serde_json::Value)> = match json.get("repositories") {
        None => return Ok(Vec::new()),
        Some(serde_json::Value::Array(entries)) => entries.iter().map(|e| (None, e)).collect(),
        Some(serde_json::Value::Object(entries)) => entries
            .iter()
            .map(|(key, e)| (Some(key.as_str()), e))
            .collect(),
        Some(_) => return Err(unread()),
    };
    let packagist = |key: &str| matches!(key, "packagist.org" | "packagist");
    let mut out = Vec::new();
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
            Some(true) => {}
            Some(false) => out.push(Named::Unusable(format!("Packagist turned off in {path}"))),
            None => out.push(match entry.get("url").and_then(|url| url.as_str()) {
                Some(url) => Named::parse(url, &PACKAGIST),
                None => Named::Unusable(format!("a repository in {path}")),
            }),
        }
    }
    out.retain(|named| *named != Named::Public);
    Ok(out)
}

/// What `composer.lock` records of each package. Composer marks a package
/// from a Composer registry with that registry's `notification-url`;
/// Packagist's is `https://packagist.org/downloads/`. A path package, or one
/// with no `notification-url` (from a VCS, inline or artifact repository),
/// does not come from a registry. None when the file is not a lock the Count
/// can read.
fn lock_origins(lock: &str) -> Option<BTreeMap<String, Origin>> {
    let lock = serde_json::from_str::<serde_json::Value>(lock).ok()?;
    let mut origins = BTreeMap::<String, Vec<Origin>>::new();
    for key in ["packages", "packages-dev"] {
        let packages = match lock.get(key) {
            None => continue,
            Some(packages) => packages.as_array()?,
        };
        for package in packages {
            let name = package.get("name")?.as_str()?.to_ascii_lowercase();
            let kind = |field: &str| package.get(field).and_then(|f| f.get("type"));
            let origin = if [kind("dist"), kind("source")]
                .iter()
                .any(|t| t.and_then(|t| t.as_str()) == Some("path"))
            {
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
    Some(
        origins
            .into_iter()
            .filter_map(|(name, entries)| Some((name, Origin::of(entries)?)))
            .collect(),
    )
}
