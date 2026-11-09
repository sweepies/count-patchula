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
    let (paths, files, _deep) = patchula_engine::read_tree(root, &scanner).unwrap();
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
    let (paths, files, _deep) = patchula_engine::read_tree(root, &scanner).unwrap();
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
    let (_, files, _) = patchula_engine::read_tree(root, &scanner).unwrap();
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
    let (paths, files, _deep) = patchula_engine::read_tree(&root, &scanner).unwrap();
    assert!(files.is_empty(), "{files:?}");
    assert!(!paths.contains("app/package.json"));
}

/// A settings file behind a symlink could name a private registry, so it is
/// read as unreadable, never as absent: its packages' lookups fail rather than
/// go to the public registry.
#[cfg(unix)]
#[test]
fn a_symlinked_settings_file_is_unreadable_not_absent() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("checkout");
    git(dir.path(), &["init", "-q", "checkout"]);
    write(
        &root,
        "pyproject.toml",
        "[project]\nname = \"p\"\ndependencies = [\"acme-a>=1\"]\n",
    );
    write(
        &root,
        "config/uv.toml",
        "index-url = \"https://pypi.acme.dev/simple\"\n",
    );
    std::os::unix::fs::symlink("config/uv.toml", root.join("uv.toml")).unwrap();
    write(
        &root,
        "Gemfile",
        "source \"https://rubygems.org\"\ngem \"rake\", \"~> 13.0\"\n",
    );
    write(
        &root,
        "config/bundle",
        "BUNDLE_MIRROR__ALL: \"https://gems.acme.dev\"\n",
    );
    std::fs::create_dir_all(root.join(".bundle")).unwrap();
    std::os::unix::fs::symlink("../config/bundle", root.join(".bundle/config")).unwrap();
    // A linked `.bundle` directory, which Git lists as one path.
    write(
        &root,
        "app/Gemfile",
        "source \"https://rubygems.org\"\ngem \"rack\", \"~> 3.0\"\n",
    );
    write(
        &root,
        "config/bundle-dir/config",
        "BUNDLE_MIRROR__ALL: \"https://gems.acme.dev\"\n",
    );
    std::os::unix::fs::symlink("../config/bundle-dir", root.join("app/.bundle")).unwrap();
    let scanner = scanner("amends \"PatchulaConfig.pkl\"\n");
    let (_, files, _) = patchula_engine::read_tree(&root, &scanner).unwrap();
    assert_eq!(
        files.get("uv.toml"),
        Some(&patchula_manager::Text::Unread(
            patchula_manager::Unread::Failed
        ))
    );
    let extraction = scanner.extract(&files).unwrap();
    let registry = extraction
        .dependencies
        .iter()
        .find(|d| d.name == "acme-a")
        .and_then(|d| d.registry.clone());
    assert!(
        registry
            .as_deref()
            .is_some_and(|r| r.contains("uv.toml the Count cannot read")),
        "{registry:?}"
    );
    assert_eq!(
        files.get(".bundle/config"),
        Some(&patchula_manager::Text::Unread(
            patchula_manager::Unread::Failed
        ))
    );
    assert_eq!(
        files.get("app/.bundle/config"),
        Some(&patchula_manager::Text::Unread(
            patchula_manager::Unread::Failed
        ))
    );
    let rake = extraction
        .dependencies
        .iter()
        .find(|d| d.name == "rake")
        .and_then(|d| d.registry.clone());
    assert!(
        rake.as_deref()
            .is_some_and(|r| r.contains(".bundle/config, which the Count cannot read")),
        "{rake:?}"
    );
}

/// The sources of a build Gradle settings include are read in a second
/// pass, since only the settings say where it is.
#[test]
fn the_sources_of_an_included_gradle_build_are_read() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("checkout");
    git(dir.path(), &["init", "-q", "checkout"]);
    write(
        &root,
        "settings.gradle.kts",
        "includeBuild(\"tools/logic\")\n",
    );
    write(
        &root,
        "build.gradle.kts",
        "repositories { mavenCentral() }\n",
    );
    write(
        &root,
        "tools/logic/src/main/kotlin/Repos.kt",
        "fun add(p: Project) { p.repositories.mavenLocal() }\n",
    );
    write(&root, "app/src/main/kotlin/App.kt", "fun main() {}\n");
    let scanner = scanner("amends \"PatchulaConfig.pkl\"\n");
    let (_, files, _) = patchula_engine::read_tree(&root, &scanner).unwrap();
    assert!(files.contains_key("tools/logic/src/main/kotlin/Repos.kt"));
    assert!(!files.contains_key("app/src/main/kotlin/App.kt"));
}

/// A selected file that is not UTF-8 refuses the local read, the same way
/// GitHub extraction refuses the repository, rather than being dropped.
#[test]
fn a_selected_non_utf8_file_refuses_the_read() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    write(root, "package.json", MANIFEST);
    write(root, "app/package.json", MANIFEST);
    std::fs::write(root.join("app/package.json"), b"{\"x\":\xff}").unwrap();
    git(root, &["add", "."]);
    let scanner = scanner("amends \"PatchulaConfig.pkl\"\n");
    let error = patchula_engine::read_tree(root, &scanner).unwrap_err();
    assert_eq!(error.to_string(), "selected file is not UTF-8");
}
