//! The local-module closure of a Pkl config: `amends`, `extends` and
//! `import` or `import*` targets walked from an entry file. Both the local
//! evaluator and the engine's repository configs resolve it here, so the
//! rules cannot drift: imports are read from pklr's own tokens, so they are
//! what the evaluator sees, remote (`:`), absolute and non-POSIX imports are
//! refused, an
//! import cannot climb out of the entry's directory, and the walk reads at
//! most 100 modules.
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::Path;

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
        let imports =
            pkl_imports(&text).map_err(|e| format!("cannot read config module {path}: {e}"))?;
        for import in imports {
            let relative = import.target;
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
    crate::normalize_path(path)
        .ok_or_else(|| "config import escapes repository".to_owned())?
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| "config path is not UTF-8".to_owned())
}

/// Whether `path` stays inside `root`. An empty root is the repository
/// itself: every repository-relative path is inside it.
fn within(root: &Path, path: &Path) -> bool {
    root.as_os_str().is_empty() || path.starts_with(root)
}

/// One `amends`, `extends`, `import` or `import*` target in a Pkl module:
/// the string it names, and the byte range of that string's contents in the
/// module's text when they are written exactly as they read, with no escape
/// or line break to decode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PklImport {
    pub target: String,
    /// The byte offset the string literal starts at.
    pub at: usize,
    pub range: Option<Range<usize>>,
}

/// The targets of the `amends`, `extends`, `import` and `import*` clauses
/// and `import(…)` and `import*(…)` expressions `text` carries, in source
/// order, interpolations included. They are read from pklr's tokens, the
/// lexer the evaluator itself uses, so comments, every string form and
/// escape, quoted identifiers and shebang lines read exactly as Pkl reads
/// them. Config loading and the `pkl` manager both read imports here. A
/// module pklr cannot lex is an error, never an empty list.
pub fn pkl_imports(text: &str) -> Result<Vec<PklImport>, String> {
    // pklr's message can quote the text it failed on, which may be anything
    // the module holds; only the line is reported.
    let tokens = pklr::lexer::lex(text).map_err(|e| {
        let at = match &e {
            pklr::Error::Lex { span, .. } => Some(span.offset()),
            _ => None,
        };
        match at.and_then(|at| text.get(..at)) {
            Some(before) => format!("not valid Pkl at line {}", before.matches('\n').count() + 1),
            None => "not valid Pkl".to_owned(),
        }
    })?;
    let mut imports = Vec::new();
    collect(text, &tokens, &mut imports);
    Ok(imports)
}

fn collect(text: &str, tokens: &[pklr::lexer::Token], out: &mut Vec<PklImport>) {
    use pklr::lexer::{StringPart, TokenKind};
    // pklr's parser skips semicolons after every token, so `import; "x"`
    // and `import(; "x")` are imports too; the walk skips them the same way.
    let after = |mut i: usize| {
        i += 1;
        while tokens
            .get(i)
            .is_some_and(|t| t.kind == TokenKind::Semicolon)
        {
            i += 1;
        }
        i
    };
    for (i, token) in tokens.iter().enumerate() {
        match &token.kind {
            TokenKind::KwAmends
            | TokenKind::KwExtends
            | TokenKind::KwImport
            | TokenKind::KwImportStar => {
                let mut at = after(i);
                // Only `import` and `import*` take parentheses, as
                // expressions, and the string must close them.
                let call = matches!(token.kind, TokenKind::KwImport | TokenKind::KwImportStar)
                    && tokens.get(at).is_some_and(|t| t.kind == TokenKind::LParen);
                if call {
                    at = after(at);
                }
                let closed = !call
                    || tokens
                        .get(after(at))
                        .is_some_and(|t| t.kind == TokenKind::RParen);
                if let Some(literal) = tokens.get(at)
                    && let TokenKind::StringLit(target) = &literal.kind
                    && closed
                {
                    out.push(PklImport {
                        target: target.clone(),
                        at: literal.offset,
                        range: contents(text, literal.offset..literal.end, target),
                    });
                }
            }
            TokenKind::InterpolatedString(parts) => {
                for part in parts {
                    if let StringPart::Tokens(inner) = part {
                        collect(text, inner, out);
                    }
                }
            }
            _ => {}
        }
    }
}

/// The byte range of a string literal's contents within `span`, its
/// delimiters (`#`s, then `"` or `"""`, and their mirror) stripped, when the
/// raw contents are `decoded` exactly: one line, no escapes.
fn contents(text: &str, span: Range<usize>, decoded: &str) -> Option<Range<usize>> {
    let raw = text.get(span.clone())?;
    let pounds = raw.bytes().take_while(|b| *b == b'#').count();
    let quotes = if raw[pounds..].starts_with("\"\"\"") {
        3
    } else {
        1
    };
    let open = pounds + quotes;
    let close = raw.len().checked_sub(open)?;
    let inner = raw.get(open..close)?;
    (inner == decoded && !inner.contains(['\n', '\r']))
        .then(|| span.start + open..span.start + close)
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
                // A custom delimiter keeps each backslash a plain character.
                format!("amends \"PatchulaConfig.pkl\"\nimport #\"{target}\"#\n"),
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
    fn a_revisited_module_is_skipped_and_a_missing_one_fails() {
        // A diamond: both a.pkl and b.pkl import shared.pkl; the second
        // visit is skipped rather than read or failed again.
        let files = BTreeMap::from([
            (
                "entry.pkl".to_owned(),
                "import \"a.pkl\"\nimport \"b.pkl\"\n".to_owned(),
            ),
            ("a.pkl".to_owned(), "import \"shared.pkl\"\n".to_owned()),
            ("b.pkl".to_owned(), "import \"shared.pkl\"\n".to_owned()),
            ("shared.pkl".to_owned(), String::new()),
        ]);
        let modules = walk("entry.pkl", &files).unwrap();
        assert!(modules.contains_key("shared.pkl"));
        // Only the schema module stands in for a missing file.
        let files = BTreeMap::from([("entry.pkl".to_owned(), "import \"gone.pkl\"\n".to_owned())]);
        assert_eq!(
            walk("entry.pkl", &files).unwrap_err(),
            "missing config module gone.pkl"
        );
    }

    #[test]
    fn dot_components_resolve_to_the_same_module() {
        let files = BTreeMap::from([
            (
                "dir/patchula.pkl".to_owned(),
                "import \"./shared.pkl\"\nimport \"sub/../other.pkl\"\n".to_owned(),
            ),
            ("dir/shared.pkl".to_owned(), String::new()),
            ("dir/other.pkl".to_owned(), String::new()),
        ]);
        let modules = walk("dir/patchula.pkl", &files).unwrap();
        assert!(modules.contains_key("dir/shared.pkl"));
        assert!(modules.contains_key("dir/other.pkl"));
    }

    fn targets(text: &str) -> Vec<String> {
        pkl_imports(text)
            .unwrap()
            .into_iter()
            .map(|i| i.target)
            .collect()
    }

    #[test]
    fn pkl_imports_reads_every_clause_form() {
        for (text, expected) in [
            ("amends \"a.pkl\"", vec!["a.pkl"]),
            ("extends \"b.pkl\"", vec!["b.pkl"]),
            ("import \"c.pkl\"", vec!["c.pkl"]),
            ("import* \"d.pkl\"", vec!["d.pkl"]),
            ("import (\"e.pkl\")", vec!["e.pkl"]),
            ("amends /* note */ \"f.pkl\"", vec!["f.pkl"]),
            ("extends // note\n\"g.pkl\"", vec!["g.pkl"]),
            ("import*\n// note\n/* note */\n(\"h.pkl\")", vec!["h.pkl"]),
            ("import ##\"raw-hash.pkl\"##", vec!["raw-hash.pkl"]),
            ("import \"\"\"\ni.pkl\n\"\"\"", vec!["i.pkl"]),
            // A clause keyword with no string after it is not an import.
            ("import = value", vec![]),
            ("import (compute())", vec![]),
            ("extends", vec![]),
        ] {
            assert_eq!(targets(text), expected, "{text}");
        }
    }

    #[test]
    fn the_lexer_skips_strings_comments_and_interpolations() {
        // A nested block comment and interpolations inside every string
        // form, none of which import anything.
        let text = concat!(
            "/* outer /* inner */ tail */ c = 1\n",
            "d = \"x\\(f(\"s\", 1) /* c */ \n g(2))y\"\n",
            "e = \"\"\"\n  x\\(h(3))y\n  \"\"\"\n",
            "f = #\"raw \\(not-code)\"#\n",
            "import \"real.pkl\"\n",
        );
        assert_eq!(targets(text), ["real.pkl"]);
    }

    /// Escapes reach past what they escape, and a custom delimiter changes
    /// the escape: `\"""` does not close a text block, `\#"` does not close a
    /// `#"…"#` string, and a plain `\(` there is text.
    #[test]
    fn escapes_never_end_a_string_early() {
        let text = concat!(
            "a = \"\"\"\n  \\\"\"\"\n  import \"inside.pkl\"\n\"\"\"\n",
            "b = #\"before \\#\"# import \"inside.pkl\" \\(x)\"#\n",
            "c = \"\\\\\"\n",
            "import \"real.pkl\"\n",
        );
        assert_eq!(targets(text), ["real.pkl"]);
    }

    /// An import inside an interpolation is code, and an import: plain and
    /// custom-delimited strings alike.
    #[test]
    fn imports_inside_interpolations_count() {
        let text = concat!(
            "x = \"\\(import(\"a.pkl\").v)\"\n",
            "y = #\"\\#(import(\"b.pkl\").v) \\(import(\"not.pkl\"))\"#\n",
            "z = \"\"\"\n\\((import(\"c.pkl\")).v)\n\"\"\"\n",
        );
        assert_eq!(targets(text), ["a.pkl", "b.pkl", "c.pkl"]);
    }

    /// A target's range is in bytes, past any multi-byte text before it.
    #[test]
    fn pkl_imports_ranges_are_byte_offsets() {
        let text = "// é ü\nimport \"package://x/y@1.0.0#/Z.pkl\"\n";
        let imports = pkl_imports(text).unwrap();
        assert_eq!(imports.len(), 1);
        assert_eq!(&text[imports[0].range.clone().unwrap()], imports[0].target);
        assert_eq!(imports[0].target, "package://x/y@1.0.0#/Z.pkl");
        // Contents that decode to something else have no range to edit.
        for text in [
            "import \"a\\u{62}c.pkl\"",
            "import \"\"\"\n  abc.pkl\n  \"\"\"",
        ] {
            let imports = pkl_imports(text).unwrap();
            assert_eq!(imports[0].target, "abc.pkl", "{text}");
            assert_eq!(imports[0].range, None, "{text}");
        }
        // A custom delimiter makes a backslash plain text: the contents are
        // the target as written.
        let text = "import #\"a\\b.pkl\"#";
        let imports = pkl_imports(text).unwrap();
        assert_eq!(imports[0].target, "a\\b.pkl");
        assert_eq!(&text[imports[0].range.clone().unwrap()], "a\\b.pkl");
    }

    /// Quoted identifiers, `$` and Unicode identifiers, shebang lines and
    /// `import` as a member name read as Pkl reads them: no import is
    /// invented, and none after them is missed.
    #[test]
    fn identifiers_and_shebangs_read_as_pkl_reads_them() {
        let text = concat!(
            "#!/usr/bin/env -S pkl eval --allowed-modules=file:/*\n",
            "routes { `/*` = \"fallback\" }\n",
            "headers { `x-\"quote` = \"1\" }\n",
            "`import \"quoted.pkl\"` = 1\n",
            "local function $import(s: String) = s\n",
            "x = $import(\"dollar.pkl\")\n",
            "y = éimport(\"unicode.pkl\")\n",
            "z = \"\\($import(1))\"\n",
            "w = \"\\(import(compute()))\"\n",
            "import \"real.pkl\"\n",
        );
        assert_eq!(targets(text), ["real.pkl"]);
    }

    /// pklr's parser skips semicolons after every token, so these are imports
    /// it resolves; an `import(…)` whose string does not close it is not.
    #[test]
    fn imports_read_as_pklrs_parser_reads_them() {
        let text = concat!(
            "amends; \"a.pkl\"\n",
            "import; \"b.pkl\"\n",
            "x = import;(; \"c.pkl\";)\n",
            "y = import(\"not.pkl\" + suffix)\n",
        );
        assert_eq!(targets(text), ["a.pkl", "b.pkl", "c.pkl"]);
    }

    /// A lex error names the line, never the text pklr failed on.
    #[test]
    fn lex_errors_quote_no_source() {
        let error = pkl_imports("a = 1\nx = \"\\u{secretToken123").unwrap_err();
        assert_eq!(error, "not valid Pkl at line 2");
    }

    /// A module pklr cannot lex is an error, not an empty import list, and
    /// hostile nesting fails fast rather than taking exponential time.
    #[test]
    fn unlexable_modules_are_errors() {
        assert!(pkl_imports("x = \"\\(import(\"a.pkl\"))").is_err());
        let started = std::time::Instant::now();
        assert!(pkl_imports(&"\"\\(".repeat(40)).is_err());
        assert!(pkl_imports(&"\"\\(".repeat(10_000)).is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
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
