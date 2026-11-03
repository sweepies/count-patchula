//! Bundler: `gem` requirements and the `ruby` directive in a Gemfile, resolved
//! from `Gemfile.lock`, plus `.ruby-version`.
//!
//! Where each gem comes from follows `crate::origins`: the `remote:` of its
//! section in `Gemfile.lock`, and what the Gemfile says. A Gemfile is Ruby,
//! so the Count relies on it only when every line is one of a few plain
//! forms (see `gemfile`). Anything else, such as a condition or a line
//! continued onto the next, leaves gems the lock does not record failing
//! their lookups until `Gemfile.lock` is committed.
use crate::origins::{Named, Origin, Origins, Public};
use crate::*;
use regex::Regex;
use std::collections::BTreeSet;
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
    let dir = &path[..path.rfind('/').map_or(0, |i| i + 1)];
    // Each string's quotes captured on both sides, so a line Ruby reads
    // differently, such as `gem "a', '1"`, is not taken apart.
    static GEM: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?m)^[ \t]*gem[ \t(]+(["'])([A-Za-z0-9_.-]+)(["'])[ \t]*,[ \t]*(["'])([^"'\n]+)(["'])(?:[ \t]*,[ \t]*(["'])[^"'\n]+(["']))?"#)
            .expect("gem pattern")
    });
    static RUBY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?m)^[ \t]*ruby[ \t(]+["']([0-9][0-9.]*)["']"#).expect("ruby pattern")
    });
    let lock_text = files.get(&format!("{path}.lock"));
    let lock = lock_text.map(|l| locked(l)).unwrap_or_default();
    let Gemfile { mut origins, gems } = gemfile(path, text);
    origins.locks.extend(lock_text.map(|l| lock_origins(l)));
    origins.uncertain.extend(bundle_config(
        dir,
        files.get(&format!("{dir}.bundle/config")),
    ));
    let mut out = Vec::new();
    for c in GEM.captures_iter(text) {
        let quoted = |open: usize, close: usize| {
            c.get(open).map(|m| m.as_str()) == c.get(close).map(|m| m.as_str())
        };
        if !(quoted(1, 3) && quoted(4, 6) && quoted(7, 8)) {
            continue;
        }
        // Two requirement strings form a compound requirement, which is
        // not rewritten.
        if c.get(7).is_some() {
            continue;
        }
        let name = &c[2];
        let first = c.get(5).unwrap();
        // A gem line the Gemfile's reading did not see as one: its gem is
        // not taken to be RubyGems'.
        if !gems.contains(name) {
            origins.configured.push(Named::Unusable(format!(
                "a gem line in {path} the Count cannot read"
            )));
        }
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

/// What a Gemfile says about where its gems come from, and the gems its
/// lines name.
struct Gemfile {
    origins: Origins,
    gems: BTreeSet<String>,
}

/// Read a Gemfile's lines. When every line is one of the plain forms
/// `Reading::line` knows, the Gemfile alone says where each gem comes from,
/// as Bundler's DSL defines it: the gem's `source:` option, else the `source`
/// block around it, else the one top-level source. Otherwise nothing it says
/// is relied on: `Gemfile.lock` decides, and an unrecorded gem's lookup fails.
fn gemfile(path: &str, text: &str) -> Gemfile {
    let mut reading = Reading::default();
    let mut read = true;
    for line in text.lines() {
        read &= tokens(line)
            .and_then(|tokens| call(&tokens))
            .is_some_and(|call| reading.line(&call));
    }
    let Reading {
        mut origins,
        blocks,
        globals,
        gems,
    } = reading;
    if !read || !blocks.is_empty() {
        origins.configured.push(Named::Unusable(format!(
            "lines in {path} the Count cannot read"
        )));
        // A source other than RubyGems that a read line names for a gem
        // still counts: against a lock that disagrees it can only make the
        // lookup fail.
        for (name, named) in &gems {
            if let Some(named) = named.clone().filter(|n| *n != Named::Public) {
                let origin = Origin::Index(named);
                let merged = match origins.declared.remove(name) {
                    Some(before) => Origin::of([before, origin]).expect("two origins"),
                    None => origin,
                };
                origins.declared.insert(name.clone(), merged);
            }
        }
        let gems = gems.into_iter().map(|(name, _)| name).collect();
        return Gemfile { origins, gems };
    }
    // Several top-level sources leave a gem outside a block to the lock.
    let global = match globals.as_slice() {
        [] => Some(Named::Unusable(format!("no source in {path}"))),
        [one] => Some(one.clone()),
        _ => None,
    };
    let mut declared = BTreeMap::<String, Option<Origin>>::new();
    for (name, named) in &gems {
        let origin = named.clone().or_else(|| global.clone()).map(Origin::Index);
        let merged = match (declared.get(name), origin) {
            (None, origin) => origin,
            (Some(Some(before)), Some(origin)) => Origin::of([before.clone(), origin]),
            _ => None,
        };
        declared.insert(name.clone(), merged);
    }
    origins.declared = declared
        .into_iter()
        .filter_map(|(name, origin)| Some((name, origin?)))
        .collect();
    let gems = gems.into_iter().map(|(name, _)| name).collect();
    Gemfile { origins, gems }
}

/// The state of reading a Gemfile line by line.
#[derive(Default)]
struct Reading {
    origins: Origins,
    /// The blocks open around the current line: a `source` block's source,
    /// or none for a `group` or `platforms` block.
    blocks: Vec<Option<Named>>,
    /// Top-level sources, each once.
    globals: Vec<Named>,
    /// Each `gem` line's gem, with the source its option or block names.
    gems: Vec<(String, Option<Named>)>,
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

impl Reading {
    /// Record what one parsed line says; false when it is not a form the
    /// Count knows.
    fn line(&mut self, call: &Call) -> bool {
        let keys = || call.options.iter().map(|(key, _)| key.as_str());
        let only = |allowed: &[&str]| keys().all(|key| allowed.contains(&key));
        let strings = |values: &[Value]| values.iter().all(|v| matches!(v, Value::Str(_)));
        let names = |values: &[Value]| {
            !values.is_empty()
                && values
                    .iter()
                    .all(|v| matches!(v, Value::Str(_) | Value::Sym(_)))
        };
        let source = |value: &Value| match value {
            Value::Sym(s) if s == "rubygems" => Some(Named::Public),
            Value::Str(s) => Some(Named::parse(s, &RUBYGEMS)),
            _ => None,
        };
        let in_source = self.blocks.iter().rev().find_map(Clone::clone);
        match call.head.as_str() {
            "" => true,
            "end" => {
                call.positional.is_empty()
                    && call.options.is_empty()
                    && !call.block
                    && self.blocks.pop().is_some()
            }
            "source" => {
                let ([value], []) = (call.positional.as_slice(), call.options.as_slice()) else {
                    return false;
                };
                let Some(named) = source(value) else {
                    return false;
                };
                if named != Named::Public {
                    self.origins.configured.push(named.clone());
                }
                if call.block {
                    // Bundler does not nest source blocks.
                    if in_source.is_some() {
                        return false;
                    }
                    self.blocks.push(Some(named));
                } else if !self.blocks.is_empty() {
                    return false;
                } else if !self.globals.contains(&named) {
                    self.globals.push(named);
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
                let mut named = in_source;
                for (key, value) in &call.options {
                    match key.as_str() {
                        // The gem's own option wins over its block.
                        "source" => match value {
                            Value::Str(_) => {
                                named = source(value);
                                if let Some(other) = named.clone().filter(|n| *n != Named::Public) {
                                    self.origins.configured.push(other);
                                }
                            }
                            _ => return false,
                        },
                        key if GEM_KEYS.contains(&key) || OUTSIDE_KEYS.contains(&key) => {}
                        key if outside && GIT_KEYS.contains(&key) => {}
                        _ => return false,
                    }
                }
                if outside {
                    self.origins.outside.insert(name.clone());
                }
                self.gems.push((name.clone(), named));
                true
            }
            "group" | "platforms" | "platform" => {
                let known = call.block && names(&call.positional) && only(&["optional"]);
                if known {
                    self.blocks.push(None);
                }
                known
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
    let len = bytes.len();
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
                if inner.contains(['\\', '#', '"', '\'']) {
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
                    .unwrap_or(len);
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
                let end = (i..bytes.len()).find(|&j| !word(bytes[j])).unwrap_or(len);
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
/// `BUNDLE_SETTINGS`, or a file the Count cannot read. Bundler reads the file
/// line by line as `KEY: value`, not as YAML, so the Count does too, and any
/// line of another shape, such as an indented one, is unreadable.
fn bundle_config(dir: &str, text: Option<&String>) -> Option<Named> {
    let text = text?;
    let unread = || {
        Some(Named::Unusable(format!(
            "{dir}.bundle/config, which the Count cannot read"
        )))
    };
    for line in text.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() || line == "---" {
            continue;
        }
        if line.starts_with(char::is_whitespace) {
            return unread();
        }
        // The key runs to the first colon followed by a space or the end.
        let Some(colon) = line
            .match_indices(':')
            .map(|(i, _)| i)
            .find(|&i| line[i + 1..].chars().next().is_none_or(char::is_whitespace))
        else {
            return unread();
        };
        let key = line[..colon].trim_matches(['"', '\'']);
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
        let mut reading = Reading::default();
        if line == "end" {
            reading.blocks.push(None);
        }
        tokens(line)
            .and_then(|t| call(&t))
            .is_some_and(|c| reading.line(&c))
    }

    #[test]
    fn bundle_config_is_read_as_bundler_reads_it() {
        let read = |text: &str| bundle_config("", Some(&text.to_owned()));
        assert_eq!(
            read("---\nBUNDLE_PATH: \"vendor/bundle\"\nBUNDLE_BUILD__PG: \"--with-pg\"\n"),
            None
        );
        for text in [
            "BUNDLE_MIRROR__HTTPS://RUBYGEMS__ORG/: \"https://gems.acme.dev\"\n",
            "BUNDLE_GEMFILE: \"other/Gemfile\"\n",
            "BUNDLE_GEMS__ACME__DEV: \"bot:token\"\n",
            "bundle_path: x\n",
            // YAML would read one setting here; Bundler reads two.
            "---\nBUNDLE_PATH: |\n BUNDLE_MIRROR__ALL: https://gems.acme.dev\n",
            "../shared/bundle-config",
            crate::UNREADABLE_SETTINGS,
        ] {
            assert!(read(text).is_some(), "{text}");
        }
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
            "gem \"a', '1.0\", git: \"https://github.com/acme/a\"",
        ] {
            assert!(!reads(line), "{line}");
        }
    }
}
