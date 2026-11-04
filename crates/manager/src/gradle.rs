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
        included_by: None,
        registry: None,
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
        package: "gradle".into(),
        kind: DependencyKind::Toolchain,
        current: version.as_str().into(),
        resolved: Some(version.as_str().into()),
        digest: None,
        source: Source::Mise,
        scheme: Scheme::Loose,
        extract: None,
        included_by: None,
        registry: None,
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

/// Maven Central, Google's Maven repository and the Gradle Plugin Portal,
/// for `crate::origins`: the public repositories Maven lookups use.
pub(crate) const MAVEN_PUBLIC: crate::origins::Public = crate::origins::Public {
    source: Source::Maven,
    label: "https://repo.maven.apache.org/maven2",
    is_public: |url| {
        crate::web_host(url).is_some_and(|host| {
            matches!(
                host.as_str(),
                "repo.maven.apache.org"
                    | "repo1.maven.org"
                    | "maven.google.com"
                    | "dl.google.com"
                    | "plugins.gradle.org"
            )
        })
    },
};

/// Identifiers that add a repository, or could, in Gradle build logic.
/// `mavenCentral()`, `google()`, `gradlePluginPortal()` and `jcenter()` add
/// public ones and are not among them.
const REPOSITORY_CALLS: [&str; 6] = [
    "maven",
    "ivy",
    "flatDir",
    "mavenLocal",
    "mavenRepo",
    "exclusiveContent",
];

/// Folders conventionally holding a Gradle build's own build logic, whose
/// Kotlin, Groovy and Java sources the Count reads for repositories.
const BUILD_LOGIC: [&str; 4] = [
    "buildSrc/",
    "build-logic/",
    "build-conventions/",
    "gradle/plugins/",
];

/// Whether `path` is a source file build logic can be written in.
pub(crate) fn logic_source(path: &str) -> bool {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    [".kt", ".kts", ".groovy", ".java", ".gradle"]
        .iter()
        .any(|suffix| leaf.ends_with(suffix))
}

/// Whether `path` is Gradle build logic the Count reads for repositories,
/// by its path alone: a script, or a source file in a build-logic folder.
/// Included builds elsewhere are read once the settings name them; see
/// `included_builds`.
pub(crate) fn build_logic(path: &str) -> bool {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    let in_logic = BUILD_LOGIC
        .iter()
        .any(|dir| path.starts_with(dir) || path.contains(&format!("/{dir}")));
    selected(path) && (leaf.ends_with(".gradle") || leaf.ends_with(".gradle.kts"))
        || in_logic && logic_source(path)
}

/// The folder an `includeBuild` in the settings script at `settings` names,
/// as a path prefix; none when it leaves the repository.
fn included_folder(settings: &str, named: &str) -> Option<String> {
    let mut parts: Vec<&str> = settings.split('/').collect();
    parts.pop();
    for part in named.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    (!parts.is_empty()).then(|| format!("{}/", parts.join("/")))
}

/// The folders of builds the repository's Gradle settings include, as path
/// prefixes, with the settings file naming each; none for one whose name
/// the Count cannot resolve.
fn includes(files: &Files) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    for (path, text) in files {
        let leaf = path.rsplit('/').next().unwrap_or(path);
        if !matches!(leaf, "settings.gradle" | "settings.gradle.kts") {
            continue;
        }
        let Some((code, strings)) = code(text) else {
            continue;
        };
        for (at, _) in code.match_indices("includeBuild") {
            let end = at + "includeBuild".len();
            let named = strings.iter().find(|(start, _)| {
                *start >= end
                    && code[end..*start]
                        .bytes()
                        .all(|b| b.is_ascii_whitespace() || b == b'(')
            });
            let folder = named.and_then(|(_, named)| included_folder(path, named));
            out.push((path.clone(), folder));
        }
    }
    out
}

/// The folders of builds the repository's Gradle settings include, whose
/// sources the Count reads for repositories too.
pub(crate) fn included_builds(files: &Files) -> Vec<String> {
    includes(files)
        .into_iter()
        .filter_map(|(_, folder)| folder)
        .collect()
}

/// A lexer for Groovy and Kotlin build logic: comments and the literal text
/// of strings blanked, keeping offsets, while code inside `${ … }`
/// templates stays code, so it is scanned like any other.
struct Lexer<'a> {
    bytes: &'a [u8],
    out: Vec<u8>,
    /// Each string's start and literal text, templates left out.
    strings: Vec<(usize, String)>,
}

impl Lexer<'_> {
    fn blank(&mut self, from: usize, to: usize) {
        for b in &mut self.out[from..to] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    }

    /// Code from `i`: to the end, or, in a template, past the `}` that
    /// closes it. None when a comment, string or template does not end.
    fn code(&mut self, mut i: usize, template: bool) -> Option<usize> {
        let bytes = self.bytes;
        let mut depth = 0usize;
        while i < bytes.len() {
            let rest = &bytes[i..];
            if rest.starts_with(b"//") {
                let end = rest
                    .iter()
                    .position(|b| *b == b'\n')
                    .map_or(bytes.len(), |n| i + n);
                self.blank(i, end);
                i = end;
            } else if rest.starts_with(b"/*") {
                let end = i + 2 + rest[2..].windows(2).position(|w| w == b"*/")? + 2;
                self.blank(i, end);
                i = end;
            } else if rest.starts_with(b"\"\"\"") || rest.starts_with(b"'''") {
                i = self.string(i, 3)?;
            } else if matches!(rest[0], b'"' | b'\'') {
                i = self.string(i, 1)?;
            } else if rest.starts_with(b"$/") {
                i = self.slashy(i, b"$/", b"/$")?;
            } else if rest[0] == b'/' && self.slashy_starts(i) {
                i = self.slashy(i, b"/", b"/")?;
            } else {
                match rest[0] {
                    b'{' => depth += 1,
                    b'}' if template && depth == 0 => return Some(i + 1),
                    b'}' => depth = depth.checked_sub(1)?,
                    _ => {}
                }
                i += 1;
            }
        }
        (!template).then_some(bytes.len())
    }

    /// Whether a `/` at `i` opens a Groovy slashy string rather than
    /// dividing: where an expression is expected, before anything but a
    /// comment or a space.
    fn slashy_starts(&self, i: usize) -> bool {
        let next = self.bytes.get(i + 1).copied().unwrap_or(b' ');
        let before = self.out[..i]
            .iter()
            .rev()
            .find(|b| !b.is_ascii_whitespace())
            .copied();
        !matches!(next, b'/' | b'*')
            && !next.is_ascii_whitespace()
            && before.is_none_or(|b| b"=~(,:[{!&|?;".contains(&b))
    }

    /// A Groovy slashy (`/…/`) or dollar-slashy (`$/…/$`) string from
    /// `start`; past its end. Its text is blanked, and its `${ … }`
    /// templates stay code.
    fn slashy(&mut self, start: usize, open: &[u8], close: &[u8]) -> Option<usize> {
        let bytes = self.bytes;
        let mut literal = Vec::new();
        let mut i = start + open.len();
        loop {
            if i >= bytes.len() {
                return None;
            }
            if bytes[i..].starts_with(close) {
                self.strings
                    .push((start, String::from_utf8_lossy(&literal).into_owned()));
                return Some(i + close.len());
            }
            let escape: &[u8] = if open == b"/" { b"\\/" } else { b"$/" };
            if bytes[i..].starts_with(escape) || (open != b"/" && bytes[i..].starts_with(b"$$")) {
                literal.extend_from_slice(&bytes[i..i + 2]);
                self.blank(i, i + 2);
                i += 2;
            } else if bytes[i..].starts_with(b"${") {
                self.blank(i, i + 2);
                let end = self.code(i + 2, true)?;
                self.blank(end - 1, end);
                i = end;
            } else {
                literal.push(bytes[i]);
                self.blank(i, i + 1);
                i += 1;
            }
        }
    }

    /// The string starting at `start` with a quote `width` long; past its
    /// end. Double-quoted strings hold `${ … }` templates; single-quoted
    /// ones (Groovy strings, Kotlin characters) do not.
    fn string(&mut self, start: usize, width: usize) -> Option<usize> {
        let bytes = self.bytes;
        let quote = &bytes[start..start + width];
        let templates = quote[0] == b'"';
        let mut literal = Vec::new();
        let mut i = start + width;
        loop {
            if i >= bytes.len() || (width == 1 && bytes[i] == b'\n') {
                return None;
            }
            if bytes[i..].starts_with(quote) {
                self.strings
                    .push((start, String::from_utf8_lossy(&literal).into_owned()));
                return Some(i + width);
            }
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                literal.extend_from_slice(&bytes[i..i + 2]);
                self.blank(i, i + 2);
                i += 2;
            } else if templates && bytes[i..].starts_with(b"${") {
                // The template's code stays; its braces are the template's.
                self.blank(i, i + 2);
                let end = self.code(i + 2, true)?;
                self.blank(end - 1, end);
                i = end;
            } else {
                literal.push(bytes[i]);
                self.blank(i, i + 1);
                i += 1;
            }
        }
    }
}

/// Code with comments and the literal text of strings blanked, keeping
/// offsets, and the strings' literal text with where they start; none when
/// a comment, string or template does not end, or braces do not balance.
fn code(text: &str) -> Option<(String, Vec<(usize, String)>)> {
    let mut lexer = Lexer {
        bytes: text.as_bytes(),
        out: text.as_bytes().to_vec(),
        strings: Vec::new(),
    };
    lexer.code(0, false)?;
    // Blanking replaced whole bytes, so a multi-byte character inside a
    // string becomes spaces and the rest stays as it was.
    let code = String::from_utf8(lexer.out).ok()?;
    Some((code, lexer.strings))
}

/// `code` with each `publishing { … }` block blanked, since it says where
/// the build publishes, not where it resolves from; none when braces do not
/// balance.
fn without_publishing(code: &str) -> Option<String> {
    let bytes = code.as_bytes();
    let mut out = bytes.to_vec();
    let mut depth = 0i64;
    for b in bytes {
        match b {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            return None;
        }
    }
    if depth != 0 {
        return None;
    }
    let mut from = 0;
    while let Some(at) = code[from..].find("publishing") {
        let start = from + at;
        let end = start + "publishing".len();
        from = end;
        let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        if (start > 0 && word(bytes[start - 1])) || bytes.get(end).is_some_and(|b| word(*b)) {
            continue;
        }
        let Some(open) = code[end..]
            .find(|c: char| !c.is_whitespace())
            .map(|n| end + n)
        else {
            continue;
        };
        if bytes[open] != b'{' {
            continue;
        }
        let mut depth = 0;
        let mut close = open;
        for (n, b) in bytes[open..].iter().enumerate() {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = open + n;
                        break;
                    }
                }
                _ => {}
            }
        }
        for b in &mut out[start..=close] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
        from = close + 1;
    }
    String::from_utf8(out).ok()
}

/// Why a Gradle build's Maven lookups cannot be taken to be public: build
/// logic that could add a repository besides Maven Central, Google's and the
/// Gradle Plugin Portal, an included build the Count does not read, or a
/// file it cannot read. Gradle tries repositories in order and filters them
/// by content; the Count does not follow that, so any other repository
/// anywhere makes every lookup fail.
pub(crate) fn private_repositories(files: &Files) -> Option<String> {
    // An included build is read like `buildSrc`, so its files must be here.
    let includes = includes(files);
    for (settings, folder) in &includes {
        let read = folder
            .as_ref()
            .is_some_and(|folder| files.keys().any(|path| path.starts_with(folder.as_str())));
        if !read {
            return Some(format!(
                "an included build {settings} names, which the Count does not read"
            ));
        }
    }
    let included: Vec<&str> = includes
        .iter()
        .filter_map(|(_, folder)| folder.as_deref())
        .collect();
    let logic = |path: &str| {
        build_logic(path)
            || logic_source(path) && included.iter().any(|folder| path.starts_with(folder))
    };
    for (path, text) in files.iter().filter(|(path, _)| logic(path)) {
        if text.as_str() == crate::UNREADABLE_SETTINGS {
            return Some(format!("{path}, which the Count cannot read"));
        }
        let unread = || Some(format!("build logic in {path} the Count cannot read"));
        let Some((code, strings)) = code(text) else {
            return unread();
        };
        let Some(code) = without_publishing(&code) else {
            return unread();
        };
        let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let bytes = code.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if !word(bytes[i]) || (i > 0 && word(bytes[i - 1])) {
                i += 1;
                continue;
            }
            let end = (i..bytes.len())
                .find(|&j| !word(bytes[j]))
                .unwrap_or(bytes.len());
            let token = &code[i..end];
            if REPOSITORY_CALLS.contains(&token)
                || token.contains("ArtifactRepository")
                || token.contains("RepositoryHandler")
            {
                return Some(format!("a repository {path} may add"));
            }
            // `apply from: "https://…"` and `apply(from = "https://…")`.
            if token == "from"
                && let Some((_, string)) = strings.iter().find(|(at, _)| {
                    *at >= end
                        && code[end..*at]
                            .bytes()
                            .all(|b| b.is_ascii_whitespace() || b"=:(".contains(&b))
                })
                && string.contains("://")
            {
                return Some(format!("a script {path} applies from elsewhere"));
            }
            i = end;
        }
    }
    None
}
