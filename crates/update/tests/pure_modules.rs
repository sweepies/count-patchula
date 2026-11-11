//! `src/pr.rs` and `src/pr/` are the pure half of the pull request lifecycle: it decides,
//! it never asks. This test fails if a `Client`, a `Cache`, `async` or an
//! `.await` slips back into its production sources — the lifecycle's rule
//! is that a module is pure or it does I/O, never both. In the style of
//! lookup's `manager_facts.rs`.
use std::path::{Path, PathBuf};

fn sources(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, files);
        } else if path.extension().is_some_and(|e| e == "rs") {
            files.push(path);
        }
    }
}

/// The files `file` declares as `#[cfg(test)] mod name;`: whole test-only
/// modules, resolved the way rustc resolves a non-inline module.
fn test_modules(file: &Path, text: &str) -> Vec<PathBuf> {
    let stem = file.file_stem().unwrap().to_str().unwrap();
    let dir = if matches!(stem, "lib" | "main" | "mod") {
        file.parent().unwrap().to_path_buf()
    } else {
        file.parent().unwrap().join(stem)
    };
    let mut modules = Vec::new();
    let mut lines = text.lines().map(str::trim);
    while let Some(line) = lines.next() {
        if line != "#[cfg(test)]" {
            continue;
        }
        if let Some(name) = lines
            .next()
            .and_then(|next| next.strip_prefix("mod "))
            .and_then(|rest| rest.strip_suffix(';'))
        {
            modules.push(dir.join(format!("{name}.rs")));
            modules.push(dir.join(name).join("mod.rs"));
        }
    }
    modules
}

/// A word-bounded `word`: `clients` and `CacheRow` do not count.
fn names(line: &str, word: &str) -> bool {
    line.match_indices(word).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + word.len()..].chars().next();
        !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
            && !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

/// `{` minus `}` on `line`, outside string and character literals and
/// `//` comments.
fn depth_change(line: &str) -> i64 {
    let mut depth = 0;
    let mut chars = line.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => match c {
                '\\' => {
                    chars.next();
                }
                c if c == q => quote = None,
                _ => {}
            },
            None => match c {
                '"' => quote = Some('"'),
                '\'' if chars.clone().nth(1) == Some('\'') => quote = Some('\''),
                '/' if chars.peek() == Some(&'/') => break,
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            },
        }
    }
    depth
}

/// The production lines of `text`, numbered from 1. A `#[cfg(test)]`
/// attribute may only introduce `mod name;` (a test-only file, scanned
/// elsewhere as excluded) or one inline `mod name {` that closes on the
/// file's last line; any other test-only item would hide what follows it.
fn production(file: &Path, text: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let last = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .unwrap_or(0);
    let mut kept = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        if line.trim() != "#[cfg(test)]" {
            kept.push((index + 1, line.to_owned()));
            index += 1;
            continue;
        }
        let next = lines.get(index + 1).map_or("", |l| l.trim());
        let module = next
            .strip_prefix("mod ")
            .or_else(|| next.strip_prefix("pub(crate) mod "));
        match module {
            Some(rest) if rest.ends_with(';') => index += 2,
            Some(rest) if rest.ends_with('{') => {
                let mut depth = 0;
                let mut close = None;
                for (at, body) in lines.iter().enumerate().skip(index + 1) {
                    depth += depth_change(body);
                    if depth == 0 {
                        close = Some(at);
                        break;
                    }
                }
                assert_eq!(
                    close,
                    Some(last),
                    "{}:{}: an inline test module must close on the file's last line",
                    file.display(),
                    index + 1
                );
                break;
            }
            _ => panic!(
                "{}:{}: #[cfg(test)] may only introduce a test module: {next}",
                file.display(),
                index + 1
            ),
        }
    }
    kept
}

#[test]
fn pr_view_sources_never_do_io() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![src.join("pr.rs")];
    sources(&src.join("pr"), &mut files);
    let excluded: Vec<PathBuf> = files
        .iter()
        .flat_map(|file| test_modules(file, &std::fs::read_to_string(file).unwrap()))
        .collect();
    for file in files.iter().filter(|file| !excluded.contains(file)) {
        let text = std::fs::read_to_string(file).unwrap();
        for (number, line) in production(file, &text) {
            let code_part = line.split("//").next().unwrap();
            for word in ["Client", "Cache", "async", "block_on"] {
                assert!(
                    !names(code_part, word),
                    "{}:{number} names {word} in production code: {line}",
                    file.display(),
                );
            }
            assert!(
                !code_part.contains(".await"),
                "{}:{number} awaits in production code: {line}",
                file.display(),
            );
        }
    }
}

#[test]
fn production_lines_stop_only_at_a_closing_test_module() {
    let file = Path::new("x.rs");
    let ok = "fn a() {}\n#[cfg(test)]\nmod oracle;\nfn b() {}\n#[cfg(test)]\nmod tests {\n    fn c() { let _ = \"}\"; }\n}\n";
    let kept: Vec<_> = production(file, ok).into_iter().map(|(_, l)| l).collect();
    assert_eq!(kept, ["fn a() {}", "fn b() {}"]);
    let after = "#[cfg(test)]\nmod tests {\n}\nasync fn hidden() {}\n";
    assert!(std::panic::catch_unwind(|| production(file, after)).is_err());
    let item = "#[cfg(test)]\nfn helper() {}\nasync fn hidden() {}\n";
    assert!(std::panic::catch_unwind(|| production(file, item)).is_err());
}

#[test]
fn the_scan_sees_the_words_it_bans() {
    assert!(names("use patchula_github::Client;", "Client"));
    assert!(names("let cache: Cache;", "Cache"));
    assert!(names("async fn read() {}", "async"));
    assert!(!names("let clients = 1;", "Client"));
    assert!(!names("let cached_pr = 1;", "Cache"));
    assert!(!names("let async_count = 1;", "async"));
}
