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

/// Whether `path` is a folder Gradle build logic conventionally lives in.
pub(crate) fn logic_folder(path: &str) -> bool {
    let path = format!("{}/", path.trim_end_matches('/'));
    BUILD_LOGIC
        .iter()
        .any(|dir| path == *dir || path.ends_with(&format!("/{dir}")))
}

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
    (leaf.ends_with(".gradle") || leaf.ends_with(".gradle.kts")) && !fixture(path)
        || in_logic && logic_source(path)
}

/// Whether `path` is in a source set's test or resource folder, where
/// build scripts are fixtures rather than the build's own: under
/// `src/<set>/` for a set named for tests, or `src/<set>/resources/` for
/// any set but `main`. A project that only lives in a folder named `src`
/// is not.
fn fixture(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    parts.windows(3).any(|window| {
        let [src, set, next] = window else {
            return false;
        };
        // A set named for tests: `test`, `testFixtures`, `functionalTest`;
        // not `latest` or `contest`.
        let tests = set.starts_with("test") || set.contains("Test");
        *src == "src" && *set != "main" && (tests || *next == "resources")
    })
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

/// What one piece of build logic points Gradle at besides itself.
enum Reference {
    /// A build it includes, as a folder prefix; none when the Count cannot
    /// resolve its name, such as one built from a template or a call.
    Build(Option<String>),
    /// A script it applies, as a repository path; none when the Count
    /// cannot resolve it, such as a URL or a computed name.
    Script(Option<String>),
}

/// The identifier ending just before `at` in `code`, past spaces and `(`.
fn word_before(code: &str, at: usize) -> &str {
    let head = code[..at].trim_end_matches(|c: char| c.is_whitespace() || c == '(');
    let start = head
        .char_indices()
        .rev()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_'))
        .map_or(0, |(i, c)| i + c.len_utf8());
    &head[start..]
}

/// The string right after `end` in `code`, past spaces and `separators`.
fn string_after<'s>(
    code: &str,
    strings: &'s [(usize, String)],
    end: usize,
    separators: &[u8],
) -> Option<&'s str> {
    strings
        .iter()
        .find(|(at, _)| {
            *at >= end
                && code[end..*at]
                    .bytes()
                    .all(|b| b.is_ascii_whitespace() || separators.contains(&b))
        })
        .map(|(_, string)| string.as_str())
}

/// The folder of the build `path` belongs to: the nearest folder at or
/// above it with a settings script, as a path prefix.
fn root_folder(files: &Files, path: &str) -> Option<String> {
    // `buildSrc` is a build of its own, for its own scripts; its plugins
    // run in the main build.
    if let Some(at) = path
        .find("buildSrc/")
        .filter(|at| *at == 0 || path.as_bytes()[at - 1] == b'/')
    {
        let rest = &path[at + "buildSrc/".len()..];
        if !rest.contains('/')
            && (rest.starts_with("build.gradle") || rest.starts_with("settings.gradle"))
        {
            return Some(path[..at + "buildSrc/".len()].to_owned());
        }
    }
    let mut dir = &path[..path.rfind('/').map_or(0, |i| i + 1)];
    loop {
        if ["settings.gradle", "settings.gradle.kts"]
            .iter()
            .any(|name| files.contains_key(&format!("{dir}{name}")))
        {
            return Some(dir.to_owned());
        }
        if dir.is_empty() {
            return None;
        }
        let parent = dir.trim_end_matches('/');
        dir = &parent[..parent.rfind('/').map_or(0, |i| i + 1)];
    }
}

/// The script an `apply from` ending at `end` applies, as a repository
/// path: a plain string, a project's or the root project's `file(…)`, or a
/// string starting with the project's or root project's folder (`$rootDir`,
/// `${rootDir}`, `$projectDir`, `${projectDir}`); none for anything else.
fn applied_script(
    files: &Files,
    path: &str,
    text: &str,
    code: &str,
    strings: &[(usize, String)],
    end: usize,
) -> Option<String> {
    let (start, literal) = strings.iter().find(|(at, _)| *at >= end)?;
    let between: String = code[end..*start].split_whitespace().collect();
    let between = between.trim_start_matches([':', '=', '(']);
    let mut root = match between {
        "" | "file(" | "project.file(" => false,
        "rootProject.file(" => true,
        _ => return None,
    };
    let raw = text.get(start + 1..)?;
    if text[*start..].starts_with("\"\"\"") || text[*start..].starts_with("'''") {
        return None;
    }
    let mut tail = literal.as_str();
    for (prefix, folded, at_root) in [
        ("${rootDir}/", "$/", true),
        ("${rootProject.rootDir}/", "$/", true),
        ("${projectDir}/", "$/", false),
        ("$rootDir/", "$rootDir/", true),
        ("$projectDir/", "$projectDir/", false),
    ] {
        if raw.starts_with(prefix) {
            tail = literal.strip_prefix(folded)?;
            root = at_root;
            break;
        }
    }
    if tail.contains('$') || tail.contains("://") {
        return None;
    }
    let resolve = |base: &str| {
        included_folder(&format!("{base}x"), tail).map(|f| f.trim_end_matches('/').to_owned())
    };
    if root {
        return resolve(&root_folder(files, path)?);
    }
    // Relative to the project's folder: a build or settings script's own;
    // for a script another applies, either its own folder or the build's,
    // which must not name two different files.
    let own = &path[..path.rfind('/').map_or(0, |i| i + 1)];
    let leaf = path.rsplit('/').next().unwrap_or(path);
    if leaf.starts_with("build.gradle") || leaf.starts_with("settings.gradle") {
        return resolve(own);
    }
    let mut candidates: Vec<String> = [Some(own.to_owned()), root_folder(files, path)]
        .into_iter()
        .flatten()
        .filter_map(|base| resolve(&base))
        .collect();
    candidates.dedup();
    let existing: Vec<&String> = candidates
        .iter()
        .filter(|c| files.contains_key(*c))
        .collect();
    match existing.as_slice() {
        [one] => Some((*one).clone()),
        [] => candidates.into_iter().next(),
        _ => None,
    }
}

/// The builds and scripts the build logic at `path` names. Settings name
/// included builds relative to their own folder; anywhere else the Count
/// does not resolve them.
fn references(
    files: &Files,
    path: &str,
    text: &str,
    code: &str,
    strings: &[(usize, String)],
) -> Vec<Reference> {
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let bytes = code.as_bytes();
    let leaf = path.rsplit('/').next().unwrap_or(path);
    let settings = matches!(leaf, "settings.gradle" | "settings.gradle.kts");
    let plain = |s: &str| !s.contains('$') && !s.contains("://");
    let mut out = Vec::new();
    for (name, at) in ["includeBuild", "from"]
        .iter()
        .flat_map(|name| code.match_indices(name).map(move |(at, _)| (*name, at)))
    {
        let end = at + name.len();
        if (at > 0 && word(bytes[at - 1])) || bytes.get(end).is_some_and(|b| word(*b)) {
            continue;
        }
        match name {
            "includeBuild" => {
                let named = string_after(code, strings, end, b"(").filter(|s| plain(s));
                let folder = named
                    .filter(|_| settings)
                    .and_then(|named| included_folder(path, named));
                out.push(Reference::Build(folder));
            }
            _ if word_before(code, at) == "apply" => {
                let script = applied_script(files, path, text, code, strings, end);
                out.push(Reference::Script(script));
            }
            _ => {}
        }
    }
    // An `apply` whose line names `from` any other way, such as
    // `apply(mapOf("from" to …))` or `apply to: p, from: …`, applies a
    // script the Count does not follow.
    for (at, _) in code.match_indices("apply") {
        let end = at + "apply".len();
        if (at > 0 && word(bytes[at - 1])) || bytes.get(end).is_some_and(|b| word(*b)) {
            continue;
        }
        // A call of Gradle's `apply`, not a method of that name on an
        // object (`.apply { … }`) or its definition.
        let before = code[..at].trim_end();
        let next = code[end..].trim_start().bytes().next();
        if before.ends_with('.')
            || ["fun", "def", "void"].iter().any(|w| before.ends_with(w))
            || next == Some(b'{')
        {
            continue;
        }
        let line_end = code[end..].find('\n').map_or(code.len(), |n| end + n);
        let line = &code[end..line_end];
        let direct = line
            .trim_start_matches(|c: char| c.is_whitespace() || c == '(')
            .starts_with("from");
        let named_from = line.match_indices("from").any(|(n, _)| {
            let b = line.as_bytes();
            !(n > 0 && word(b[n - 1])) && !b.get(n + 4).is_some_and(|c| word(*c))
        }) || strings
            .iter()
            .any(|(start, s)| *start >= end && *start < line_end && s == "from");
        if named_from && !direct {
            out.push(Reference::Script(None));
        }
    }
    out
}

/// The files the build logic among `files` names that the Count must read
/// too: the sources of included builds and scripts applied by name.
pub(crate) fn wanted(files: &Files) -> (Vec<String>, Vec<String>) {
    let mut builds = Vec::new();
    let mut scripts = Vec::new();
    for (path, text) in files.iter().filter(|(path, _)| logic_source(path)) {
        let Some((code, strings)) = code(path, text) else {
            continue;
        };
        for reference in references(files, path, text, &code, &strings) {
            match reference {
                Reference::Build(Some(folder)) => builds.push(folder),
                Reference::Script(Some(script)) => scripts.push(script),
                _ => {}
            }
        }
    }
    (builds, scripts)
}

/// A lexer for Groovy and Kotlin build logic: comments and the literal text
/// of strings blanked, keeping offsets, while code inside `${ … }`
/// templates stays code, so it is scanned like any other.
struct Lexer<'a> {
    bytes: &'a [u8],
    out: Vec<u8>,
    /// Each string's start and literal text, with `$` standing for each
    /// template.
    strings: Vec<(usize, String)>,
    /// Kotlin: raw strings take no escapes, and backticks quote names.
    kotlin: bool,
    /// Java: strings hold no templates, and there are no slashy strings.
    java: bool,
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
            } else if self.kotlin && rest[0] == b'`' {
                // A name in backticks: one a plain name could be, such as
                // `maven`, stays code; any other, such as the plugin
                // `maven-publish`, is blanked.
                let end = i + 1 + rest[1..].iter().position(|b| matches!(b, b'`' | b'\n'))?;
                if bytes[end] != b'`' {
                    return None;
                }
                let name = &bytes[i + 1..end];
                let plain = name
                    .first()
                    .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
                    && name.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_');
                if plain {
                    self.blank(i, i + 1);
                    self.blank(end, end + 1);
                } else {
                    self.blank(i, end + 1);
                }
                i = end + 1;
            } else if !self.java && rest.starts_with(b"$/") {
                i = self.slashy(i, b"$/", b"/$")?;
            } else if !self.java && rest[0] == b'/' && self.slashy_starts(i) {
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
        let templates = quote[0] == b'"' && !self.java;
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
            let raw = self.kotlin && width == 3;
            if bytes[i] == b'\\' && !raw && i + 1 < bytes.len() {
                literal.extend_from_slice(&bytes[i..i + 2]);
                self.blank(i, i + 2);
                i += 2;
            } else if templates && bytes[i..].starts_with(b"${") {
                // The template's code stays; its braces are the template's.
                literal.push(b'$');
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
fn code(path: &str, text: &str) -> Option<(String, Vec<(usize, String)>)> {
    let mut lexer = Lexer {
        bytes: text.as_bytes(),
        out: text.as_bytes().to_vec(),
        strings: Vec::new(),
        kotlin: path.ends_with(".kt") || path.ends_with(".kts"),
        java: path.ends_with(".java"),
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

/// The index of the first byte at or after `i` that is not whitespace.
fn skip_ws(code: &str, mut i: usize) -> usize {
    while code
        .as_bytes()
        .get(i)
        .is_some_and(|b| b.is_ascii_whitespace())
    {
        i += 1;
    }
    i
}

/// The index just past the bracket at `open` (`{` or `(`); none when it
/// never closes. Strings and comments are blanked in `code`, so plain
/// counting finds the match.
fn matching(code: &str, open: usize) -> Option<usize> {
    let (left, right) = match code.as_bytes()[open] {
        b'{' => (b'{', b'}'),
        b'(' => (b'(', b')'),
        _ => return None,
    };
    let mut depth = 0usize;
    for (n, b) in code.as_bytes()[open..].iter().enumerate() {
        match b {
            _ if *b == left => depth += 1,
            _ if *b == right => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + n + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// The literal of the string starting at `at`, when one does.
fn string_at(strings: &[(usize, String)], at: usize) -> Option<&str> {
    strings
        .iter()
        .find(|(start, _)| *start == at)
        .map(|(_, s)| s.as_str())
}

/// The statement `at` falls in, back to the last `;`, `{`, `}` or
/// newline, so a keyword leading it can say the statement is not a plain
/// assignment.
fn leading(code: &str, from: usize, at: usize) -> &str {
    let mut start = at;
    while start > from && !matches!(code.as_bytes()[start - 1], b';' | b'{' | b'}' | b'\n') {
        start -= 1;
    }
    &code[start..at]
}

/// Whether `text` holds `name` as a whole word.
fn has_word(text: &str, name: &str) -> bool {
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    text.match_indices(name).any(|(at, _)| {
        !(at > 0 && word(text.as_bytes()[at - 1]))
            && !text
                .as_bytes()
                .get(at + name.len())
                .is_some_and(|b| word(*b))
    })
}

/// The URLs a repository's argument list or block assigns: each `url` or
/// `setUrl` statement's literal, and whether any assigns it another way —
/// more than once, conditionally or by an expression — which the Count
/// cannot read as one address.
#[derive(Default)]
struct Urls {
    /// Every literal a `url` statement assigns.
    literal: Vec<String>,
    /// An assignment the Count cannot take for a literal: another
    /// expression, a template, a conditional statement or a second URL.
    suspect: bool,
}

impl Urls {
    /// The one address every assignment names: a single literal, or
    /// several naming the same public URL, which cannot redirect a
    /// lookup. `None` for no assignment and for assignments the Count
    /// cannot read as one.
    fn resolve(&self) -> Option<String> {
        if self.suspect {
            return None;
        }
        let [first, rest @ ..] = self.literal.as_slice() else {
            return None;
        };
        (rest.is_empty() || rest.iter().all(|u| u == first) && (MAVEN_PUBLIC.is_public)(first))
            .then(|| first.clone())
    }
    /// Whether the repository has address assignments the Count cannot
    /// read as one, so it may serve anything.
    fn unusable(&self) -> bool {
        if self.suspect {
            return true;
        }
        match self.literal.as_slice() {
            [] | [_] => false,
            [first, rest @ ..] => {
                !(rest.iter().all(|u| u == first) && (MAVEN_PUBLIC.is_public)(first))
            }
        }
    }
}

/// The URL assignments in `range`, a repository's `url`/`setUrl`
/// statements — `url "…"`, `url = uri("…")`, `setUrl(…)` — and, where
/// `positional`, the one literal an argument list may be, as `maven("…")`
/// or `maven(url = "…")` write it. A statement inside a nested block such
/// as an `if`, led by a control keyword or assigning anything but one
/// literal, is not one the Count reads; an argument list that is anything
/// but literals and url statements — an identifier, an Elvis, a
/// concatenation — cannot be read either.
fn repo_urls(
    code: &str,
    strings: &[(usize, String)],
    range: std::ops::Range<usize>,
    positional: bool,
) -> Urls {
    let bytes = code.as_bytes();
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut out = Urls::default();
    let mut named = false;
    // The spans accepted `url` statements consumed: a positional argument
    // list must hold nothing else.
    let mut consumed: Vec<std::ops::Range<usize>> = Vec::new();
    let mut depth = 0usize;
    let mut i = range.start;
    while i < range.end {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if !word(bytes[i]) || (i > range.start && word(bytes[i - 1])) {
            i += 1;
            continue;
        }
        let end = (i..range.end)
            .find(|&j| !word(bytes[j]))
            .unwrap_or(range.end);
        if matches!(&code[i..end], "artifactUrls" | "setArtifactUrls") {
            // Alternate artifact locations the Count does not read.
            out.suspect = true;
            i = end;
            continue;
        }
        if !matches!(&code[i..end], "url" | "setUrl") {
            i = end;
            continue;
        }
        named = true;
        let before = leading(code, range.start, i);
        if has_word(before, "def") || has_word(before, "final") {
            // A variable named `url`, not the repository's.
            i = end;
            continue;
        }
        let conditional = depth > 0
            || [
                "if", "else", "for", "while", "until", "try", "catch", "switch",
            ]
            .iter()
            .any(|name| has_word(before, name));
        if conditional {
            out.suspect = true;
            i = end;
            continue;
        }
        let mut j = skip_ws(code, end);
        let assign = match bytes.get(j) {
            Some(b'=' | b':') | Some(b'(') => {
                j = skip_ws(code, j + 1);
                true
            }
            Some(b'"' | b'\'') => true,
            _ => false,
        };
        if !assign {
            i = end;
            continue;
        }
        // `url = uri("…")` and `setUrl(uri("…"))`.
        if j < range.end
            && code[j..].starts_with("uri")
            && !bytes.get(j + 3).is_some_and(|b| word(*b))
        {
            j = skip_ws(code, j + 3);
            if bytes.get(j) == Some(&b'(') {
                j = skip_ws(code, j + 1);
            }
        }
        // The literal alone assigns the URL: what follows it is a
        // statement's end — a `;`, a closing bracket, a newline — or
        // nothing, never another expression.
        let clean_end = |j: usize| {
            let Some(n) = code[j + 1..range.end].find(bytes[j] as char) else {
                return false;
            };
            let rest = &code[j + n + 2..range.end];
            let next = rest.trim_start();
            next.is_empty()
                || rest[..rest.len() - next.len()].contains('\n')
                || matches!(next.as_bytes()[0], b';' | b')' | b'}')
        };
        match string_at(strings, j).filter(|url| !url.contains('$')) {
            Some(url) if clean_end(j) => {
                out.literal.push(url.to_owned());
                if let Some(n) = code[j + 1..range.end].find(bytes[j] as char) {
                    consumed.push(i..j + n + 2);
                }
            }
            _ => out.suspect = true,
        }
        i = end;
    }
    if !positional {
        return out;
    }
    if !named {
        // `maven("…")`: the argument list is exactly one string literal —
        // `maven(repoUrl ?: "…")`, `maven("a", "b")` or any other
        // expression claims everything.
        let mut in_range = strings.iter().filter(|(at, _)| range.contains(at));
        match (in_range.next(), in_range.next()) {
            (Some((at, url)), None) => {
                let close = code[*at + 1..range.end]
                    .find(bytes[*at] as char)
                    .map(|n| at + n + 2)
                    .unwrap_or(range.end);
                if code[range.start..*at].trim().is_empty()
                    && code[close..range.end].trim().is_empty()
                    && !url.contains('$')
                {
                    out.literal.push(url.clone());
                } else {
                    out.suspect = true;
                }
            }
            (None, None) if !code[range].trim().is_empty() => out.suspect = true,
            (None, None) => {}
            _ => out.suspect = true,
        }
        return out;
    }
    // `maven(url = "…")`: the `url` statements are the whole argument
    // list, so a string they did not consume, or anything past spaces,
    // `;` and the `)` of `uri(…)`, claims everything.
    let uncovered = |i: usize| !consumed.iter().any(|r| r.contains(&i));
    for (at, _) in strings.iter().filter(|(at, _)| range.contains(at)) {
        if uncovered(*at) {
            out.suspect = true;
        }
    }
    if (range.start..range.end).any(|i| {
        uncovered(i)
            && !matches!(bytes[i], b';' | b')')
            && !bytes[i].is_ascii_whitespace()
    }) {
        out.suspect = true;
    }
    out
}

/// The groups and `group:module`s an include-only content filter claims.
type Claims = (BTreeSet<String>, BTreeSet<(String, String)>);

/// The groups and `group:module`s an include-only filter block claims:
/// `includeGroup "g"`, `includeModule "g", "m"` and the Kotlin forms
/// `includeGroup("g")`, `includeModule("g", "m")`, and nothing else. `None`
/// for any other statement — `includeGroupByRegex`, `includeVersion`,
/// `excludeGroup` — since the repository then claims everything.
fn filter_claims(
    code: &str,
    strings: &[(usize, String)],
    range: std::ops::Range<usize>,
) -> Option<Claims> {
    let bytes = code.as_bytes();
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut groups = BTreeSet::new();
    let mut modules = BTreeSet::new();
    let mut i = range.start;
    while i < range.end {
        i = skip_ws(code, i);
        while bytes.get(i) == Some(&b';') {
            i = skip_ws(code, i + 1);
        }
        if i >= range.end {
            break;
        }
        if !word(bytes[i]) {
            return None;
        }
        let end = (i..range.end)
            .find(|&j| !word(bytes[j]))
            .unwrap_or(range.end);
        let count = match &code[i..end] {
            "includeGroup" => 1,
            "includeModule" => 2,
            _ => return None,
        };
        i = skip_ws(code, end);
        if bytes.get(i) == Some(&b'(') {
            i = skip_ws(code, i + 1);
        }
        let mut args = Vec::new();
        for n in 0..count {
            if n > 0 {
                if bytes.get(i) != Some(&b',') {
                    return None;
                }
                i = skip_ws(code, i + 1);
            }
            if !matches!(bytes.get(i), Some(b'"' | b'\'')) {
                return None;
            }
            let arg = string_at(strings, i)?;
            // A template or escape is a group the Count does not know.
            if arg.contains('$') || arg.contains('\\') {
                return None;
            }
            args.push(arg.to_owned());
            i = code[i + 1..range.end]
                .find(bytes[i] as char)
                .map(|n| i + n + 2)?;
            i = skip_ws(code, i);
        }
        if bytes.get(i) == Some(&b')') {
            i = skip_ws(code, i + 1);
        }
        if count == 1 {
            groups.insert(args.pop()?);
        } else {
            modules.insert((args.swap_remove(0), args.pop()?));
        }
    }
    // An empty block — `content { }`, or comments only — bounds nothing,
    // so the repository still claims everything.
    (!groups.is_empty() || !modules.is_empty()).then_some((groups, modules))
}

/// The claims of every `content { … }` block a repository declares
/// itself, unioned: `None` when any holds anything but literal
/// `includeGroup` and `includeModule` statements — an empty block,
/// `includeGroupByRegex`, `includeVersion`, `excludeGroup` — since the
/// repository then claims everything. A `content`/`filter` call in
/// parentheses `content(action)`, or a block nested inside any other
/// closure, is one the Count does not read, as is `filter` outside
/// `exclusiveContent` (handled in `exclusive`): all claim everything.
/// `None` too when `range` has no such block.
fn content_filter(
    code: &str,
    strings: &[(usize, String)],
    range: std::ops::Range<usize>,
) -> Option<Option<Claims>> {
    let bytes = code.as_bytes();
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut union = Claims::default();
    let mut seen = false;
    let mut depth = 0usize;
    let mut i = range.start;
    while i < range.end {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if !word(bytes[i]) || (i > range.start && word(bytes[i - 1])) {
            i += 1;
            continue;
        }
        let end = (i..range.end)
            .find(|&j| !word(bytes[j]))
            .unwrap_or(range.end);
        let token = &code[i..end];
        if matches!(token, "content" | "filter") {
            let open = skip_ws(code, end);
            if token == "content" && depth == 0 && bytes.get(open) == Some(&b'{') {
                let close = matching(code, open)?;
                let Some((groups, modules)) = filter_claims(code, strings, open + 1..close - 1)
                else {
                    return Some(None);
                };
                union.0.extend(groups);
                union.1.extend(modules);
                seen = true;
                i = close;
                continue;
            }
            if matches!(bytes.get(open), Some(b'{' | b'(')) {
                return Some(None);
            }
        }
        i = end;
    }
    Some(seen.then_some(union))
}

/// What a repository call's declaration says it serves.
struct Serves {
    /// Its URL, where the Count reads one.
    url: Option<String>,
    /// The groups and `group:module`s include-only filters claim;
    /// `None` when it can serve anything.
    claims: Option<Claims>,
    /// A call naming a public repository: `mavenCentral()`, `google()` or
    /// `gradlePluginPortal()` alone inside `forRepository`.
    public_call: bool,
    /// Its address is assigned in a way the Count cannot read as one: by
    /// several statements, another expression or conditionally.
    unusable_url: bool,
}

/// The repository a call at `end` (just past its name) declares, and the
/// offset the Count read to: optional `(args)`, an optional `{ block }`
/// with its `url` and `content` filters.
fn named_repository(code: &str, strings: &[(usize, String)], end: usize) -> (Serves, usize) {
    let bytes = code.as_bytes();
    let mut named = Serves {
        url: None,
        claims: None,
        public_call: false,
        unusable_url: false,
    };
    let mut urls = Urls::default();
    let mut i = skip_ws(code, end);
    if bytes.get(i) == Some(&b'(') {
        let Some(close) = matching(code, i) else {
            return (named, i);
        };
        let args = repo_urls(code, strings, i + 1..close - 1, true);
        urls.literal.extend(args.literal);
        urls.suspect |= args.suspect;
        i = skip_ws(code, close);
    } else if matches!(bytes.get(i), Some(b'"' | b'\'')) {
        // `maven "…"` names the URL positionally: one literal is the
        // whole argument, ended by a statement's end or the block.
        if let Some(url) = string_at(strings, i) {
            let after = code[i + 1..]
                .find(bytes[i] as char)
                .map_or(i + 1, |n| i + n + 2);
            let rest = &code[after..];
            let next = rest.trim_start();
            let clean = next.is_empty()
                || rest[..rest.len() - next.len()].contains('\n')
                || matches!(next.as_bytes()[0], b';' | b')' | b'}' | b'{');
            if clean && !url.contains('$') {
                urls.literal.push(url.to_owned());
            } else {
                urls.suspect = true;
            }
            i = skip_ws(code, after);
        } else {
            i += 1;
        }
    }
    if bytes.get(i) == Some(&b'{') {
        let Some(close) = matching(code, i) else {
            return (named, i);
        };
        let inner = i + 1..close - 1;
        let block = repo_urls(code, strings, inner.clone(), false);
        urls.literal.extend(block.literal);
        urls.suspect |= block.suspect;
        named.claims = content_filter(code, strings, inner).flatten();
        i = close;
        let (url, unusable) = (urls.resolve(), urls.unusable());
        named.url = url;
        named.unusable_url = unusable;
        if unusable {
            named.claims = None;
        }
        return (named, i);
    }
    named.url = urls.resolve();
    named.unusable_url = urls.unusable();
    (named, i)
}

/// The calls `forRepository` may hold alone for its repository to stay a
/// public one.
const EXCLUSIVE_PUBLIC: [&str; 3] = ["mavenCentral", "google", "gradlePluginPortal"];

/// Calls that name a public repository: `mavenCentral()`,
/// `google()`, `gradlePluginPortal()` and `jcenter()`. Given a body —
/// `mavenCentral { url = "…" }` — they name one the Count does not read.
const PUBLIC_REPOS: [&str; 4] = ["mavenCentral", "google", "gradlePluginPortal", "jcenter"];

/// Whether the `repositories` call ending at `end` mutates the handler —
/// `repositories.all { … }`, `repositories.add(…)` — which the Count does
/// not read.
fn handler_mutation(code: &str, end: usize) -> bool {
    let bytes = code.as_bytes();
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    if bytes.get(skip_ws(code, end)) != Some(&b'.') {
        return false;
    }
    let j = skip_ws(code, skip_ws(code, end) + 1);
    let e = (j..code.len()).find(|&k| !word(bytes[k])).unwrap_or(code.len());
    matches!(&code[j..e], "all" | "add" | "addAll")
}

/// Whether `range` holds exactly the call `name()`: the one statement a
/// `forRepository` block may be for its repository to stay public.
fn lone_call(code: &str, range: std::ops::Range<usize>, name: &str) -> bool {
    let Some(rest) = code[range].trim().strip_prefix(name) else {
        return false;
    };
    let Some(rest) = rest.trim_start().strip_prefix('(') else {
        return false;
    };
    let Some(rest) = rest.trim_start().strip_prefix(')') else {
        return false;
    };
    rest.trim_end().trim_end_matches(';').trim_end().is_empty()
}

/// The repository `exclusiveContent { … }` at `end` wraps and filters, and
/// the offset the Count read to: `forRepository { … }` holds it and
/// `filter { … }` bounds what it serves.
fn exclusive(code: &str, strings: &[(usize, String)], end: usize) -> (Serves, usize) {
    let bytes = code.as_bytes();
    let mut named = Serves {
        url: None,
        claims: None,
        public_call: false,
        unusable_url: false,
    };
    let i = skip_ws(code, end);
    if bytes.get(i) != Some(&b'{') {
        return (named, end);
    }
    let Some(close) = matching(code, i) else {
        return (named, i);
    };
    let inner = i + 1..close - 1;
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut declared = false;
    // The union of the `filter` blocks' claims; a filter bounds a
    // repository its own `content` does not.
    let mut filters = Claims::default();
    let mut filtered = false;
    let mut bad_filter = false;
    let mut depth = 0usize;
    let mut j = inner.start;
    while j < inner.end {
        match bytes[j] {
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if !word(bytes[j]) || (j > inner.start && word(bytes[j - 1])) {
            j += 1;
            continue;
        }
        let e = (j..inner.end)
            .find(|&k| !word(bytes[k]))
            .unwrap_or(inner.end);
        match &code[j..e] {
            "forRepository" if depth == 0 => {
                let open = skip_ws(code, e);
                if bytes.get(open) != Some(&b'{') {
                    return (named, close);
                }
                let Some(inside) = matching(code, open) else {
                    return (named, close);
                };
                let range = open + 1..inside - 1;
                if EXCLUSIVE_PUBLIC
                    .iter()
                    .any(|call| lone_call(code, range.clone(), call))
                {
                    named.public_call = true;
                } else {
                    // Anything else is read as ordinary repository
                    // declarations.
                    let mut k = range.start;
                    while k < range.end {
                        if !word(bytes[k]) || (k > range.start && word(bytes[k - 1])) {
                            k += 1;
                            continue;
                        }
                        let e2 = (k..range.end)
                            .find(|&x| !word(bytes[x]))
                            .unwrap_or(range.end);
                        let token = &code[k..e2];
                        if REPOSITORY_CALLS.contains(&token) {
                            let (repo, until) = if token == "exclusiveContent" {
                                exclusive(code, strings, e2)
                            } else {
                                named_repository(code, strings, e2)
                            };
                            if declared {
                                // Several declarations name no one address.
                                named.url = None;
                                named.claims = None;
                                named.unusable_url = true;
                            } else {
                                named.url = repo.url;
                                named.claims = repo.claims;
                                named.unusable_url = repo.unusable_url;
                                declared = true;
                            }
                            k = until.max(e2);
                            continue;
                        }
                        if token.contains("ArtifactRepository")
                            || token.contains("RepositoryHandler")
                            || PUBLIC_REPOS.contains(&token)
                                && bytes.get(skip_ws(code, e2)) == Some(&b'{')
                            || token == "repositories" && handler_mutation(code, e2)
                            || matches!(token, "content" | "filter" | "forRepository")
                                && matches!(bytes.get(skip_ws(code, e2)), Some(b'{' | b'('))
                        {
                            named.claims = None;
                            named.unusable_url = true;
                        }
                        k = e2;
                    }
                }
                j = inside;
                continue;
            }
            "filter" if depth == 0 => {
                let open = skip_ws(code, e);
                if bytes.get(open) != Some(&b'{') {
                    return (named, close);
                }
                let Some(inside) = matching(code, open) else {
                    return (named, close);
                };
                filtered = true;
                match filter_claims(code, strings, open + 1..inside - 1) {
                    Some((groups, modules)) => {
                        filters.0.extend(groups);
                        filters.1.extend(modules);
                    }
                    None => bad_filter = true,
                }
                j = inside;
                continue;
            }
            // A repository or filter call nested inside another closure
            // or given an action in parentheses is one the Count does
            // not read.
            "forRepository" | "filter" | "content" | "exclusiveContent"
                if matches!(bytes.get(skip_ws(code, e)), Some(b'{' | b'(')) =>
            {
                bad_filter = true;
            }
            _ => {}
        }
        j = e;
    }
    named.claims = if named.unusable_url || bad_filter {
        None
    } else if !filtered {
        named.claims
    } else {
        match named.claims.take() {
            // A filter bounds a repository that claims everything.
            None => Some(filters),
            Some((mut groups, mut modules)) => {
                groups.extend(filters.0);
                modules.extend(filters.1);
                Some((groups, modules))
            }
        }
    };
    (named, close)
}

/// Record what one repository declaration means for the build: nothing
/// when it is public, claims when an include-only filter bounds it, and
/// failure for every lookup otherwise.
fn record(out: &mut crate::origins::Repositories, path: &str, named: Serves) {
    if named.public_call
        || named
            .url
            .as_deref()
            .is_some_and(|url| (MAVEN_PUBLIC.is_public)(url))
    {
        return;
    }
    let label = named.url.as_deref().map(crate::redacted);
    let at = |path: &str| match &label {
        Some(url) => format!("a repository in {path} at {url}"),
        None => format!("a repository in {path} at an address the Count does not read"),
    };
    match named.claims {
        Some((groups, modules)) => {
            out.warnings.push(format!(
                "{} claims only some artifacts; lookups for what it claims fail",
                at(path)
            ));
            let reason = format!("{} claims it", at(path));
            for group in groups {
                out.claimed.entry(group).or_insert_with(|| reason.clone());
            }
            for (group, module) in modules {
                out.claimed
                    .entry(format!("{group}:{module}"))
                    .or_insert_with(|| reason.clone());
            }
        }
        None => {
            let reason = if named.unusable_url {
                format!("a repository in {path} whose address the Count does not read")
            } else {
                match &label {
                    Some(_) => at(path),
                    None => format!("a repository {path} may add"),
                }
            };
            out.close(reason);
        }
    }
}

/// Why a Gradle build's Maven lookups cannot be taken to be public, and
/// the warnings that say so once per build: build logic that could add a
/// repository besides Maven Central, Google's and the Gradle Plugin
/// Portal, a build it includes or a script it applies that the Count does
/// not read, or a file it cannot read. Gradle tries repositories in order
/// and filters them by content, so a repository `content` or
/// `exclusiveContent` limits to named groups and modules claims only
/// those — lookups for them fail — while any other repository anywhere
/// makes every lookup fail.
///
/// Build logic is read from the scripts and build-logic folders, then from
/// what they include and apply, until nothing new is named.
pub(crate) fn private_repositories(files: &Files) -> Option<crate::origins::Repositories> {
    let mut out = crate::origins::Repositories::default();
    let mut queue: Vec<&str> = files
        .keys()
        .filter(|path| build_logic(path))
        .map(String::as_str)
        .collect();
    let mut seen: BTreeSet<&str> = queue.iter().copied().collect();
    while let Some(path) = queue.pop() {
        let text = &files[path];
        if matches!(
            text.as_str(),
            crate::UNREADABLE_SETTINGS | crate::UNREADABLE_DEEP
        ) {
            let reason = if text.as_str() == crate::UNREADABLE_DEEP {
                format!(
                    "include chain deeper than {} rounds starting at {path}; the remaining files were not read",
                    crate::MORE_ROUNDS
                )
            } else {
                format!("{path}, which the Count cannot read")
            };
            out.close(reason);
            return Some(out);
        }
        let unread = || format!("build logic in {path} the Count cannot read");
        let Some((code, strings)) = code(path, text) else {
            out.close(unread());
            return Some(out);
        };
        let Some(code) = without_publishing(&code) else {
            out.close(unread());
            return Some(out);
        };
        for reference in references(files, path, text, &code, &strings) {
            let named: Vec<&str> = match reference {
                Reference::Build(Some(folder)) => files
                    .keys()
                    .filter(|p| p.starts_with(folder.as_str()) && logic_source(p))
                    .map(String::as_str)
                    .collect(),
                Reference::Script(Some(script)) => files
                    .get_key_value(&script)
                    .map(|(p, _)| p.as_str())
                    .into_iter()
                    .collect(),
                Reference::Build(None) | Reference::Script(None) => Vec::new(),
            };
            if named.is_empty() {
                out.close(format!(
                    "a build or script {path} names, which the Count does not read"
                ));
                return Some(out);
            }
            for next in named {
                if seen.insert(next) {
                    queue.push(next);
                }
            }
        }
        let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let bytes = code.as_bytes();
        let len = bytes.len();
        let mut i = 0;
        while i < len {
            if !word(bytes[i]) || (i > 0 && word(bytes[i - 1])) {
                i += 1;
                continue;
            }
            let end = (i..len).find(|&j| !word(bytes[j])).unwrap_or(len);
            let token = &code[i..end];
            if token == "exclusiveContent" {
                let (named, until) = exclusive(&code, &strings, end);
                record(&mut out, path, named);
                i = until.max(end);
                continue;
            }
            if REPOSITORY_CALLS.contains(&token) {
                let (named, _) = named_repository(&code, &strings, end);
                record(&mut out, path, named);
            } else if PUBLIC_REPOS.contains(&token) {
                // `mavenCentral { url = "…" }` configures a repository the
                // Count does not read; the bare `mavenCentral()` calls
                // stay public.
                if bytes.get(skip_ws(&code, end)) == Some(&b'{') {
                    out.close(format!("a repository in {path} the Count does not read"));
                }
            } else if token.contains("ArtifactRepository")
                || token.contains("RepositoryHandler")
                || token == "repositories" && handler_mutation(&code, end)
            {
                out.close(format!("a repository {path} may add"));
            }
            i = end;
        }
        // A repository call by a quoted name, as Groovy allows:
        // `'maven' { … }`.
        for (at, string) in &strings {
            let after = at + string.len() + 2;
            let next = code
                .get(after..)
                .and_then(|rest| rest.trim_start().bytes().next());
            if REPOSITORY_CALLS.contains(&string.as_str())
                && matches!(next, Some(b'{' | b'('))
                || PUBLIC_REPOS.contains(&string.as_str()) && next == Some(b'{')
            {
                out.close(format!("a repository {path} may add"));
            }
        }
    }
    out.any().then_some(out)
}
