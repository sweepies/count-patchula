//! Bundler: `gem` requirements and the `ruby` directive in a Gemfile, resolved
//! from `Gemfile.lock`, plus `.ruby-version`.
//!
//! Where each gem comes from follows `crate::origins`, with `Gemfile.lock` as
//! the record: a gem it lists comes from the `remote:` of its section. A
//! Gemfile is Ruby, so the Count reads its declarations only to tell whether
//! an unrecorded gem can only be RubyGems': every line must be one of a few
//! plain forms, and naming no source besides RubyGems. Anything else, such as
//! a condition, a second source or a line continued onto the next, leaves
//! unrecorded gems' lookups failing until `Gemfile.lock` is committed.
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
    let Some(dir) = path.strip_suffix("Gemfile") else {
        return Ok(Vec::new());
    };
    static GEM: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?m)^[ \t]*gem[ \t(]+["']([A-Za-z0-9_.-]+)["'][ \t]*,[ \t]*["']([^"'\n]+)["'](?:[ \t]*,[ \t]*["']([^"'\n]+)["'])?([^\n]*)$"#)
            .expect("gem pattern")
    });
    static RUBY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?m)^[ \t]*ruby[ \t(]+["']([0-9][0-9.]*)["']"#).expect("ruby pattern")
    });
    let lock_text = files.get(&format!("{dir}Gemfile.lock"));
    let lock = lock_text.map(|l| locked(l)).unwrap_or_default();
    let mut origins = gemfile(path, text);
    origins.locks.extend(lock_text.map(|l| lock_origins(l)));
    origins.uncertain.extend(bundle_config(
        dir,
        files.get(&format!("{dir}.bundle/config")),
    ));
    let mut out = Vec::new();
    for c in GEM.captures_iter(text) {
        // Two requirement strings form a compound requirement, which is
        // not rewritten.
        if c.get(3).is_some() {
            continue;
        }
        let name = &c[1];
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

/// What a Gemfile itself says about where its gems come from: the sources it
/// names besides RubyGems, and the gems it takes from git, a path or a forge.
/// A line that is not one of the plain forms below counts as a source the
/// Count cannot read, so unrecorded gems are not taken to be RubyGems'.
fn gemfile(path: &str, text: &str) -> Origins {
    let mut origins = Origins::default();
    let mut unread = false;
    for line in text.lines() {
        let read = tokens(line)
            .and_then(|tokens| call(&tokens))
            .is_some_and(|call| declaration(&call, &mut origins));
        unread |= !read;
    }
    if unread {
        origins.configured.push(Named::Unusable(format!(
            "lines in {path} the Count cannot read"
        )));
    }
    origins
}

/// Gem options that leave the gem on its source.
const GEM_KEYS: [&str; 6] = [
    "require",
    "group",
    "groups",
    "platform",
    "platforms",
    "force_ruby_platform",
];
/// Gem options that take the gem from git, a path or a forge instead.
const OUTSIDE_KEYS: [&str; 6] = ["git", "path", "github", "gist", "bitbucket", "gitlab"];
/// Options that only go with `OUTSIDE_KEYS`.
const GIT_KEYS: [&str; 5] = ["branch", "ref", "tag", "submodules", "glob"];

/// Record what one parsed Gemfile line says; false when it is not a form the
/// Count knows.
fn declaration(call: &Call, origins: &mut Origins) -> bool {
    let keys = || call.options.iter().map(|(key, _)| key.as_str());
    let only = |allowed: &[&str]| keys().all(|key| allowed.contains(&key));
    let strings = |values: &[Value]| values.iter().all(|v| matches!(v, Value::Str(_)));
    let names = |values: &[Value]| {
        !values.is_empty()
            && values
                .iter()
                .all(|v| matches!(v, Value::Str(_) | Value::Sym(_)))
    };
    match call.head.as_str() {
        "" => true,
        "end" => call.positional.is_empty() && call.options.is_empty() && !call.block,
        "source" => {
            let named = match call.positional.as_slice() {
                [Value::Sym(s)] if s == "rubygems" => Named::Public,
                [Value::Str(s)] if s == "rubygems" => Named::Public,
                [Value::Str(s)] => Named::parse(s, &RUBYGEMS),
                _ => return false,
            };
            if !call.options.is_empty() {
                return false;
            }
            if named != Named::Public {
                origins.configured.push(named);
            }
            true
        }
        "gem" => {
            let [Value::Str(name), requirements @ ..] = call.positional.as_slice() else {
                return false;
            };
            if call.block || !strings(requirements) {
                return false;
            }
            let outside = keys().any(|key| OUTSIDE_KEYS.contains(&key));
            for (key, value) in &call.options {
                match (key.as_str(), value) {
                    ("source", Value::Str(source)) => {
                        let named = Named::parse(source, &RUBYGEMS);
                        if named != Named::Public {
                            origins.configured.push(named);
                        }
                    }
                    (key, _) if GEM_KEYS.contains(&key) || OUTSIDE_KEYS.contains(&key) => {}
                    (key, _) if outside && GIT_KEYS.contains(&key) => {}
                    _ => return false,
                }
            }
            if outside {
                origins.outside.insert(name.clone());
            }
            true
        }
        "group" | "platforms" | "platform" => {
            call.block && names(&call.positional) && only(&["optional"])
        }
        "gemspec" => {
            !call.block
                && call.positional.is_empty()
                && only(&["path", "name", "development_group", "glob"])
        }
        "ruby" => {
            !call.block
                && call.positional.len() <= 1
                && strings(&call.positional)
                && only(&["file", "engine", "engine_version", "patchlevel"])
        }
        _ => false,
    }
}

/// One Gemfile line as a method call: `head positional…, key: value… [do]`.
#[derive(Debug, Default)]
struct Call {
    head: String,
    positional: Vec<Value>,
    options: Vec<(String, Value)>,
    block: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Value {
    Str(String),
    Sym(String),
    /// `true`, `false`, `nil` or a list of plain values.
    Literal,
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Str(String),
    Sym(String),
    Word(String),
    /// `key:`
    Key(String),
    /// `=>`
    Arrow,
    Comma,
    Open,
    Close,
    /// `%i[…]` or `%w[…]`.
    List,
    LBracket,
    RBracket,
}

/// The tokens of one line, without its comment; none for anything outside a
/// plain call, such as interpolation, escapes, blocks in braces or operators.
fn tokens(line: &str) -> Option<Vec<Token>> {
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b' ' | b'\t' | b'\r' => i += 1,
            b'#' => break,
            b'"' | b'\'' => {
                let end = i + 1 + line[i + 1..].find(b as char)?;
                let inner = &line[i + 1..end];
                if inner.contains(['\\', '#']) {
                    return None;
                }
                out.push(Token::Str(inner.to_owned()));
                i = end + 1;
            }
            b',' | b'(' | b')' | b'[' | b']' => {
                out.push(match b {
                    b',' => Token::Comma,
                    b'(' => Token::Open,
                    b')' => Token::Close,
                    b'[' => Token::LBracket,
                    _ => Token::RBracket,
                });
                i += 1;
            }
            b'=' if bytes.get(i + 1) == Some(&b'>') => {
                out.push(Token::Arrow);
                i += 2;
            }
            b':' if bytes
                .get(i + 1)
                .is_some_and(|&c| c.is_ascii_alphabetic() || c == b'_') =>
            {
                let end = (i + 1..bytes.len())
                    .find(|&j| !word(bytes[j]))
                    .unwrap_or(bytes.len());
                out.push(Token::Sym(line[i + 1..end].to_owned()));
                i = end;
            }
            // `%i[a b]` and `%w[a b]`: lists of symbols or strings.
            b'%' if matches!(bytes.get(i + 1), Some(b'i' | b'w'))
                && bytes.get(i + 2) == Some(&b'[') =>
            {
                let end = i + 3 + line[i + 3..].find(']')?;
                if !line[i + 3..end]
                    .split_whitespace()
                    .all(|item| item.bytes().all(word))
                {
                    return None;
                }
                out.push(Token::List);
                i = end + 1;
            }
            b if b.is_ascii_alphabetic() || b == b'_' => {
                let end = (i..bytes.len())
                    .find(|&j| !word(bytes[j]))
                    .unwrap_or(bytes.len());
                let name = line[i..end].to_owned();
                if bytes.get(end) == Some(&b':') && bytes.get(end + 1) != Some(&b':') {
                    out.push(Token::Key(name));
                    i = end + 1;
                } else {
                    out.push(Token::Word(name));
                    i = end;
                }
            }
            _ => return None,
        }
    }
    Some(out)
}

/// A line's tokens as a call; none for anything else.
fn call(tokens: &[Token]) -> Option<Call> {
    let Some((Token::Word(head), mut rest)) = tokens.split_first() else {
        return tokens.is_empty().then(Call::default);
    };
    let mut call = Call {
        head: head.clone(),
        ..Call::default()
    };
    if let Some((Token::Word(last), init)) = rest.split_last()
        && last == "do"
    {
        call.block = true;
        rest = init;
    }
    if let [Token::Open, inner @ .., Token::Close] = rest {
        rest = inner;
    }
    let mut i = 0;
    while i < rest.len() {
        if i > 0 {
            if rest[i] != Token::Comma {
                return None;
            }
            i += 1;
        }
        let key = match (rest.get(i), rest.get(i + 1)) {
            (Some(Token::Key(key)), _) => Some((key.clone(), 1)),
            (Some(Token::Sym(key)), Some(Token::Arrow)) => Some((key.clone(), 2)),
            _ => None,
        };
        if let Some((key, width)) = key {
            i += width;
            call.options.push((key, value(rest, &mut i)?));
        } else if call.options.is_empty() {
            call.positional.push(value(rest, &mut i)?);
        } else {
            return None;
        }
    }
    Some(call)
}

/// The literal value at `rest[*i]`, moving past it.
fn value(rest: &[Token], i: &mut usize) -> Option<Value> {
    let token = rest.get(*i)?;
    *i += 1;
    Some(match token {
        Token::Str(s) => Value::Str(s.clone()),
        Token::Sym(s) => Value::Sym(s.clone()),
        Token::Word(w) if matches!(w.as_str(), "true" | "false" | "nil") => Value::Literal,
        Token::List => Value::Literal,
        Token::LBracket => {
            let mut first = true;
            while rest.get(*i) != Some(&Token::RBracket) {
                if !first {
                    (rest.get(*i)? == &Token::Comma).then_some(())?;
                    *i += 1;
                }
                first = false;
                value(rest, i)?;
            }
            *i += 1;
            Value::Literal
        }
        _ => return None,
    })
}

/// Bundler settings in a project's `.bundle/config` that do not change where
/// gems come from. Any other, such as a mirror, `BUNDLE_GEMFILE`, a lockfile
/// path or plugins, makes every lookup fail.
const BUNDLE_SETTINGS: [&str; 30] = [
    "auto_clean_without_path",
    "auto_install",
    "bin",
    "cache_all",
    "cache_all_platforms",
    "cache_path",
    "clean",
    "console",
    "default_install_uses_path",
    "deployment",
    "disable_shared_gems",
    "disable_version_check",
    "force_ruby_platform",
    "frozen",
    "ignore_funding_requests",
    "ignore_messages",
    "jobs",
    "no_install",
    "no_prune",
    "only",
    "path",
    "path.system",
    "path_relative_to_cwd",
    "retry",
    "shebang",
    "silence_deprecations",
    "silence_root_warning",
    "timeout",
    "with",
    "without",
];

/// What a project's `.bundle/config` makes uncertain: a setting outside
/// `BUNDLE_SETTINGS`, or a file the Count cannot read.
fn bundle_config(dir: &str, text: Option<&String>) -> Option<Named> {
    let text = text?;
    let unread = || {
        Some(Named::Unusable(format!(
            "{dir}.bundle/config, which the Count cannot read"
        )))
    };
    let settings = match serde_yaml_ng::from_str::<serde_yaml_ng::Value>(text) {
        Ok(serde_yaml_ng::Value::Null) => return None,
        Ok(serde_yaml_ng::Value::Mapping(settings)) => settings,
        _ => return unread(),
    };
    for key in settings.keys() {
        let Some(key) = key.as_str() else {
            return unread();
        };
        let setting = key
            .strip_prefix("BUNDLE_")
            .map(|name| name.to_ascii_lowercase().replace("__", "."));
        let harmless = setting.is_some_and(|name| {
            BUNDLE_SETTINGS.contains(&name.as_str()) || name.starts_with("build.")
        });
        if !harmless {
            return Some(Named::Unusable(format!(
                "{} in {dir}.bundle/config, which the Count does not follow",
                crate::origins::shown(key)
            )));
        }
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;

    fn reads(line: &str) -> bool {
        let mut origins = Origins::default();
        tokens(line)
            .and_then(|t| call(&t))
            .is_some_and(|c| declaration(&c, &mut origins))
    }

    #[test]
    fn plain_gemfile_lines_are_read_and_anything_else_is_not() {
        for line in [
            "",
            "# frozen_string_literal: true",
            "source \"https://rubygems.org\"",
            "source 'https://rubygems.org' # the default",
            "source :rubygems",
            "source(\"https://rubygems.org\")",
            "gem \"rails\", \"~> 7.1\"",
            "gem 'pg', '>= 1.1', '< 2.0', require: false",
            "gem \"debug\", platforms: %i[ mri windows ], require: \"debug/prelude\"",
            "gem \"tzinfo-data\", :platforms => [:mingw, :mswin]",
            "gem(\"rake\", \"~> 13.0\")",
            "gem \"rails\", github: \"rails/rails\", branch: \"main\"",
            "group :development, :test do",
            "platforms :jruby do",
            "end",
            "gemspec",
            "gemspec path: \"..\"",
            "ruby \"3.3.0\"",
            "ruby file: \".ruby-version\"",
        ] {
            assert!(reads(line), "{line}");
        }
        for line in [
            "source ENV.fetch(\"GEM_SOURCE\")",
            "source \"https://rubygems.org\", type: :gem",
            "gem \"rails\", \"~> 7.1\",",
            "  git: \"https://github.com/acme/a\"",
            "gem \"a\", \"1.0\" if RUBY_VERSION >= \"3\"",
            "gem \"a\", \"#{version}\"",
            "gem \"a\", branch: \"main\"",
            "gem \"a\", **opts",
            "gem \"a\", \"git\" => \"https://github.com/acme/a\"",
            "gem \"a\", install_if: -> { true }",
            "git_source(:acme) { |repo| \"https://git.acme.dev/#{repo}\" }",
            "git \"https://github.com/acme/a\" do",
            "eval_gemfile \"Gemfile.local\"",
            "if ENV[\"CI\"]",
            "plugin \"bundler-acme\"",
            "group :test",
            "source \"https://gems.acme.dev\" do gem \"a\" end",
        ] {
            assert!(!reads(line), "{line}");
        }
    }
}
