//! The local-module closure of a Pkl config: `amends`, `extends` and
//! `import` or `import*` targets walked from an entry file. Both the local
//! evaluator and the engine's repository configs resolve it here, so the
//! rules cannot drift: comments are stripped before matching, remote
//! (`:`) and absolute imports are refused, an import cannot climb out of
//! the entry's tree, and the walk reads at most 100 modules.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

/// The text of `entry` and every local module it imports, keyed by the
/// normalized path `read` resolves them under. `read` returns a module's
/// text, or `None` when there is none: the schema module's own text stands
/// in for a missing `schema_name` file, and any other missing module
/// fails.
pub fn import_files(
    entry: &str,
    schema_name: &str,
    schema: &str,
    read: &mut dyn FnMut(&str) -> Result<Option<String>, String>,
) -> Result<BTreeMap<String, String>, String> {
    let imports = regex::Regex::new(r#"(?:amends|extends|import\*?)\s*\(?\s*"([^"]+)""#)
        .expect("import regex");
    let mut files = BTreeMap::new();
    let mut pending = vec![normalize(Path::new(entry))?];
    let mut visited = BTreeSet::new();
    while let Some(path) = pending.pop() {
        if !visited.insert(path.clone()) {
            continue;
        }
        if visited.len() > 100 {
            return Err("config import budget exceeded".into());
        }
        let text = match read(&path)? {
            Some(text) => text,
            None if Path::new(&path)
                .file_name()
                .is_some_and(|name| name == schema_name) =>
            {
                schema.into()
            }
            None => return Err(format!("missing config module {path}")),
        };
        for captures in imports.captures_iter(&strip_comments(&text)) {
            let relative = &captures[1];
            if relative.contains(':') || relative.starts_with('/') {
                return Err(
                    "config imports must be local modules; remote imports are disabled".into(),
                );
            }
            let parent = Path::new(&path).parent().unwrap_or_else(|| Path::new(""));
            pending.push(normalize(&parent.join(relative))?);
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
    Ok(normalized.to_string_lossy().into_owned())
}

/// `text` without Pkl comments: `//` to the end of the line and `/* … */`,
/// which nests, outside string literals. A `"…"` string keeps its escapes
/// and a `"""…"""` text block is kept whole, so neither a `//` nor a `/*`
/// inside one is taken for a comment.
fn strip_comments(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut depth = 0;
    while i < chars.len() {
        let c = chars[i];
        if depth > 0 {
            if c == '/' && chars.get(i + 1) == Some(&'*') {
                depth += 1;
                i += 2;
            } else if c == '*' && chars.get(i + 1) == Some(&'/') {
                depth -= 1;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        match c {
            '/' if chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                depth = 1;
                i += 2;
            }
            '"' if chars.get(i + 1) == Some(&'"') && chars.get(i + 2) == Some(&'"') => {
                out.extend(['"', '"', '"']);
                i += 3;
                while i < chars.len() {
                    if chars[i] == '"'
                        && chars.get(i + 1) == Some(&'"')
                        && chars.get(i + 2) == Some(&'"')
                    {
                        out.extend(['"', '"', '"']);
                        i += 3;
                        break;
                    }
                    out.push(chars[i]);
                    i += 1;
                }
            }
            '"' => {
                out.push('"');
                i += 1;
                while i < chars.len() {
                    let ch = chars[i];
                    out.push(ch);
                    i += 1;
                    if ch == '\\' && i < chars.len() {
                        out.push(chars[i]);
                        i += 1;
                    } else if ch == '"' {
                        break;
                    }
                }
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(
        files: &BTreeMap<String, String>,
    ) -> impl FnMut(&str) -> Result<Option<String>, String> + '_ {
        move |path| Ok(files.get(path).cloned())
    }

    #[test]
    fn commented_out_imports_are_not_imports() {
        let files = BTreeMap::from([(
            "patchula.pkl".to_owned(),
            "amends \"PatchulaConfig.pkl\"\n// import \"https://example.com/remote.pkl\"\n/* extends \"/abs.pkl\" */\n".to_owned(),
        )]);
        let modules = import_files(
            "patchula.pkl",
            "PatchulaConfig.pkl",
            "schema",
            &mut read(&files),
        )
        .unwrap();
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
        ] {
            let files = BTreeMap::from([(
                "patchula.pkl".to_owned(),
                format!("amends \"PatchulaConfig.pkl\"\nimport \"{target}\"\n"),
            )]);
            let error = import_files(
                "patchula.pkl",
                "PatchulaConfig.pkl",
                "schema",
                &mut read(&files),
            )
            .unwrap_err();
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
        let error = import_files(
            "mod0.pkl",
            "PatchulaConfig.pkl",
            "schema",
            &mut read(&files),
        )
        .unwrap_err();
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
        let modules = import_files(
            "patchula.pkl",
            "PatchulaConfig.pkl",
            "schema",
            &mut read(&files),
        )
        .unwrap();
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
        let error = import_files(
            "patchula.pkl",
            "PatchulaConfig.pkl",
            "schema",
            &mut read(&files),
        )
        .unwrap_err();
        assert_eq!(error, "config import escapes repository");
    }
}
