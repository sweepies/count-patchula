//! Bundler: `gem` requirements and the `ruby` directive in a Gemfile, resolved
//! from `Gemfile.lock`, plus `.ruby-version`.
use crate::*;
use regex::Regex;
use std::sync::LazyLock;

fn ruby(path: &str, text: &str, start: usize, version: &str, pointer: &str) -> Dependency {
    Dependency {
        manager: Manager::Bundler,
        file: path.into(),
        location: Location::new(text, start, start + version.len(), pointer.into()),
        name: "ruby".into(),
        package: "ruby".into(),
        kind: DependencyKind::Toolchain,
        current: version.into(),
        resolved: Some(version.into()),
        digest: None,
        source: Source::Mise,
        scheme: Scheme::Loose,
        extract: None,
        included_by: None,
    }
}
fn numeric(version: &str) -> bool {
    !version.is_empty()
        && version
            .split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// `name (version)` entries under each `specs:` section, without platforms.
fn locked(lock: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut specs = false;
    for line in lock.lines() {
        if !line.starts_with(' ') {
            specs = false;
            continue;
        }
        if line.trim() == "specs:" {
            specs = true;
            continue;
        }
        // Exactly four spaces: top-level specs, not their dependencies.
        if specs
            && line.starts_with("    ")
            && !line.starts_with("     ")
            && let Some((name, rest)) = line.trim().split_once(" (")
            && let Some(version) = rest.strip_suffix(')')
        {
            let version = version.split('-').next().unwrap_or(version);
            out.entry(name.to_string())
                .or_insert_with(|| version.to_string());
        }
    }
    out
}

pub(super) fn extract(path: &str, text: &str, files: &Files) -> Result<Vec<Dependency>, String> {
    if path.ends_with(".ruby-version") {
        let Some(token) = text.split_whitespace().next() else {
            return Ok(Vec::new());
        };
        let version = token.strip_prefix("ruby-").unwrap_or(token);
        if !numeric(version) {
            return Ok(Vec::new());
        }
        let start = text.find(token).expect("token from text") + (token.len() - version.len());
        return Ok(vec![ruby(path, text, start, version, "/ruby")]);
    }
    if !path.ends_with("Gemfile") {
        return Ok(Vec::new());
    }
    static GEM: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?m)^[ \t]*gem[ \t(]+["']([A-Za-z0-9_.-]+)["'][ \t]*,[ \t]*["']([^"'\n]+)["'](?:[ \t]*,[ \t]*["']([^"'\n]+)["'])?([^\n]*)$"#)
            .expect("gem pattern")
    });
    static RUBY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?m)^[ \t]*ruby[ \t(]+["']([0-9][0-9.]*)["']"#).expect("ruby pattern")
    });
    let lock = files
        .get(&format!(
            "{}Gemfile.lock",
            path.strip_suffix("Gemfile").unwrap_or("")
        ))
        .map(|l| locked(l))
        .unwrap_or_default();
    let mut out = Vec::new();
    for c in GEM.captures_iter(text) {
        let rest = c.get(4).map_or("", |m| m.as_str());
        if ["git:", "path:", "github:", ":git", ":path", ":github"]
            .iter()
            .any(|k| rest.contains(k))
        {
            continue;
        }
        // Two requirement strings form a compound requirement, which is
        // not rewritten.
        if c.get(3).is_some() {
            continue;
        }
        let name = &c[1];
        let first = c.get(2).unwrap();
        let current = first.as_str().to_string();
        out.push(Dependency {
            manager: Manager::Bundler,
            file: path.into(),
            location: Location::new(text, first.start(), first.end(), format!("/gems/{name}")),
            name: name.into(),
            package: name.into(),
            kind: DependencyKind::Production,
            current,
            resolved: lock.get(name).cloned(),
            digest: None,
            source: Source::Rubygems,
            scheme: Scheme::Rubygems,
            extract: None,
            included_by: None,
        });
    }
    if let Some(version) = RUBY
        .captures(text)
        .and_then(|c| c.get(1))
        .filter(|v| numeric(v.as_str()))
    {
        out.push(ruby(path, text, version.start(), version.as_str(), "/ruby"));
    }
    Ok(out)
}
