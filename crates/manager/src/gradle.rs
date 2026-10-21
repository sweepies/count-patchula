//! Gradle: version catalogs (`libs.versions.toml`), literal coordinates and
//! plugin versions in Gradle scripts, and the wrapper distribution. Versions
//! held in variables are resolved by `gradle_vars`.
use crate::*;
use regex::Regex;
use std::{collections::BTreeSet, sync::LazyLock};
use toml_edit::{Document, Item, Value};

const PORTAL: &str = "https://plugins.gradle.org/m2";

pub(super) fn plugin(id: &str) -> String {
    format!("{PORTAL}|{id}:{id}.gradle.plugin")
}
pub(super) fn maven(
    path: &str,
    text: &str,
    package: String,
    kind: DependencyKind,
    start: usize,
    version: &str,
    pointer: String,
) -> Dependency {
    Dependency {
        manager: Manager::Gradle,
        file: path.into(),
        location: Location::new(text, start, start + version.len(), pointer),
        name: package
            .rsplit_once('|')
            .map_or(package.as_str(), |(_, c)| c)
            .to_string(),
        package,
        kind,
        current: version.into(),
        resolved: Some(version.into()),
        digest: None,
        source: Source::Maven,
        scheme: Scheme::Maven,
        extract: None,
    }
}
/// Gradle dynamic and rich versions are not pins.
pub(super) fn literal(version: &str) -> bool {
    !version.is_empty()
        && version.starts_with(|c: char| c.is_ascii_digit())
        && !version.contains(['+', '$', ' ', '[', '(', ','])
}
/// The inner span of a TOML string value without escapes.
fn inner(text: &str, value: &Value) -> Option<(usize, String)> {
    let span = value.span()?;
    let raw = &text[span.clone()];
    let string = value.as_str()?;
    (matches!(raw.chars().next(), Some('"' | '\'')) && raw.len() == string.len() + 2)
        .then(|| (span.start + 1, string.to_string()))
}

fn catalog(path: &str, text: &str) -> Result<Vec<Dependency>, String> {
    let doc = Document::parse(text).map_err(|e| e.to_string())?;
    let versions = doc.get("versions").and_then(Item::as_table_like);
    let mut claimed = BTreeSet::new();
    let mut out = Vec::new();
    // A version.ref is edited in [versions], once, for its first user.
    let mut reference = |name: &str, package: String, kind, out: &mut Vec<Dependency>| {
        if !claimed.insert(name.to_string()) {
            return;
        }
        let Some(value) = versions.and_then(|v| v.get(name)).and_then(Item::as_value) else {
            return;
        };
        if let Some((start, version)) = inner(text, value).filter(|(_, v)| literal(v)) {
            out.push(maven(
                path,
                text,
                package,
                kind,
                start,
                &version,
                format!("/versions/{name}"),
            ));
        }
    };
    for (table, kind) in [
        ("libraries", DependencyKind::Production),
        ("plugins", DependencyKind::Build),
    ] {
        let Some(entries) = doc.get(table).and_then(Item::as_table_like) else {
            continue;
        };
        for (alias, entry) in entries.iter() {
            let pointer = format!("/{table}/{alias}");
            if let Some(value) = entry.as_value().filter(|v| v.is_str()) {
                // "group:artifact:version" or "plugin.id:version"
                let Some((start, coordinate)) = inner(text, value) else {
                    continue;
                };
                let Some((module, version)) = coordinate.rsplit_once(':') else {
                    continue;
                };
                if !literal(version) {
                    continue;
                }
                let package = if table == "plugins" {
                    plugin(module)
                } else if module.contains(':') {
                    module.to_string()
                } else {
                    continue;
                };
                out.push(maven(
                    path,
                    text,
                    package,
                    kind,
                    start + module.len() + 1,
                    version,
                    pointer,
                ));
                continue;
            }
            let package = if table == "plugins" {
                match entry.get("id").and_then(Item::as_str) {
                    Some(id) => plugin(id),
                    None => continue,
                }
            } else if let Some(module) = entry.get("module").and_then(Item::as_str) {
                module.to_string()
            } else if let (Some(group), Some(name)) = (
                entry.get("group").and_then(Item::as_str),
                entry.get("name").and_then(Item::as_str),
            ) {
                format!("{group}:{name}")
            } else {
                continue;
            };
            let Some(version) = entry.get("version") else {
                continue;
            };
            if let Some(name) = version.get("ref").and_then(Item::as_str) {
                reference(name, package, kind, &mut out);
            } else if let Some((start, v)) = version
                .as_value()
                .and_then(|v| inner(text, v))
                .filter(|(_, v)| literal(v))
            {
                out.push(maven(
                    path,
                    text,
                    package,
                    kind,
                    start,
                    &v,
                    format!("{pointer}/version"),
                ));
            }
        }
    }
    Ok(out)
}

fn script(path: &str, text: &str) -> Vec<Dependency> {
    // Commented-out declarations are not dependencies.
    let text = &crate::gradle_vars::mask(text);
    static COORDINATE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"["']([A-Za-z0-9_.-]+):([A-Za-z0-9_.-]+):([0-9][A-Za-z0-9_.+-]*)["']"#)
            .expect("coordinate pattern")
    });
    static PLUGIN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"\bid\s*\(?\s*["']([A-Za-z0-9_.-]+)["']\s*\)?\s*version\s*\(?\s*["']([0-9][A-Za-z0-9_.+-]*)["']"#)
            .expect("plugin pattern")
    });
    static NOTATION: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"\bgroup\s*[:=]\s*["']([A-Za-z0-9_.-]+)["']\s*,\s*name\s*[:=]\s*["']([A-Za-z0-9_.-]+)["']\s*,\s*version\s*[:=]\s*["']([0-9][A-Za-z0-9_.+-]*)["']"#)
            .expect("notation pattern")
    });
    let mut out = Vec::new();
    for c in NOTATION.captures_iter(text) {
        let version = c.get(3).unwrap();
        if literal(version.as_str()) {
            let package = format!("{}:{}", &c[1], &c[2]);
            out.push(maven(
                path,
                text,
                package.clone(),
                DependencyKind::Production,
                version.start(),
                version.as_str(),
                format!("/{package}"),
            ));
        }
    }
    for c in COORDINATE.captures_iter(text) {
        let version = c.get(3).unwrap();
        // A group and artifact name something; "2020-07-01T12:34:56Z" does not.
        let named = |s: &str| s.bytes().any(|b| b.is_ascii_alphabetic());
        if literal(version.as_str()) && named(&c[1]) && named(&c[2]) {
            let package = format!("{}:{}", &c[1], &c[2]);
            out.push(maven(
                path,
                text,
                package.clone(),
                DependencyKind::Production,
                version.start(),
                version.as_str(),
                format!("/{package}"),
            ));
        }
    }
    for c in PLUGIN.captures_iter(text) {
        let version = c.get(2).unwrap();
        if literal(version.as_str()) {
            out.push(maven(
                path,
                text,
                plugin(&c[1]),
                DependencyKind::Build,
                version.start(),
                version.as_str(),
                format!("/plugins/{}", &c[1]),
            ));
        }
    }
    out
}

/// The wrapper's distribution version. A recorded `distributionSha256Sum`
/// is refreshed by the lockfile runner from Gradle's published checksum.
fn wrapper(path: &str, text: &str) -> Vec<Dependency> {
    static DISTRIBUTION: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?m)^distributionUrl=.*/gradle-([0-9][0-9.]*(?:-rc-[0-9]+)?)-(?:bin|all)\.zip\s*$",
        )
        .expect("distribution pattern")
    });
    let Some(version) = DISTRIBUTION.captures(text).and_then(|c| c.get(1)) else {
        return Vec::new();
    };
    let (source, package, extract) = rustovate_mise::resolve("gradle").map_or_else(
        || (Source::Mise, "gradle".to_string(), None),
        |r| (r.source, r.package, r.extract),
    );
    vec![Dependency {
        manager: Manager::Gradle,
        file: path.into(),
        location: Location::new(
            text,
            version.start(),
            version.end(),
            "/distributionUrl".into(),
        ),
        name: "gradle".into(),
        package,
        kind: DependencyKind::Toolchain,
        current: version.as_str().into(),
        resolved: Some(version.as_str().into()),
        digest: None,
        source,
        scheme: Scheme::Loose,
        extract,
    }]
}

pub(super) fn selected(path: &str) -> bool {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    // Under a source tree only main sources are build logic (such as
    // buildSrc/src/main/kotlin/*.gradle.kts); test resources and docs hold
    // fixtures that pin old versions on purpose.
    if let Some(index) = path
        .find("src/")
        .filter(|i| *i == 0 || path.as_bytes()[i - 1] == b'/')
        && !path[index..].starts_with("src/main/")
    {
        return false;
    }
    leaf.ends_with(".versions.toml") && path.contains("gradle/")
        || leaf.ends_with(".gradle")
        || leaf.ends_with(".gradle.kts")
        || leaf == "gradle.properties"
        || path.ends_with("gradle/wrapper/gradle-wrapper.properties")
}

pub(super) fn extract(path: &str, text: &str) -> Result<Vec<Dependency>, String> {
    if path.ends_with(".versions.toml") {
        catalog(path, text)
    } else if path.ends_with("gradle-wrapper.properties") {
        Ok(wrapper(path, text))
    } else if path.ends_with("gradle.properties") {
        // Properties are only versions through a reference; see gradle_vars.
        Ok(Vec::new())
    } else {
        Ok(script(path, text))
    }
}
