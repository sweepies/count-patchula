//! The per-manager proposal facts live on `patchula_manager::Dependency`;
//! lookup's `src/` keeps policy and names no `Manager::` variant. This test
//! scans the sources so the next manager-shaped predicate cannot slip back
//! in: it fails on any `Manager::` outside a `#[cfg(test)]` module.
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

/// A file's `#[cfg(test)]` module is excluded — unit tests may build
/// manager-named fixtures — by scanning only what precedes the first
/// `#[cfg(test)]` attribute, which is where each file's test module starts.
#[test]
fn no_lookup_source_names_a_manager_variant() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&src, &mut files);
    assert!(!files.is_empty());
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        let code = text.split("#[cfg(test)]").next().unwrap();
        for (index, line) in code.lines().enumerate() {
            assert!(
                !line.contains("Manager::"),
                "{}:{} names a Manager:: variant: {line}",
                file.display(),
                index + 1
            );
        }
    }
}
