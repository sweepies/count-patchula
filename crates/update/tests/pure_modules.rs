//! `src/pr/` is the pure half of the pull request lifecycle: it decides,
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

#[test]
fn pr_view_sources_never_do_io() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/pr");
    let mut files = Vec::new();
    sources(&dir, &mut files);
    assert!(!files.is_empty(), "{dir:?} has no sources");
    let excluded: Vec<PathBuf> = files
        .iter()
        .flat_map(|file| test_modules(file, &std::fs::read_to_string(file).unwrap()))
        .collect();
    for file in files.iter().filter(|file| !excluded.contains(file)) {
        let text = std::fs::read_to_string(file).unwrap();
        // Inline test modules come last in each file (clippy's
        // `items_after_test_module` keeps them there), so production code is
        // everything before the first one.
        let code = text.split("#[cfg(test)]").next().unwrap();
        for (index, line) in code.lines().enumerate() {
            let code_part = line.split("//").next().unwrap();
            for word in ["Client", "Cache", "async"] {
                assert!(
                    !names(code_part, word),
                    "{}:{} names {word} in production code: {line}",
                    file.display(),
                    index + 1
                );
            }
            assert!(
                !code_part.contains(".await"),
                "{}:{} awaits in production code: {line}",
                file.display(),
                index + 1
            );
        }
    }
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
