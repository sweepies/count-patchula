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

fn ruby(path: &str, text: &Lines<'_>, start: usize, version: &str, pointer: &str) -> Dependency {
    Dependency {
        cooldown: None,
        manager: Manager::Bundler,
        file: path.into(),
        location: Location::new(text, start, start + version.len(), pointer.into()),
        form: EditForm::Scalar(if path.ends_with(".ruby-version") {
            Syntax::Bare
        } else {
            Syntax::Inner
        }),
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
    text: &Lines<'_>,
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
    let lock_name = format!("{path}.lock");
    // An unread lockfile may record any remote for any gem, unlike an absent
    // one: every lookup it could steer fails instead of trusting the Gemfile.
    let (lock_text, unread) = match files.get(&lock_name).map(Text::read) {
        Some(Ok(text)) => (Some(text), false),
        Some(Err(_)) => (None, true),
        None => (None, false),
    };
    let lock = lock_text.map(locked).unwrap_or_default();
    let Gemfile {
        mut origins, gems, ..
    } = gemfile(path, text);
    origins.locks.extend(lock_text.map(lock_origins));
    if unread {
        origins.uncertain.push(Named::Unusable(format!(
            "(a {lock_name} the Count could not read)"
        )));
    }
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
            cooldown: None,
            manager: Manager::Bundler,
            file: path.into(),
            location: Location::new(text, first.start(), first.end(), format!("/gems/{name}")),
            form: EditForm::Scalar(Syntax::Inner),
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
    skipped.extend(origins::skipped(
        Manager::Bundler,
        origins.assign(&mut out, &RUBYGEMS),
        NOT_FROM_A_SOURCE,
    ));
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
pub(crate) const RUBYGEMS: Public = Public {
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
    /// The URLs of the gems' remote (non-`path`) source options.
    remote: Vec<String>,
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
        remote,
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
        return Gemfile {
            origins,
            gems,
            remote,
        };
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
    Gemfile {
        origins,
        gems,
        remote,
    }
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
    /// The URLs of remote (non-`path`) source options.
    remote: Vec<String>,
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
                        "git" | "github" | "gist" | "bitbucket" | "gitlab" => {
                            if let Value::Str(url) = value {
                                self.remote.push(url.clone());
                            }
                        }
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
    /// `true`, `false` or `nil`.
    Word(String),
    /// `[…]` of plain values.
    List(Vec<Value>),
    /// `%i[…]` or `%w[…]`: the letter and the words.
    Words(char, Vec<String>),
}

impl Value {
    /// The value as Ruby source. Every string came out of `tokens`, which
    /// takes none holding a quote, backslash or `#`, so a double-quoted
    /// string reads back as the same text and interpolates nothing.
    fn ruby(&self) -> String {
        match self {
            Self::Str(s) => format!("\"{s}\""),
            Self::Sym(s) => format!(":{s}"),
            Self::Word(w) => w.clone(),
            Self::List(values) => format!(
                "[{}]",
                values.iter().map(Self::ruby).collect::<Vec<_>>().join(", ")
            ),
            Self::Words(letter, words) => format!("%{letter}[{}]", words.join(" ")),
        }
    }
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
    List(char, Vec<String>),
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
                let items: Vec<String> = line[i + 3..end]
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect();
                if !items.iter().all(|item| item.bytes().all(word)) {
                    return None;
                }
                out.push(Token::List(char::from(bytes[i + 1]), items));
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
        Token::Word(w) if matches!(w.as_str(), "true" | "false" | "nil") => Value::Word(w.clone()),
        Token::List(letter, items) => Value::Words(*letter, items.clone()),
        Token::LBracket => {
            let mut values = Vec::new();
            while rest.get(*i) != Some(&Token::RBracket) {
                if !values.is_empty() {
                    (rest.get(*i)? == &Token::Comma).then_some(())?;
                    *i += 1;
                }
                values.push(value(rest, i)?);
            }
            *i += 1;
            Value::List(values)
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
fn bundle_config(dir: &str, text: Option<&Text>) -> Option<Named> {
    let unread = || {
        Some(Named::Unusable(format!(
            "{dir}.bundle/config, which the Count cannot read"
        )))
    };
    let Ok(text) = text?.read() else {
        return unread();
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
            let setting = crate::origins::shown(key).unwrap_or("a setting");
            return Some(Named::Unusable(format!(
                "{setting} in {dir}.bundle/config, which the Count does not follow"
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

/// Whether a Gemfile — its `source` lines, its gems' source and remote
/// options, and the lockfile beside it that Bundler resolves against —
/// steers a run only to RubyGems, a public forge or local sources. The
/// repository's `.bundle/config` never decides it: the runner's Bundler
/// never reads that file.
pub(crate) fn public_only(path: &str, text: &str, files: &Files) -> bool {
    let Gemfile {
        origins, remote, ..
    } = gemfile(path, text);
    origins.public_only()
        && remote
            .iter()
            .all(|url| crate::public::fetch_ok(url, &RUBYGEMS))
        && files.get(&format!("{path}.lock")).is_none_or(|lock| {
            lock.read()
                .is_ok_and(|lock| crate::public::lines_public(lock, &RUBYGEMS))
        })
}

/// The URLs a Gemfile's `source` directives and `gem` `source:` options
/// name, as written.
pub(crate) fn source_urls(text: &str) -> Vec<String> {
    let mut urls = Vec::new();
    for line in text.lines() {
        let Some(call) = tokens(line).and_then(|tokens| call(&tokens)) else {
            continue;
        };
        match call.head.as_str() {
            "source" => {
                for value in &call.positional {
                    if let Value::Str(url) = value {
                        urls.push(url.clone());
                    }
                }
            }
            "gem" => {
                for (key, value) in &call.options {
                    if key == "source"
                        && let Value::Str(url) = value
                    {
                        urls.push(url.clone());
                    }
                }
            }
            _ => {}
        }
    }
    urls
}

/// A Gemfile as the Count renders it for Bundler.
#[derive(Debug, PartialEq, Eq)]
pub struct RenderedGemfile {
    /// The Gemfile Bundler evaluates.
    pub text: String,
    /// Files beside the Gemfile that it reads (`ruby file:`), by name.
    pub reads: Vec<String>,
}

/// `text`, a Gemfile, rebuilt from the Count's reading of it: every line
/// one of the plain forms `Reading::line` knows, written back from its
/// parsed values, so Bundler evaluates nothing the repository wrote as
/// Ruby. Anything that would need Ruby the Count did not write — a line it
/// cannot read, a `gemspec`, or a gem from git, a path or a forge, whose
/// gemspec Bundler would evaluate — is an error naming it.
pub fn render_gemfile(text: &str) -> Result<RenderedGemfile, String> {
    let mut reading = Reading::default();
    let mut rendered = String::new();
    let mut reads = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let at = |what: &str| format!("line {} {what}", index + 1);
        let Some(call) = tokens(line).and_then(|tokens| call(&tokens)) else {
            return Err(at("is not a form the Count reads"));
        };
        let keys = || call.options.iter().map(|(key, _)| key.as_str());
        if call.head == "gemspec" {
            return Err(at("loads a gemspec, which Bundler evaluates as Ruby"));
        }
        if call.head == "gem" && keys().any(|key| OUTSIDE_KEYS.contains(&key)) {
            return Err(at(
                "takes a gem from git, a path or a forge, whose gemspec Bundler evaluates as Ruby",
            ));
        }
        if !reading.line(&call) {
            return Err(at("is not a form the Count reads"));
        }
        if call.head.is_empty() {
            continue;
        }
        for (key, value) in &call.options {
            if call.head == "ruby" && key == "file" {
                match value {
                    Value::Str(name)
                        if !name.contains(['/', '\\']) && !matches!(name.as_str(), "." | "..") =>
                    {
                        reads.push(name.clone());
                    }
                    _ => return Err(at("reads a file that is not beside the Gemfile")),
                }
            }
        }
        let mut parts: Vec<String> = call
            .positional
            .iter()
            .map(|value| match value {
                // Bundler's name for RubyGems, as the URL it stands for.
                Value::Sym(s) if call.head == "source" && s == "rubygems" => {
                    "\"https://rubygems.org\"".to_owned()
                }
                value => value.ruby(),
            })
            .collect();
        parts.extend(
            call.options
                .iter()
                .map(|(key, value)| format!("{key}: {}", value.ruby())),
        );
        rendered.push_str(&call.head);
        if !parts.is_empty() {
            rendered.push(' ');
            rendered.push_str(&parts.join(", "));
        }
        if call.block {
            rendered.push_str(" do");
        }
        rendered.push('\n');
    }
    if !reading.blocks.is_empty() {
        return Err("a block is never closed".into());
    }
    Ok(RenderedGemfile {
        text: rendered,
        reads,
    })
}

pub(crate) static DESCRIPTOR: Descriptor = Descriptor;

pub(crate) struct Descriptor;

impl crate::Ecosystem for Descriptor {
    fn proposing(&self, dep: &Dependency) -> Proposing {
        Proposing {
            lock: Lock {
                keeps_ranges: true,
                movable: dep.source == Source::Rubygems,
                reported: !matches!(dep.kind, DependencyKind::Toolchain | DependencyKind::Tool)
                    && dep.resolved.is_some()
                    && dep.source == Source::Rubygems,
                ..Proposing::BASE.lock
            },
            ..Proposing::BASE
        }
    }
    fn selected(&self, _config: &Config, path: &str) -> bool {
        matches!(
            crate::roles::leaf(path),
            "Gemfile" | "Gemfile.lock" | ".ruby-version"
        )
    }
    fn registry_file(&self, path: &str) -> bool {
        crate::roles::eco_roles(Manager::Bundler, path).registry
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
        let read = |text: &str| bundle_config("", Some(&Text::from(text)));
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
        ] {
            assert!(read(text).is_some(), "{text}");
        }
        assert!(bundle_config("", Some(&Text::Unread(crate::Unread::Failed))).is_some());
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

    /// The rendered Gemfile holds the plain forms alone, rebuilt from their
    /// parsed values: comments and spacing go, meaning stays.
    #[test]
    fn a_plain_gemfile_renders_from_its_reading() {
        let rendered = render_gemfile(
            "# frozen_string_literal: true\n\
             source :rubygems\n\
             ruby file: \".ruby-version\"\n\
             gem 'pg', '>= 1.1', '< 2.0', require: false # db\n\
             gem \"debug\", platforms: %i[ mri windows ], require: \"debug/prelude\"\n\
             gem \"tzinfo-data\", :platforms => [:mingw, :mswin]\n\
             source \"https://gems.acme.dev\" do\n\
             \x20 gem(\"private\", \"~> 1.0\")\n\
             end\n\
             group :development, :test, optional: true do\n\
             \x20 gem \"rspec\", source: \"https://rubygems.org\"\n\
             end\n",
        )
        .unwrap();
        assert_eq!(
            rendered.text,
            "source \"https://rubygems.org\"\n\
             ruby file: \".ruby-version\"\n\
             gem \"pg\", \">= 1.1\", \"< 2.0\", require: false\n\
             gem \"debug\", platforms: %i[mri windows], require: \"debug/prelude\"\n\
             gem \"tzinfo-data\", platforms: [:mingw, :mswin]\n\
             source \"https://gems.acme.dev\" do\n\
             gem \"private\", \"~> 1.0\"\n\
             end\n\
             group :development, :test, optional: true do\n\
             gem \"rspec\", source: \"https://rubygems.org\"\n\
             end\n"
        );
        assert_eq!(rendered.reads, [".ruby-version"]);
    }

    /// Anything the Count did not read, and every form that makes Bundler
    /// evaluate a gemspec, is refused with the line that holds it.
    #[test]
    fn a_gemfile_that_needs_ruby_is_not_rendered() {
        for (text, reason) in [
            (
                "source \"https://rubygems.org\"\ngemspec\n",
                "line 2 loads a gemspec",
            ),
            (
                "gem \"a\", path: \"vendor/a\"\n",
                "line 1 takes a gem from git",
            ),
            (
                "gem \"a\", git: \"https://example.test/a.git\"\n",
                "line 1 takes a gem from git",
            ),
            (
                "gem \"a\", github: \"acme/a\"\n",
                "line 1 takes a gem from git",
            ),
            ("File.write(\"marker\", \"ran\")\n", "line 1 is not a form"),
            ("eval_gemfile \"Gemfile.local\"\n", "line 1 is not a form"),
            ("git_source(:acme) { |r| r }\n", "line 1 is not a form"),
            ("plugin \"bundler-acme\"\n", "line 1 is not a form"),
            ("install_if -> { true } do\n", "line 1 is not a form"),
            ("gem \"a\", install_if: true\n", "line 1 is not a form"),
            ("path \"vendor\" do\nend\n", "line 1 is not a form"),
            (
                "git \"https://example.test/a.git\" do\nend\n",
                "line 1 is not a form",
            ),
            (
                "source \"https://x.test\", type: :acme\n",
                "line 1 is not a form",
            ),
            ("=begin\n=end\n", "line 1 is not a form"),
            ("__END__\n", "line 1 is not a form"),
            ("group :test do\n", "a block is never closed"),
            ("ruby file: \"../.ruby-version\"\n", "line 1 reads a file"),
        ] {
            let error = render_gemfile(text).unwrap_err();
            assert!(error.starts_with(reason), "{text:?}: {error}");
        }
    }
}
