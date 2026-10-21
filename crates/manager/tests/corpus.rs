//! The corpus in tests/corpus holds real manifests from permissively
//! licensed projects. Extracting it must match the checked-in snapshot, one line
//! per dependency, so a pull request's diff shows exactly what it changes.
//! Regenerate with `mise run corpus:snapshot`.
use patchula_manager::{Files, Scanner};
use std::path::Path;

fn walk(root: &Path, dir: &Path, scanner: &Scanner, files: &mut Files) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap())
        .collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, scanner, files);
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if scanner.needs(&relative)
            && let Ok(text) = std::fs::read_to_string(&path)
        {
            files.insert(relative, text);
        }
    }
}

#[test]
fn corpus_extraction_matches_its_snapshot() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus");
    let config = patchula_config::load(&root).unwrap();
    let scanner = Scanner::new(&config).unwrap();
    let mut files = Files::new();
    walk(&root, &root, &scanner, &mut files);
    let extraction = scanner.extract(&files).unwrap();
    let mut lines: Vec<String> = extraction
        .dependencies
        .iter()
        .map(|d| {
            format!(
                "{}\t{}\t{}\t{}\t{}",
                serde_json::to_value(d.manager).unwrap().as_str().unwrap(),
                d.file,
                d.name,
                d.current,
                d.resolved.as_deref().unwrap_or("-")
            )
        })
        .collect();
    // Every declaration must be editable in place: rewriting it to its own
    // current value exercises the edit path without registry data.
    for dep in &extraction.dependencies {
        if let Err(error) =
            patchula_manager::edits(dep, &files[&dep.file], &dep.current, dep.digest.as_deref())
        {
            lines.push(format!("edit-error\t{}\t{}\t{error}", dep.file, dep.name));
        }
    }
    lines.sort();
    lines.extend(extraction.warnings.iter().map(|w| format!("warning\t{w}")));
    let actual = lines.join("\n") + "\n";
    let snapshot = root.join("../corpus-extraction.tsv");
    if std::env::var_os("PATCHULA_UPDATE_SNAPSHOT").is_some() {
        std::fs::write(&snapshot, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&snapshot).unwrap_or_default();
    if actual != expected {
        let missing: Vec<_> = expected
            .lines()
            .filter(|l| !actual.contains(*l))
            .take(10)
            .collect();
        let extra: Vec<_> = actual
            .lines()
            .filter(|l| !expected.contains(*l))
            .take(10)
            .collect();
        panic!(
            "corpus extraction changed; run `mise run corpus:snapshot` if intended.\nmissing: {missing:#?}\nextra: {extra:#?}"
        );
    }
}
