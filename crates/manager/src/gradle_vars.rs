//! Gradle versions held in variables: `ext` properties and blocks, `def`/`val`
//! locals, `extra` properties, map literals (`versions += [jackson: "2.15.0"]`)
//! and `gradle.properties`, referenced from coordinates (`"g:a:$jacksonVersion"`,
//! `"g:a:${versions.jackson}"`), map notation and plugin versions. A version is
//! edited at its definition, once, for its first user, as catalog `version.ref`s
//! are. Names defined more than once, computed values and the project's own
//! `version` are skipped: Gradle scoping is approximated by one namespace per
//! repository, so only unambiguous definitions are trusted.
use crate::*;
use regex::Regex;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::LazyLock,
};

/// Comments blanked to spaces, keeping byte offsets, newlines and strings.
pub(super) fn mask(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0;
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for b in &mut out[from..to] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    };
    while i < bytes.len() {
        let rest = &bytes[i..];
        if rest.starts_with(b"//") {
            let end = rest
                .iter()
                .position(|b| *b == b'\n')
                .map_or(bytes.len(), |n| i + n);
            blank(&mut out, i, end);
            i = end;
        } else if rest.starts_with(b"/*") {
            let end = rest[2..]
                .windows(2)
                .position(|w| w == b"*/")
                .map_or(bytes.len(), |n| i + n + 4);
            blank(&mut out, i, end);
            i = end;
        } else if rest.starts_with(b"\"\"\"") || rest.starts_with(b"'''") {
            let delimiter = &rest[..3];
            i += 3 + bytes[i + 3..]
                .windows(3)
                .position(|w| w == delimiter)
                .map_or(bytes.len() - i - 3, |n| n + 3);
        } else if matches!(rest[0], b'"' | b'\'') {
            let quote = rest[0];
            i += 1;
            while i < bytes.len() && bytes[i] != quote && bytes[i] != b'\n' {
                i += if bytes[i] == b'\\' { 2 } else { 1 };
            }
            i += 1;
        } else {
            i += 1;
        }
    }
    // Only ASCII bytes outside strings were replaced by ASCII spaces.
    String::from_utf8(out).unwrap_or_else(|_| text.to_string())
}

/// `rootProject.ext.x`, `project.x` and `extra.x` all name `x`.
fn normalise(name: &str) -> &str {
    let mut name = name;
    for prefix in ["rootProject.", "project.", "ext.", "extra."] {
        name = name.strip_prefix(prefix).unwrap_or(name);
    }
    name
}

#[derive(Clone)]
struct Definition {
    file: String,
    start: usize,
    value: String,
}

/// Project identity, Gradle settings and build switches are not versions.
fn reserved(name: &str) -> bool {
    matches!(name, "version" | "group" | "name" | "description")
        || ["org.gradle.", "systemProp.", "android.", "kotlin.", "kapt."]
            .iter()
            .any(|p| name.starts_with(p))
}

fn string_value(open: &str, value: &str, close: &str) -> bool {
    open == close && !value.contains(['$', '\\'])
}

fn script_definitions(path: &str, text: &str, out: &mut Vec<(String, Definition)>) {
    static ASSIGN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?m)(?:^|[\s;{(,])(?:(?:def|val|var|final|static|String)\s+)*((?:rootProject\.|project\.)?(?:ext\.|extra\.)?[A-Za-z_][A-Za-z0-9_]*)\s*(?::\s*String\s*)?=\s*(["'])([^"'\n]*)(["'])"#)
            .expect("assignment pattern")
    });
    static EXTRA: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"\b(?:extra|ext)(?:\.set\(\s*|\[\s*)["']([A-Za-z_][A-Za-z0-9_.]*)["']\s*(?:\]\s*=|,)\s*(["'])([^"'\n]*)(["'])"#)
            .expect("extra pattern")
    });
    static INDEXED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"([A-Za-z_][A-Za-z0-9_.]*)\[\s*["']([A-Za-z_][A-Za-z0-9_.-]*)["']\s*\]\s*=\s*(["'])([^"'\n]*)(["'])"#)
            .expect("indexed pattern")
    });
    static MAP: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"([A-Za-z_][A-Za-z0-9_.]*)\s*\+?=\s*\[").expect("map pattern")
    });
    static ENTRY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?:^|[\s,\[])([A-Za-z_][A-Za-z0-9_]*|"[A-Za-z0-9_.-]+"|'[A-Za-z0-9_.-]+')\s*:\s*(["'])([^"'\n]*)(["'])"#)
            .expect("entry pattern")
    });
    let mut push = |name: String, start: usize, value: &str| {
        out.push((
            name,
            Definition {
                file: path.into(),
                start,
                value: value.into(),
            },
        ));
    };
    for c in ASSIGN.captures_iter(text) {
        if string_value(&c[2], &c[3], &c[4]) {
            push(normalise(&c[1]).into(), c.get(3).unwrap().start(), &c[3]);
        }
    }
    for c in EXTRA.captures_iter(text) {
        if string_value(&c[2], &c[3], &c[4]) {
            push(normalise(&c[1]).into(), c.get(3).unwrap().start(), &c[3]);
        }
    }
    for c in INDEXED.captures_iter(text) {
        if string_value(&c[3], &c[4], &c[5]) {
            let map = normalise(&c[1]);
            push(format!("{map}.{}", &c[2]), c.get(4).unwrap().start(), &c[4]);
        }
    }
    // Map literals: entries directly inside the brackets, not nested maps.
    for m in MAP.captures_iter(text) {
        let name = normalise(&m[1]).to_string();
        let open = m.get(0).unwrap().end();
        let Some(close) = closing(text, open) else {
            continue;
        };
        let body = &text[open..close];
        for c in ENTRY.captures_iter(body) {
            let key = c.get(1).unwrap();
            if depth(body, key.start()) != 0 || !string_value(&c[2], &c[3], &c[4]) {
                continue;
            }
            let key = key.as_str().trim_matches(['"', '\'']);
            push(
                format!("{name}.{key}"),
                open + c.get(3).unwrap().start(),
                &c[3],
            );
        }
    }
}

/// The offset of the `]` closing a bracket opened just before `from`.
fn closing(text: &str, from: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let (mut depth, mut i, mut quote) = (1usize, from, None);
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(_) if b == b'\\' => i += 1,
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if matches!(b, b'"' | b'\'') => quote = Some(b),
            None if matches!(b, b'[' | b'(' | b'{') => depth += 1,
            None if matches!(b, b']' | b')' | b'}') => {
                depth -= 1;
                if depth == 0 {
                    return (b == b']').then_some(i);
                }
            }
            None => {}
        }
        i += 1;
    }
    None
}
/// Bracket nesting at `at` within a map body, ignoring strings.
fn depth(body: &str, at: usize) -> usize {
    let mut depth = 0usize;
    let mut quote = None;
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < at {
        let b = bytes[i];
        match quote {
            Some(_) if b == b'\\' => i += 1,
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if matches!(b, b'"' | b'\'') => quote = Some(b),
            None if matches!(b, b'[' | b'(' | b'{') => depth += 1,
            None if matches!(b, b']' | b')' | b'}') => depth = depth.saturating_sub(1),
            None => {}
        }
        i += 1;
    }
    depth
}

fn properties(path: &str, text: &str, out: &mut Vec<(String, Definition)>) {
    static PROPERTY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?m)^[ \t]*([A-Za-z_][A-Za-z0-9_.-]*)[ \t]*[=:][ \t]*([^\s\\]+)[ \t]*$")
            .expect("property pattern")
    });
    for c in PROPERTY.captures_iter(text) {
        let value = c.get(2).unwrap();
        out.push((
            c[1].to_string(),
            Definition {
                file: path.into(),
                start: value.start(),
                value: value.as_str().into(),
            },
        ));
    }
}

struct Reference {
    file: String,
    at: usize,
    name: String,
    package: String,
    kind: DependencyKind,
}

fn references(path: &str, text: &str, out: &mut Vec<Reference>) {
    static COORDINATE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#""([A-Za-z0-9_.-]+):([A-Za-z0-9_.-]+):\$(?:\{([A-Za-z_][A-Za-z0-9_.]*)\}|([A-Za-z_][A-Za-z0-9_.]*))""#)
            .expect("coordinate reference pattern")
    });
    static NOTATION: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"\bgroup\s*[:=]\s*["']([A-Za-z0-9_.-]+)["']\s*,\s*name\s*[:=]\s*["']([A-Za-z0-9_.-]+)["']\s*,\s*version\s*[:=]\s*(?:"\$\{([A-Za-z_][A-Za-z0-9_.]*)\}"|"\$([A-Za-z_][A-Za-z0-9_.]*)"|([A-Za-z_][A-Za-z0-9_.]*))"#)
            .expect("notation reference pattern")
    });
    static PLUGIN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"\bid\s*\(?\s*["']([A-Za-z0-9_.-]+)["']\s*\)?\s*version\s*\(?\s*(?:"\$\{([A-Za-z_][A-Za-z0-9_.]*)\}"|"\$([A-Za-z_][A-Za-z0-9_.]*)"|([A-Za-z_][A-Za-z0-9_.]*))"#)
            .expect("plugin reference pattern")
    });
    // In Kotlin, "$a.b" interpolates `a` and appends ".b".
    let kotlin = path.ends_with(".kts");
    let name = |braced: Option<regex::Match>, bare: Option<regex::Match>| -> Option<String> {
        if let Some(n) = braced {
            return Some(n.as_str().into());
        }
        let n = bare?.as_str();
        (!kotlin || !n.contains('.')).then(|| n.into())
    };
    let mut push = |at: usize, name: Option<String>, package: String, kind| {
        if let Some(name) = name {
            out.push(Reference {
                file: path.into(),
                at,
                name: normalise(&name).into(),
                package,
                kind,
            });
        }
    };
    for c in COORDINATE.captures_iter(text) {
        push(
            c.get(0).unwrap().start(),
            name(c.get(3), c.get(4)),
            format!("{}:{}", &c[1], &c[2]),
            DependencyKind::Production,
        );
    }
    for c in NOTATION.captures_iter(text) {
        push(
            c.get(0).unwrap().start(),
            name(c.get(3), c.get(4)).or_else(|| c.get(5).map(|m| m.as_str().into())),
            format!("{}:{}", &c[1], &c[2]),
            DependencyKind::Production,
        );
    }
    for c in PLUGIN.captures_iter(text) {
        push(
            c.get(0).unwrap().start(),
            name(c.get(2), c.get(3)).or_else(|| c.get(4).map(|m| m.as_str().into())),
            super::gradle::plugin(&c[1]),
            DependencyKind::Build,
        );
    }
}

/// Every dependency whose version is a variable, located at its definition.
pub(super) fn extract(files: &Files, selected: impl Fn(&str) -> bool) -> Vec<Dependency> {
    let mut definitions = Vec::new();
    let mut uses = Vec::new();
    let mut masked = BTreeMap::new();
    for (path, text) in files.iter().filter(|(p, _)| selected(p)) {
        let leaf = path.rsplit('/').next().unwrap_or(path);
        if leaf == "gradle.properties" {
            properties(path, text, &mut definitions);
        } else if leaf.ends_with(".gradle") || leaf.ends_with(".gradle.kts") {
            let text = masked.entry(path.as_str()).or_insert_with(|| mask(text));
            script_definitions(path, text, &mut definitions);
            references(path, text, &mut uses);
        }
    }
    let mut by_name = BTreeMap::<String, Vec<Definition>>::new();
    for (name, definition) in definitions {
        if !reserved(&name) {
            by_name.entry(name).or_default().push(definition);
        }
    }
    // A package reached through several variables keeps deliberate parallel
    // versions (kafka's kafka_28 .. kafka_43 compatibility pins).
    let mut variables = BTreeMap::<&str, BTreeSet<&str>>::new();
    for reference in &uses {
        variables
            .entry(&reference.package)
            .or_default()
            .insert(&reference.name);
    }
    let parallel: BTreeSet<String> = variables
        .into_iter()
        .filter(|(_, names)| names.len() > 1)
        .map(|(package, _)| package.to_string())
        .collect();
    let mut claimed = BTreeSet::new();
    let mut out = Vec::new();
    uses.sort_by(|a, b| (&a.file, a.at).cmp(&(&b.file, b.at)));
    for reference in uses {
        if parallel.contains(&reference.package) {
            continue;
        }
        let Some([definition]) = by_name.get(&reference.name).map(Vec::as_slice) else {
            continue;
        };
        if !super::gradle::literal(&definition.value) || !claimed.insert(reference.name.clone()) {
            continue;
        }
        let text = &files[&definition.file];
        out.push(super::gradle::maven(
            &definition.file,
            text,
            reference.package,
            reference.kind,
            definition.start,
            &definition.value,
            format!("/{}", reference.name),
        ));
    }
    out
}
