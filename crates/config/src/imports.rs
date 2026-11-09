//! The local-module closure of a Pkl config: `amends`, `extends` and
//! `import` or `import*` targets walked from an entry file. Both the local
//! evaluator and the engine's repository configs resolve it here, so the
//! rules cannot drift: a lexer skips comments and every string form before
//! matching, remote (`:`), absolute and non-POSIX imports are refused, an
//! import cannot climb out of the entry's directory, and the walk reads at
//! most 100 modules.
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};

/// The text of `entry` and every local module it imports, keyed by the
/// normalized path `read` resolves them under. `read` returns a module's
/// text, or `None` when there is none: the schema module's own text stands
/// in for a missing `schema_name` file, and any other missing module
/// fails.
pub async fn import_files<F, Fut>(
    entry: &str,
    schema_name: &str,
    schema: &str,
    read: &mut F,
) -> Result<BTreeMap<String, String>, String>
where
    F: FnMut(&str) -> Fut + Send,
    Fut: std::future::Future<Output = Result<Option<String>, String>> + Send,
{
    let mut files = BTreeMap::new();
    let entry_path = normalize(Path::new(entry))?;
    // Imports may not climb out of the directory the entry sits in: for
    // the engine that is the repository root, for a local eval the config
    // file's own directory.
    let root = Path::new(&entry_path)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let mut pending = vec![entry_path];
    let mut visited = BTreeSet::new();
    while let Some(path) = pending.pop() {
        if !visited.insert(path.clone()) {
            continue;
        }
        if visited.len() > 100 {
            return Err("config import budget exceeded".into());
        }
        let text = match read(&path).await? {
            Some(text) => text,
            None if Path::new(&path)
                .file_name()
                .is_some_and(|name| name == schema_name) =>
            {
                schema.into()
            }
            None => return Err(format!("missing config module {path}")),
        };
        for relative in import_targets(&text) {
            if relative.contains(':') || relative.starts_with('/') || relative.contains('\\') {
                return Err(
                    "config imports must be local modules; remote imports are disabled".into(),
                );
            }
            let parent = Path::new(&path).parent().unwrap_or_else(|| Path::new(""));
            let resolved = normalize(&parent.join(&relative))?;
            if !within(&root, Path::new(&resolved)) {
                return Err("config import escapes repository".into());
            }
            pending.push(resolved);
        }
        files.insert(path, text);
    }
    Ok(files)
}

/// `path` with `.` and `..` resolved, refusing one that climbs out of the
/// tree it started in.
fn normalize(path: &Path) -> Result<String, String> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err("config import escapes repository".into());
                }
            }
            Component::CurDir => {}
            part => normalized.push(part.as_os_str()),
        }
    }
    normalized
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| "config path is not UTF-8".to_owned())
}

/// Whether `path` stays inside `root`. An empty root is the repository
/// itself: every repository-relative path is inside it.
fn within(root: &Path, path: &Path) -> bool {
    root.as_os_str().is_empty() || path.starts_with(root)
}

/// The targets of the `amends`, `extends`, `import` and `import*` clauses
/// `text` carries: each the raw contents of the string literal that
/// follows. Comments and string literals are skipped while scanning, so
/// import-looking text inside one — a text block, a custom-delimited
/// string, a comment — is not an import.
fn import_targets(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut targets = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        i = match chars[i] {
            '/' if chars.get(i + 1) == Some(&'/') => line_comment(&chars, i),
            '/' if chars.get(i + 1) == Some(&'*') => block_comment(&chars, i),
            '#' | '"' => match string(&chars, i) {
                Some((_, end)) => end,
                None => i + 1,
            },
            c if c.is_ascii_alphabetic() || c == '_' => {
                let end = identifier(&chars, i);
                let word: String = chars[i..end].iter().collect();
                let clause = match word.as_str() {
                    "amends" | "extends" => Some(end),
                    // `import*` is the relaxed form; the `*` hugs the word.
                    "import" => Some(if chars.get(end) == Some(&'*') {
                        end + 1
                    } else {
                        end
                    }),
                    _ => None,
                };
                match clause {
                    Some(mut j) => {
                        j = trivia(&chars, j);
                        if chars.get(j) == Some(&'(') {
                            j = trivia(&chars, j + 1);
                        }
                        match string(&chars, j) {
                            Some((range, next)) => {
                                targets.push(chars[range].iter().copied().collect());
                                next
                            }
                            None => end,
                        }
                    }
                    None => end,
                }
            }
            _ => i + 1,
        };
    }
    targets
}

/// The index just past the `//` comment starting at `i`.
fn line_comment(chars: &[char], i: usize) -> usize {
    let mut j = i + 2;
    while j < chars.len() && chars[j] != '\n' {
        j += 1;
    }
    j
}

/// The index just past the `/* … */` comment starting at `i`; Pkl block
/// comments nest.
fn block_comment(chars: &[char], i: usize) -> usize {
    let mut j = i + 2;
    let mut depth = 1;
    while j < chars.len() && depth > 0 {
        if chars[j] == '/' && chars.get(j + 1) == Some(&'*') {
            depth += 1;
            j += 2;
        } else if chars[j] == '*' && chars.get(j + 1) == Some(&'/') {
            depth -= 1;
            j += 2;
        } else {
            j += 1;
        }
    }
    j
}

/// The index of the first character at or after `i` that is not whitespace
/// or part of a comment.
fn trivia(chars: &[char], mut i: usize) -> usize {
    loop {
        while chars.get(i).is_some_and(|c| c.is_whitespace()) {
            i += 1;
        }
        i = if chars.get(i) == Some(&'/') && chars.get(i + 1) == Some(&'/') {
            line_comment(chars, i)
        } else if chars.get(i) == Some(&'/') && chars.get(i + 1) == Some(&'*') {
            block_comment(chars, i)
        } else {
            return i;
        };
    }
}

/// The index just past the identifier starting at `i`.
fn identifier(chars: &[char], i: usize) -> usize {
    let mut j = i + 1;
    while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
        j += 1;
    }
    j
}

/// The raw contents of the string literal starting at `i`, as a range into
/// `chars`, and the index just past it; `None` when `i` does not open one.
/// Pkl's forms are covered: `"…"`, `"""…"""`, and `#"…"#`-style strings
/// with any run of `#` delimiters, which are raw — no escapes and no
/// interpolation. In the unescaped forms `\(` interpolates, and the
/// expression inside is skipped as code.
fn string(chars: &[char], i: usize) -> Option<(Range<usize>, usize)> {
    let mut hashes = 0;
    while chars.get(i + hashes) == Some(&'#') {
        hashes += 1;
    }
    if chars.get(i + hashes) != Some(&'"') {
        return None;
    }
    let mut j = i + hashes + 1;
    let block = chars.get(j) == Some(&'"') && chars.get(j + 1) == Some(&'"');
    if block {
        j += 2;
    }
    let content = j;
    let raw = hashes > 0;
    while j < chars.len() {
        if block {
            if chars[j] == '"'
                && chars.get(j + 1) == Some(&'"')
                && chars.get(j + 2) == Some(&'"')
                && (0..hashes).all(|k| chars.get(j + 3 + k) == Some(&'#'))
            {
                return Some((content..j, j + 3 + hashes));
            }
            if !raw && chars[j] == '\\' && chars.get(j + 1) == Some(&'(') {
                j = interpolation(chars, j + 2);
                continue;
            }
            j += 1;
        } else if chars[j] == '"' && (0..hashes).all(|k| chars.get(j + 1 + k) == Some(&'#')) {
            return Some((content..j, j + 1 + hashes));
        } else if !raw && chars[j] == '\\' {
            if chars.get(j + 1) == Some(&'(') {
                j = interpolation(chars, j + 2);
            } else {
                j += 2;
            }
        } else {
            j += 1;
        }
    }
    None
}

/// The index just past the `)` closing the interpolation whose expression
/// starts at `i`, skipping nested strings and comments as code.
fn interpolation(chars: &[char], mut i: usize) -> usize {
    let mut depth = 1;
    while i < chars.len() && depth > 0 {
        i = match chars[i] {
            '(' => {
                depth += 1;
                i + 1
            }
            ')' => {
                depth -= 1;
                i + 1
            }
            '/' if chars.get(i + 1) == Some(&'/') => line_comment(chars, i),
            '/' if chars.get(i + 1) == Some(&'*') => block_comment(chars, i),
            '#' | '"' => match string(chars, i) {
                Some((_, end)) => end,
                None => i + 1,
            },
            _ => i + 1,
        };
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(
        files: &BTreeMap<String, String>,
    ) -> impl FnMut(&str) -> std::future::Ready<Result<Option<String>, String>> + '_ {
        move |path| std::future::ready(Ok(files.get(path).cloned()))
    }

    fn walk(
        entry: &str,
        files: &BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, String>, String> {
        futures_executor::block_on(import_files(
            entry,
            "PatchulaConfig.pkl",
            "schema",
            &mut read(files),
        ))
    }

    #[test]
    fn commented_out_imports_are_not_imports() {
        let files = BTreeMap::from([(
            "patchula.pkl".to_owned(),
            "amends \"PatchulaConfig.pkl\"\n// import \"https://example.com/remote.pkl\"\n/* extends \"/abs.pkl\" */\n".to_owned(),
        )]);
        let modules = walk("patchula.pkl", &files).unwrap();
        assert_eq!(
            modules.keys().map(String::as_str).collect::<Vec<_>>(),
            ["PatchulaConfig.pkl", "patchula.pkl"]
        );
        assert_eq!(modules["PatchulaConfig.pkl"], "schema");
    }

    #[test]
    fn remote_and_absolute_imports_are_refused() {
        for target in [
            "https://example.com/x.pkl",
            "package://x.pkl",
            "/etc/passwd.pkl",
            "dir\\file.pkl",
            "C:relative.pkl",
            "C:\\absolute.pkl",
            "\\\\server\\share.pkl",
        ] {
            let files = BTreeMap::from([(
                "patchula.pkl".to_owned(),
                format!("amends \"PatchulaConfig.pkl\"\nimport \"{target}\"\n"),
            )]);
            let error = walk("patchula.pkl", &files).unwrap_err();
            assert_eq!(
                error, "config imports must be local modules; remote imports are disabled",
                "{target}"
            );
        }
    }

    #[test]
    fn the_import_walk_is_budgeted() {
        // Each module imports the next; the 101st is over the budget.
        let mut files = BTreeMap::new();
        for index in 0..101 {
            let next = format!("mod{}.pkl", index + 1);
            files.insert(format!("mod{index}.pkl"), format!("import \"{next}\"\n"));
        }
        let error = walk("mod0.pkl", &files).unwrap_err();
        assert_eq!(error, "config import budget exceeded");
    }

    #[test]
    fn relative_imports_resolve_and_escapes_fail() {
        let files = BTreeMap::from([
            (
                "patchula.pkl".to_owned(),
                "amends \"PatchulaConfig.pkl\"\nimport \"lib/../shared.pkl\"\n".to_owned(),
            ),
            (
                "shared.pkl".to_owned(),
                "extends \"deep/inner.pkl\"\n".to_owned(),
            ),
            (
                "deep/inner.pkl".to_owned(),
                "amends \"PatchulaConfig.pkl\"\n".to_owned(),
            ),
        ]);
        let modules = walk("patchula.pkl", &files).unwrap();
        assert_eq!(
            modules.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "PatchulaConfig.pkl",
                "deep/PatchulaConfig.pkl",
                "deep/inner.pkl",
                "patchula.pkl",
                "shared.pkl"
            ]
        );
        // The schema stands in for the schema module wherever it is named.
        assert_eq!(modules["deep/PatchulaConfig.pkl"], "schema");
        let files = BTreeMap::from([(
            "patchula.pkl".to_owned(),
            "amends \"PatchulaConfig.pkl\"\nimport \"../escape.pkl\"\n".to_owned(),
        )]);
        let error = walk("patchula.pkl", &files).unwrap_err();
        assert_eq!(error, "config import escapes repository");
    }

    #[test]
    fn import_looking_text_inside_strings_and_comments_is_not_an_import() {
        let files = BTreeMap::from([(
            "patchula.pkl".to_owned(),
            "amends \"PatchulaConfig.pkl\"\nnote = \"\"\"\nimport \"missing.pkl\"\n\"\"\"\nraw = #\"import \\\"also-missing.pkl\\\"\"#\nblock = #\"\"\"\nimport \\\"third.pkl\\\"\n\"\"\"#\n// import \"fourth.pkl\"\n/* import \"fifth.pkl\" */\n".to_owned(),
        )]);
        let modules = walk("patchula.pkl", &files).unwrap();
        assert_eq!(
            modules.keys().map(String::as_str).collect::<Vec<_>>(),
            ["PatchulaConfig.pkl", "patchula.pkl"]
        );
    }

    #[test]
    fn an_absolute_entry_cannot_import_outside_its_directory() {
        let files = BTreeMap::from([
            (
                "/repo/patchula.pkl".to_owned(),
                "amends \"PatchulaConfig.pkl\"\nimport \"sub/x.pkl\"\n".to_owned(),
            ),
            (
                "/repo/sub/x.pkl".to_owned(),
                "extends \"PatchulaConfig.pkl\"\n".to_owned(),
            ),
        ]);
        let modules = walk("/repo/patchula.pkl", &files).unwrap();
        assert!(modules.contains_key("/repo/sub/x.pkl"));
        let files = BTreeMap::from([(
            "/repo/patchula.pkl".to_owned(),
            "amends \"PatchulaConfig.pkl\"\nimport \"../outside.pkl\"\n".to_owned(),
        )]);
        let error = walk("/repo/patchula.pkl", &files).unwrap_err();
        assert_eq!(error, "config import escapes repository");
    }
}
