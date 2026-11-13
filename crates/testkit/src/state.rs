//! `tests/state-versions.tsv` holds a row for every code-versioned `State`
//! and `tests/state-exempt.tsv` names every other one. Between them they
//! cover each `impl State` in non-test workspace source — a state cannot
//! exist without a row or an exemption, and a listing without an impl
//! fails too. Each state's crate owns the rows whose first source lives
//! under its `src/` and checks them in its parity test; the coverage scan
//! runs workspace-wide from here, so crates without a parity test of their
//! own are covered as well.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// A declared `impl State`: the crate and file it lives in and whether it
/// sets `CODE_VERSIONED`.
struct Declared {
    krate: String,
    path: PathBuf,
    code_versioned: bool,
}

/// The end of the nested block comment opening at `i` (`/*`).
fn block_end(bytes: &[u8], i: usize) -> usize {
    let mut depth = 1;
    let mut j = i + 2;
    while j + 1 < bytes.len() && depth > 0 {
        match (bytes[j], bytes[j + 1]) {
            (b'/', b'*') => {
                depth += 1;
                j += 2;
            }
            (b'*', b'/') => {
                depth -= 1;
                j += 2;
            }
            _ => j += 1,
        }
    }
    j.min(bytes.len())
}

/// The end of the literal opened at `at` and closed by `quote`,
/// escape-aware. A char literal bails on a newline — an unterminated `'`
/// was a lifetime — while a string runs to its closing quote.
fn quoted(bytes: &[u8], at: usize, quote: u8) -> usize {
    let mut j = at + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
            b if b == quote => return j + 1,
            b'\n' if quote == b'\'' => return j,
            _ => j += 1,
        }
    }
    bytes.len()
}

/// The end of the raw string opening at `i` (`r"..."`, `r#"..."#`,
/// `br"..."`, `br#"..."#`), or `None` when `i` does not open one.
fn raw_end(bytes: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    if bytes[i] == b'b' {
        if bytes.get(j) != Some(&b'r') {
            return None;
        }
        j += 1;
    }
    let hashes = {
        let start = j;
        while bytes.get(j) == Some(&b'#') {
            j += 1;
        }
        j - start
    };
    if bytes.get(j) != Some(&b'"') {
        return None;
    }
    j += 1;
    while j < bytes.len() {
        if bytes[j] == b'"'
            && j + 1 + hashes <= bytes.len()
            && bytes[j + 1..j + 1 + hashes].iter().all(|b| *b == b'#')
        {
            return Some(j + 1 + hashes);
        }
        j += 1;
    }
    Some(bytes.len())
}

/// `text` with the inside of comments and string and char literals
/// replaced by spaces (newlines kept, length unchanged): braces and
/// `#[cfg(test)]` inside them cannot fool the scans below.
fn skeleton(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0;
    while i < bytes.len() {
        let end = match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => bytes[i..]
                .iter()
                .position(|b| *b == b'\n')
                .map(|offset| i + offset)
                .unwrap_or(bytes.len()),
            b'/' if bytes.get(i + 1) == Some(&b'*') => block_end(bytes, i),
            b'"' => quoted(bytes, i, b'"'),
            b'b' if bytes.get(i + 1) == Some(&b'"') => quoted(bytes, i + 1, b'"'),
            b'\'' if bytes.get(i + 1) == Some(&b'\\') => quoted(bytes, i, b'\''),
            b'\'' if bytes.get(i + 2) == Some(&b'\'') => i + 3,
            b'r' | b'b' => match raw_end(bytes, i) {
                Some(end) => end,
                None => {
                    i += 1;
                    continue;
                }
            },
            _ => {
                i += 1;
                continue;
            }
        };
        for byte in &mut out[i..end] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
        i = end;
    }
    String::from_utf8(out).expect("blanking keeps UTF-8")
}

/// Whether a `cfg` predicate can only hold in test builds: `test` itself
/// or an `all(...)` that requires it. `any(...)` and `not(...)` can hold
/// in production, so their modules stay part of the scan.
fn test_only(predicate: &str) -> bool {
    let predicate = predicate.trim();
    if predicate == "test" {
        return true;
    }
    let Some(inner) = predicate
        .strip_prefix("all")
        .map(str::trim_start)
        .filter(|inner| inner.starts_with('('))
    else {
        return false;
    };
    let mut depth = 0;
    let mut arg = String::new();
    let mut args = Vec::new();
    for ch in inner.chars() {
        match ch {
            '(' => {
                depth += 1;
                if depth > 1 {
                    arg.push(ch);
                }
            }
            ')' => {
                depth -= 1;
                if depth == 0 {
                    args.push(std::mem::take(&mut arg));
                    break;
                }
                arg.push(ch);
            }
            ',' if depth == 1 => args.push(std::mem::take(&mut arg)),
            _ => arg.push(ch),
        }
    }
    args.iter().any(|arg| test_only(arg))
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    i
}

/// Whether `bytes` at `i` starts with `word` on a word boundary.
fn keyword(bytes: &[u8], i: usize, word: &[u8]) -> bool {
    bytes[i..].starts_with(word)
        && !bytes
            .get(i + word.len())
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
}

/// The end of the `(`-, `[`- or `{`-opened group that starts at `i`.
fn group(bytes: &[u8], i: usize) -> usize {
    let open = bytes[i];
    let close = match open {
        b'(' => b')',
        b'[' => b']',
        _ => b'}',
    };
    let mut depth = 1;
    let mut j = i + 1;
    while j < bytes.len() && depth > 0 {
        if bytes[j] == open {
            depth += 1;
        } else if bytes[j] == close {
            depth -= 1;
        }
        j += 1;
    }
    j
}

/// The `#[cfg(test)]`-gated modules one source file's skeleton declares:
/// `(start, end)` ranges an inline `mod name { .. }` occupies, and the
/// names of `mod name;` file modules.
fn test_modules(skel: &str) -> (Vec<(usize, usize)>, Vec<String>) {
    let bytes = skel.as_bytes();
    let attr = regex::Regex::new(r"#\[\s*cfg\s*\(").unwrap();
    let mut ranges = Vec::new();
    let mut names = Vec::new();
    for found in attr.find_iter(skel) {
        let mut j = group(bytes, found.end() - 1);
        let predicate = &skel[found.end()..j - 1];
        j = skip_ws(bytes, j);
        if bytes.get(j) != Some(&b']') || !test_only(predicate) {
            continue;
        }
        j += 1;
        // Attributes between the cfg and the `mod` still belong to it.
        loop {
            j = skip_ws(bytes, j);
            if bytes.get(j) == Some(&b'#') && bytes.get(j + 1) == Some(&b'[') {
                j = group(bytes, j + 1);
            } else {
                break;
            }
        }
        if keyword(bytes, j, b"pub") {
            j = skip_ws(bytes, j + 3);
            if bytes.get(j) == Some(&b'(') {
                j = skip_ws(bytes, group(bytes, j));
            }
        }
        if !keyword(bytes, j, b"mod") {
            continue;
        }
        let name_start = skip_ws(bytes, j + 3);
        let mut name_end = name_start;
        while bytes
            .get(name_end)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        {
            name_end += 1;
        }
        if name_end == name_start {
            continue;
        }
        j = skip_ws(bytes, name_end);
        match bytes.get(j) {
            Some(&b'{') => ranges.push((found.start(), group(bytes, j))),
            Some(&b';') => names.push(skel[name_start..name_end].to_owned()),
            _ => {}
        }
    }
    (ranges, names)
}

/// The files a `#[cfg(test)] mod name;` in `file` can resolve to.
fn module_files(file: &Path, name: &str) -> [PathBuf; 2] {
    let parent = file.parent().expect("a path inside the workspace");
    let stem = file
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("a UTF-8 file name");
    let base = if stem == "lib" || stem == "main" || stem == "mod" {
        parent.to_path_buf()
    } else {
        parent.join(stem)
    };
    [
        base.join(format!("{name}.rs")),
        base.join(name).join("mod.rs"),
    ]
}

/// Every `.rs` file under `dir`, recursively.
fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, files);
        } else if path.extension().is_some_and(|e| e == "rs") {
            files.push(path);
        }
    }
}

/// `text` with `#[cfg(test)]` module bodies blanked out, so impls inside
/// them — the cache crate's `release!` `test-derived` states and the
/// test-only scope doubles — are never declared.
fn without_test_modules(text: &str) -> String {
    let mut effective = text.as_bytes().to_vec();
    for (start, end) in test_modules(&skeleton(text)).0 {
        for byte in &mut effective[start..end] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    }
    String::from_utf8(effective).expect("blanking keeps UTF-8")
}

/// The `(namespace, code_versioned)` of each `impl State` in `text`:
/// `impl<T> State for`, qualified `patchula_cache::State` paths and
/// whitespace variants all count.
fn impls_in(text: &str, path: &Path) -> Vec<(String, bool)> {
    // Scan the skeleton so a comment or string cannot pose as an impl
    // block or hide the brace that ends one, then read the consts from
    // the real text.
    let skel = skeleton(text);
    let header = regex::Regex::new(
        r"(?m)^[ \t]*impl(?:\s*<(?:[^<>]|<[^<>]*>)*>)?\s+(?:[A-Za-z_]\w*::)*State\s+for\b",
    )
    .unwrap();
    let namespace = regex::Regex::new(r#"NAMESPACE\s*:\s*&'static str\s*=\s*"([^"]+)""#).unwrap();
    let versioned = regex::Regex::new(r"CODE_VERSIONED\s*:\s*bool\s*=\s*true").unwrap();
    let mut found = Vec::new();
    for m in header.find_iter(&skel) {
        let bytes = skel.as_bytes();
        let mut j = m.end();
        while j < bytes.len() && bytes[j] != b'{' && bytes[j] != b';' {
            j += 1;
        }
        if bytes.get(j) != Some(&b'{') {
            continue;
        }
        let body = &text[j..group(bytes, j)];
        let code_versioned = versioned.is_match(body);
        let Some(name) = namespace.captures(body) else {
            panic!("{}: impl State without a NAMESPACE literal", path.display())
        };
        found.push((name[1].to_owned(), code_versioned));
    }
    found
}

/// Every `impl State` in non-test source across the workspace, keyed by
/// `NAMESPACE`: `crates/*/src/**/*.rs`, minus `#[cfg(test)]` modules —
/// inline ones and `mod name;` file modules — and anything under a
/// `tests` directory.
fn declared() -> BTreeMap<String, Declared> {
    let root = crate::versions::root();
    let mut declared = BTreeMap::new();
    for entry in std::fs::read_dir(root.join("crates")).unwrap().flatten() {
        let src = entry.path().join("src");
        if !src.is_dir() {
            continue;
        }
        let krate = entry.file_name().to_string_lossy().into_owned();
        let mut files = Vec::new();
        collect(&src, &mut files);
        // A `#[cfg(test)] mod name;` resolves to a file; skip those files
        // the way the inline modules' contents are skipped below.
        let mut skip = BTreeSet::new();
        for file in &files {
            let text = std::fs::read_to_string(file)
                .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
            for name in test_modules(&skeleton(&text)).1 {
                for candidate in module_files(file, &name) {
                    if candidate.is_file() {
                        skip.insert(candidate);
                    }
                }
            }
        }
        for file in files {
            if skip.contains(&file) || file.components().any(|part| part.as_os_str() == "tests") {
                continue;
            }
            let text = std::fs::read_to_string(&file)
                .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
            for (namespace, code_versioned) in impls_in(&without_test_modules(&text), &file) {
                let previous = declared.insert(
                    namespace.clone(),
                    Declared {
                        krate: krate.clone(),
                        path: file.clone(),
                        code_versioned,
                    },
                );
                if let Some(previous) = previous {
                    panic!(
                        "{namespace}: declared in {} and {}",
                        previous.path.display(),
                        file.display()
                    );
                }
            }
        }
    }
    declared
}

/// `tests/state-exempt.tsv`: the namespace of every `impl State` that is
/// not code-versioned and the reason it needs no row.
fn exemptions() -> BTreeMap<String, String> {
    let path = crate::versions::root().join("tests/state-exempt.tsv");
    let table = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    table
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let mut fields = line.split('\t');
            let (Some(name), Some(reason), None) = (fields.next(), fields.next(), fields.next())
            else {
                panic!("tests/state-exempt.tsv: malformed row {line:?}");
            };
            (name.to_owned(), reason.to_owned())
        })
        .collect()
}

/// The workspace-wide half of the guard: a code-versioned impl is a row
/// in `tests/state-versions.tsv`, any other is an entry in
/// `tests/state-exempt.tsv`, and a listing whose impl is gone or whose
/// flag disagrees with the table fails.
fn coverage(declared: &BTreeMap<String, Declared>) {
    let rows: BTreeSet<String> = crate::versions::table("state-versions.tsv")
        .iter()
        .map(|row| row.name.clone())
        .collect();
    let exempt = exemptions();
    for (name, state) in declared {
        let path = state.path.display();
        if state.code_versioned {
            assert!(
                rows.contains(name),
                "{name}: CODE_VERSIONED impl in {path} has no row in \
                 tests/state-versions.tsv — add one"
            );
            assert!(
                !exempt.contains_key(name),
                "{name}: CODE_VERSIONED impl in {path} is listed in \
                 tests/state-exempt.tsv — remove it"
            );
        } else {
            assert!(
                exempt.contains_key(name),
                "{name}: impl in {path} is not CODE_VERSIONED — set the \
                 flag and add a row, or list it in tests/state-exempt.tsv"
            );
            assert!(
                !rows.contains(name),
                "{name}: impl in {path} is not CODE_VERSIONED but has a \
                 row in tests/state-versions.tsv — remove it or set the flag"
            );
        }
    }
    for name in exempt.keys() {
        assert!(
            declared.contains_key(name),
            "{name}: listed in tests/state-exempt.tsv but no impl State \
             declares it — remove the entry"
        );
    }
    for name in &rows {
        let Some(state) = declared.get(name) else {
            panic!(
                "{name}: a row in tests/state-versions.tsv but no impl \
                    State declares it — remove the row or restore the impl"
            )
        };
        assert!(
            state.code_versioned,
            "{name}: a row in tests/state-versions.tsv but the impl in {} \
             is not CODE_VERSIONED — move it to tests/state-exempt.tsv",
            state.path.display()
        );
    }
}

/// Every registered state has a row whose version and source hash still
/// hold, every code-versioned state the crate's sources declare is
/// registered, no row is stale, and every `impl State` in the workspace
/// is a row or an exemption. On a source or locked dependency change the
/// failure names the new hash; bump the state's `VERSION` and update its
/// row in the same commit.
pub fn check(krate: &str, registered: &std::collections::BTreeMap<&'static str, u32>) {
    let declared = declared();
    let mine = declared
        .iter()
        .filter(|(_, state)| state.krate == krate && state.code_versioned)
        .map(|(name, _)| name.clone())
        .collect();
    crate::versions::check("state-versions.tsv", krate, registered, mine);
    coverage(&declared);
}
