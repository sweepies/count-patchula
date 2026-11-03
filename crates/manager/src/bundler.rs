//! Bundler: `gem` requirements and the `ruby` directive in a Gemfile, resolved
//! from `Gemfile.lock`, plus `.ruby-version`. Where each gem comes from follows
//! `crate::origins`: its `source`, else `Gemfile.lock`, else RubyGems only
//! when the Gemfile names no other source.
use crate::origins::{Named, Origin, Origins, Public};
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
        registry: None,
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

/// Why a gem from git, a path or GitHub is skipped.
const NOT_FROM_A_SOURCE: &str =
    "comes from git, a path or GitHub, not a gem source, so it is not looked up on RubyGems";

pub(super) fn extract(
    path: &str,
    text: &str,
    files: &Files,
    skipped: &mut Vec<Skipped>,
) -> Result<Vec<Dependency>, String> {
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
    static GEM_SOURCE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?:\bsource:|:source[ \t]*=>)[ \t]*(["'][^"'\n]*["']|[^,\s)]+)"#)
            .expect("gem source pattern")
    });
    static RUBY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?m)^[ \t]*ruby[ \t(]+["']([0-9][0-9.]*)["']"#).expect("ruby pattern")
    });
    let lock_text = files.get(&format!(
        "{}Gemfile.lock",
        path.strip_suffix("Gemfile").unwrap_or("")
    ));
    let lock = lock_text.map(|l| locked(l)).unwrap_or_default();
    let Gemfile {
        mut origins,
        blocks,
    } = gemfile(path, text);
    origins.locks.extend(lock_text.map(|l| lock_origins(l)));
    let mut out = Vec::new();
    for c in GEM.captures_iter(text) {
        let rest = c.get(4).map_or("", |m| m.as_str());
        let name = &c[1];
        let line = text[..c.get(0).unwrap().start()].matches('\n').count();
        if ["git:", "path:", "github:", ":git", ":path", ":github"]
            .iter()
            .any(|k| rest.contains(k))
        {
            origins.outside.insert(name.to_owned());
            skipped.push(Skipped {
                manager: Manager::Bundler,
                file: path.into(),
                line: Some(line + 1),
                value: name.into(),
                reason: NOT_FROM_A_SOURCE.into(),
            });
            continue;
        }
        // The gem's own `source:` option, else the `source` block around it.
        let declared = match GEM_SOURCE.captures(rest) {
            Some(option) => Some(source_value(&option[1]).unwrap_or_else(|| {
                Named::Unusable(format!("a source in {path} the Count cannot read"))
            })),
            None => blocks.get(line).cloned().flatten(),
        };
        if let Some(named) = declared {
            origins
                .declared
                .insert(name.to_owned(), Origin::Index(named));
        }
        // Two requirement strings form a compound requirement, which is
        // not rewritten.
        if c.get(3).is_some() {
            continue;
        }
        let first = c.get(2).unwrap();
        out.push(Dependency {
            manager: Manager::Bundler,
            file: path.into(),
            location: Location::new(text, first.start(), first.end(), format!("/gems/{name}")),
            name: name.into(),
            package: name.into(),
            kind: DependencyKind::Production,
            current: first.as_str().to_string(),
            resolved: lock.get(name).cloned(),
            digest: None,
            source: Source::Rubygems,
            scheme: Scheme::Rubygems,
            extract: None,
            included_by: None,
            registry: None,
        });
    }
    for dep in origins.assign(&mut out, &RUBYGEMS, |dep| dep.package.clone()) {
        skipped.push(Skipped {
            manager: Manager::Bundler,
            file: dep.file,
            line: Some(dep.location.line),
            value: dep.name,
            reason: NOT_FROM_A_SOURCE.into(),
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

/// RubyGems, for `crate::origins`.
const RUBYGEMS: Public = Public {
    source: Source::Rubygems,
    label: "https://rubygems.org",
    is_public: |url| {
        crate::web_host(url)
            .is_some_and(|host| matches!(host.as_str(), "rubygems.org" | "index.rubygems.org"))
    },
};

/// A `source` value: a string literal, or `:rubygems`; none for anything
/// Bundler would evaluate, such as `ENV["GEM_SOURCE"]`.
fn source_value(text: &str) -> Option<Named> {
    let text = text
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim();
    if text == ":rubygems" || text == "\"rubygems\"" {
        return Some(Named::Public);
    }
    let quote = text.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let inner = text[1..].strip_suffix(quote)?;
    (!inner.contains(['"', '\'', '#'])).then(|| Named::parse(inner, &RUBYGEMS))
}

/// Where the Gemfile's own declarations say gems come from: a `source`
/// block around a gem, the gem's `source:` option, or git, a path or GitHub.
struct Gemfile {
    origins: Origins,
    /// The innermost `source "…" do` block around each line, by line.
    blocks: Vec<Option<Named>>,
}

fn gemfile(path: &str, text: &str) -> Gemfile {
    static SOURCE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"^[ \t]*source\b[ \t(]*(.*?)[ \t]*(\bdo\b.*)?$"#).expect("source pattern")
    });
    static BLOCK: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"\bdo\b[ \t]*(\|[^|]*\|)?[ \t]*(#.*)?$"#).expect("block pattern")
    });
    let mut origins = Origins::default();
    let mut stack: Vec<Option<Named>> = Vec::new();
    let mut blocks = Vec::new();
    for line in text.lines() {
        let code = line.split(" #").next().unwrap_or(line);
        let trimmed = code.trim();
        if trimmed.starts_with("eval_gemfile") || trimmed.starts_with("instance_eval") {
            origins.uncertain.push(Named::Unusable(format!(
                "{} in {path}, which the Count does not follow",
                trimmed.split_whitespace().next().unwrap_or("eval_gemfile")
            )));
        }
        if let Some(c) = SOURCE.captures(code) {
            let value = source_value(c.get(1).map_or("", |m| m.as_str()));
            let block = c.get(2).is_some();
            match value {
                None => origins.uncertain.push(Named::Unusable(format!(
                    "a source in {path} the Count cannot read"
                ))),
                Some(Named::Public) => {}
                Some(named) if !block => origins.configured.push(named),
                Some(_) => {}
            }
            if block {
                stack.push(Some(
                    source_value(c.get(1).map_or("", |m| m.as_str())).unwrap_or_else(|| {
                        Named::Unusable(format!("a source in {path} the Count cannot read"))
                    }),
                ));
            }
        } else if trimmed == "end" || trimmed.starts_with("end ") {
            stack.pop();
        } else if BLOCK.is_match(code) {
            stack.push(None);
        }
        blocks.push(stack.iter().rev().find_map(Clone::clone));
    }
    Gemfile { origins, blocks }
}

/// What `Gemfile.lock` records of each gem: the `remote:` of its GEM
/// section, or that it comes from git, a path or a plugin source.
fn lock_origins(lock: &str) -> BTreeMap<String, Origin> {
    let mut origins = BTreeMap::<String, Vec<Origin>>::new();
    let mut section = "";
    let mut remotes: Vec<Named> = Vec::new();
    let mut specs = false;
    for line in lock.lines() {
        if !line.starts_with(' ') {
            section = line.trim();
            remotes.clear();
            specs = false;
            continue;
        }
        let trimmed = line.trim();
        if let Some(remote) = trimmed.strip_prefix("remote:") {
            remotes.push(Named::parse(remote.trim(), &RUBYGEMS));
            continue;
        }
        if trimmed == "specs:" {
            specs = true;
            continue;
        }
        if specs
            && line.starts_with("    ")
            && !line.starts_with("     ")
            && let Some((name, _)) = trimmed.split_once(" (")
        {
            let origin =
                match section {
                    "GEM" => Origin::of(remotes.iter().cloned().map(Origin::Index)).unwrap_or_else(
                        || Origin::Index(Named::Unusable("a locked gem without a remote".into())),
                    ),
                    _ => Origin::Other,
                };
            origins.entry(name.to_owned()).or_default().push(origin);
        }
    }
    origins
        .into_iter()
        .filter_map(|(name, entries)| Some((name, Origin::of(entries)?)))
        .collect()
}
