//! The per-manager proposal facts live on `patchula_manager::Dependency`.
//! Lookup, the engine and update keep policy and never name a manager: this
//! test fails if their production sources mention `Manager` at all — a
//! variant, an import or an alias — so the next manager-shaped predicate
//! cannot slip back in.
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

/// A word-bounded `Manager`: `packageManager` and `ManagerKind` do not count.
fn names_manager(line: &str) -> bool {
    line.match_indices("Manager").any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + "Manager".len()..].chars().next();
        !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
            && !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

#[test]
fn lookup_engine_and_update_sources_never_name_a_manager() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for name in ["lookup", "engine", "update"] {
        sources(&crates.join(name).join("src"), &mut files);
    }
    assert!(files.len() > 10, "{files:?}");
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
            assert!(
                !names_manager(code_part),
                "{}:{} names Manager in production code: {line}",
                file.display(),
                index + 1
            );
        }
    }
}

#[test]
fn the_scan_sees_aliases_and_ignores_longer_words() {
    assert!(names_manager("use patchula_words::Manager as M;"));
    assert!(names_manager("matches!(dep.manager, Manager::Npm)"));
    assert!(!names_manager("// a packageManager field"));
    assert!(!names_manager("let packageManager = 1;"));
}
