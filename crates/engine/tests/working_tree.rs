//! A working tree is read as GitHub would see it once pushed: tracked files and
//! untracked ones Git does not ignore.
use patchula_manager::Scanner;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

const MANIFEST: &str = r#"{"dependencies":{"left-pad":"1.3.0"}}"#;

fn write(root: &Path, path: &str, content: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn scanner(pkl: &str) -> Scanner {
    let config = patchula_config::load_files(
        "/repo/patchula.pkl",
        BTreeMap::from([("/repo/patchula.pkl".into(), pkl.into())]),
    )
    .unwrap();
    Scanner::new(&config).unwrap()
}

#[test]
fn ignored_directories_are_not_read_even_when_the_config_does_not_exclude_them() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    write(root, ".gitignore", "node_modules/\nforced/\n");
    write(root, ".git/info/exclude", "build/\n");
    write(root, "package.json", MANIFEST);
    write(root, "packages/app/package.json", MANIFEST);
    write(root, "node_modules/left-pad/package.json", MANIFEST);
    write(root, "node_modules/@scope/pkg/package.json", MANIFEST);
    write(root, "build/package.json", MANIFEST);
    write(root, "forced/package.json", MANIFEST);
    git(root, &["add", "package.json", ".gitignore"]);
    git(root, &["add", "-f", "forced/package.json"]);
    // The config excludes nothing, so only Git keeps the ignored files out.
    let scanner = scanner("amends \"PatchulaConfig.pkl\"\nexclude = new Listing {}\n");
    let (paths, files) = patchula_engine::read_tree(root, &scanner).unwrap();
    assert_eq!(
        files.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "forced/package.json",
            "package.json",
            "packages/app/package.json"
        ]
    );
    assert!(paths.iter().all(|path| !path.starts_with("node_modules/")
        && !path.starts_with("build/")
        && !path.starts_with(".git/")));
    assert!(paths.contains(".gitignore"));
}

#[test]
fn a_directory_outside_git_is_walked_and_the_default_excludes_apply() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "package.json", MANIFEST);
    write(root, "node_modules/left-pad/package.json", MANIFEST);
    write(root, "bower_components/jquery/package.json", MANIFEST);
    write(root, "tests/app/package.json", MANIFEST);
    write(root, ".git/package.json", MANIFEST);
    let scanner = scanner("amends \"PatchulaConfig.pkl\"\n");
    let (paths, files) = patchula_engine::read_tree(root, &scanner).unwrap();
    assert_eq!(
        files.keys().map(String::as_str).collect::<Vec<_>>(),
        ["package.json"]
    );
    assert!(paths.contains("node_modules/left-pad/package.json"));
    assert!(!paths.iter().any(|path| path.starts_with(".git/")));
}

#[test]
fn a_negated_exclude_reads_a_default_excluded_directory_again() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "examples/app/package.json", MANIFEST);
    write(root, "examples/other/package.json", MANIFEST);
    let scanner = scanner("amends \"PatchulaConfig.pkl\"\nexclude { \"!examples/app/**\" }\n");
    let (_, files) = patchula_engine::read_tree(root, &scanner).unwrap();
    assert_eq!(
        files.keys().map(String::as_str).collect::<Vec<_>>(),
        ["examples/app/package.json"]
    );
}

#[cfg(unix)]
#[test]
fn a_tracked_file_below_a_symlinked_directory_is_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("checkout");
    let outside = dir.path().join("outside");
    write(&outside, "package.json", MANIFEST);
    git(dir.path(), &["init", "-q", "checkout"]);
    write(&root, "app/package.json", MANIFEST);
    git(&root, &["add", "app/package.json"]);
    std::fs::remove_dir_all(root.join("app")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("app")).unwrap();
    let scanner = scanner("amends \"PatchulaConfig.pkl\"\n");
    let (paths, files) = patchula_engine::read_tree(&root, &scanner).unwrap();
    assert!(files.is_empty(), "{files:?}");
    assert!(!paths.contains("app/package.json"));
}
