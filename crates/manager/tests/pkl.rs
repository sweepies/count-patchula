use patchula_config::Config;
use patchula_manager::{Extraction, Scanner, apply, edits};
use patchula_words::{Manager, Source};

fn config() -> Config {
    static CONFIG: std::sync::OnceLock<Config> = std::sync::OnceLock::new();
    let mut config = CONFIG
        .get_or_init(|| {
            patchula_config::load_files(
                "/repo/patchula.pkl",
                std::collections::BTreeMap::from([(
                    "/repo/patchula.pkl".into(),
                    "amends \"PatchulaConfig.pkl\"".into(),
                )]),
            )
            .unwrap()
        })
        .clone();
    config.managers = vec![Manager::Pkl];
    config
}

fn extract(path: &str, text: &str) -> Extraction {
    Scanner::new(&config())
        .unwrap()
        .extract(&[(path.to_owned(), text.into())].into_iter().collect())
        .unwrap()
}

/// Each dependency as (name, package, current, located text).
fn read(text: &str) -> Vec<(String, String, String, String)> {
    extract("hk.pkl", text)
        .dependencies
        .into_iter()
        .map(|dep| {
            assert_eq!(dep.manager, Manager::Pkl);
            assert_eq!(dep.source, Source::Pkl);
            assert_eq!(dep.extract, None);
            (
                dep.name,
                dep.package,
                dep.current,
                text[dep.location.start..dep.location.end].to_owned(),
            )
        })
        .collect()
}

fn row(name: &str, package: &str, version: &str) -> (String, String, String, String) {
    (name.into(), package.into(), version.into(), version.into())
}

/// The package is the URI without scheme and version; a release URI's
/// version is located twice, once in the tag and once after the `@`.
#[test]
fn every_uri_form_names_its_package() {
    let text = concat!(
        "amends \"package://github.com/jdx/hk/releases/download/v2.9.0/hk@2.9.0#/Config.pkl\"\n",
        "import \"package://pkg.pkl-lang.org/pkl-pantry/pkl.toml@1.0.0#/toml.pkl\"\n",
        "import* \"package://pkg.pkl-lang.org/github.com/owner/repo/lib@0.3.1-rc.1#/*.pkl\"\n",
        "local x = import(\"package://github.com/a/b/releases/download/b-1.2.3-final/b@1.2.3#/B.pkl\")\n",
    );
    let hk = "github.com/jdx/hk/releases/download/v%version%/hk";
    let b = "github.com/a/b/releases/download/b-%version%-final/b";
    assert_eq!(
        read(text),
        [
            row("hk", hk, "2.9.0"),
            row("hk", hk, "2.9.0"),
            row("pkl.toml", "pkg.pkl-lang.org/pkl-pantry/pkl.toml", "1.0.0"),
            row(
                "lib",
                "pkg.pkl-lang.org/github.com/owner/repo/lib",
                "0.3.1-rc.1"
            ),
            row("b", b, "1.2.3"),
            row("b", b, "1.2.3"),
        ]
    );
    // The package names its stream: the repository and the tag spelling.
    for (package, repository, tag, version) in [
        (hk, "jdx/hk", "v2.11.1", "2.11.1"),
        (
            "pkg.pkl-lang.org/pkl-pantry/pkl.toml",
            "apple/pkl-pantry",
            "pkl.toml@1.0.3",
            "1.0.3",
        ),
        (
            "pkg.pkl-lang.org/github.com/owner/repo/lib",
            "owner/repo",
            "lib@1.0.0",
            "1.0.0",
        ),
        (b, "a/b", "b-1.3.0-final", "1.3.0"),
    ] {
        let stream = patchula_config::PklPackage::parse(package).unwrap();
        assert_eq!(stream.repository, repository, "{package}");
        assert_eq!(stream.version(tag), Some(version), "{package}");
    }
}

/// Two packages of one monorepo are two packages, whatever their versions.
#[test]
fn monorepo_packages_stay_apart() {
    let text = concat!(
        "import \"package://pkg.pkl-lang.org/pkl-pantry/pkl.toml@1.0.0#/toml.pkl\"\n",
        "import \"package://pkg.pkl-lang.org/pkl-pantry/k8s.contrib@1.0.0#/x.pkl\"\n",
    );
    let deps = extract("x.pkl", text).dependencies;
    assert_eq!(deps.len(), 2);
    assert_ne!(deps[0].id(), deps[1].id());
    assert_ne!(deps[0].declaration().key(), deps[1].declaration().key());
}

#[test]
fn comments_strings_and_other_imports_are_not_packages() {
    let text = concat!(
        "// import \"package://github.com/a/b/releases/download/v1.0.0/b@1.0.0#/B.pkl\"\n",
        "/* amends \"package://github.com/a/b/releases/download/v1.0.0/b@1.0.0#/B.pkl\" */\n",
        "x = \"import \\\"package://github.com/a/b/releases/download/v1.0.0/b@1.0.0#/B.pkl\\\"\"\n",
        "y = \"package://github.com/a/b/releases/download/v1.0.0/b@1.0.0#/B.pkl\"\n",
        "import \"local.pkl\"\n",
        "import \"https://example.com/x.pkl\"\n",
        "import \"@deps/x.pkl\"\n",
        "import \"pkl:json\"\n",
    );
    let extraction = extract("x.pkl", text);
    assert!(extraction.dependencies.is_empty());
    assert!(extraction.skipped.is_empty());
}

#[test]
fn unreadable_package_uris_are_skipped_with_a_reason() {
    let text = concat!(
        "import \"package://example.com/x/y@1.0.0#/Y.pkl\"\n",
        "import \"package://github.com/a/b/archive/b@1.0.0#/B.pkl\"\n",
        "import \"package://pkg.pkl-lang.org/pkl-pantry/pkl.toml@1.0.0::sha256:abc#/toml.pkl\"\n",
        "import \"package://github.com/a/b/releases/download/latest/b@1.0.0#/B.pkl\"\n",
        "import \"package://github.com/a/b/releases/download/1.0.0-1.0.0/b@1.0.0#/B.pkl\"\n",
        "import \"package://pkg.pkl-lang.org/p/x@1.0#/X.pkl\"\n",
        "import \"package://user:secret@example.com/x/y@1.0.0#/Y.pkl\"\n",
        "import \"package://pkg.pkl-lang.org/p/x\\u{40}1.0.0#/X.pkl\"\n",
        "import \"package://GitHub.com/a/b/releases/download/v1.0.0/b@1.0.0#/B.pkl\"\n",
        "import \"package://example.com/a/b@1.0.0?token=secret#/A.pkl\"\n",
        "import \"package://user:pa/ss@example.com/a/b@1.0.0#/A.pkl\"\n",
        "import \"package://github.com/a/b/releases/download/1.1.1.1.1/b@1.1.1#/B.pkl\"\n",
        "import \"package://github.com/apple/pkl-pantry/releases/download/pkl.toml%401.0.0/pkl.toml@1.0.0#/toml.pkl\"\n",
        "import \"package://[::1]/a/b@1.0.0#/A.pkl\"\n",
        "import \"package://pkg.pkl-lang.org/pkl-pantry/..@1.0.0#/A.pkl\"\n",
        "import \"package://pkg.pkl-lang.org/pkl-pantry/a|b@1.0.0#/A.pkl\"\n",
        "import \"package://pkg.pkl-lang.org/pkl-pantry/pkl.toml@1.0.0?token=x#/toml.pkl\"\n",
        "import \"package://pkg.pkl-lang.org/pkl-pantry/pkl.toml@1.0.0?x=::y#/toml.pkl\"\n",
        "import \"package://pkg.pkl-lang.org/github.com/foo@1.0.0#/X.pkl\"\n",
        "import \"package://example.com\"\n",
    );
    let extraction = extract("x.pkl", text);
    assert!(extraction.dependencies.is_empty());
    let skipped: Vec<_> = extraction
        .skipped
        .iter()
        .map(|s| (s.line, s.value.as_str(), s.reason.as_str()))
        .collect();
    let host = "only github.com and pkg.pkl-lang.org package URIs are supported";
    let path = "package URI does not name a GitHub release";
    assert_eq!(
        skipped,
        [
            (Some(1), "package://example.com/x/y@1.0.0", host),
            (Some(2), "package://github.com/a/b/archive/b@1.0.0", path),
            (
                Some(3),
                "package://pkg.pkl-lang.org/pkl-pantry/pkl.toml@1.0.0",
                "package URIs that pin a checksum are not supported yet",
            ),
            (
                Some(4),
                "package://github.com/a/b/releases/download/latest/b@1.0.0",
                "release tag does not hold the package version exactly once",
            ),
            (
                Some(5),
                "package://github.com/a/b/releases/download/1.0.0-1.0.0/b@1.0.0",
                "release tag does not hold the package version exactly once",
            ),
            (
                Some(6),
                "package://pkg.pkl-lang.org/p/x@1.0",
                "package version is not a semantic version",
            ),
            (Some(7), "<unreadable URL>", host),
            (
                Some(8),
                "package://pkg.pkl-lang.org/p/x@1.0.0",
                "package URIs written with escapes or as a text block are not supported",
            ),
            (
                Some(9),
                "package://GitHub.com/a/b/releases/download/v1.0.0/b@1.0.0",
                host
            ),
            // The query, which may hold a token, is never shown.
            (Some(10), "package://example.com/a/b@1.0.0", host),
            // A password holding `/` ends the host early; it is hidden anyway.
            (Some(11), "<unreadable URL>", host),
            (
                Some(12),
                "package://github.com/a/b/releases/download/1.1.1.1.1/b@1.1.1",
                "release tag does not hold the package version exactly once",
            ),
            (
                Some(13),
                "package://github.com/apple/pkl-pantry/releases/download/pkl.toml%401.0.0/pkl.toml@1.0.0",
                path,
            ),
            (Some(14), "<unreadable URL>", host),
            (
                Some(15),
                "package://pkg.pkl-lang.org/pkl-pantry/..@1.0.0",
                path
            ),
            (
                Some(16),
                "package://pkg.pkl-lang.org/pkl-pantry/a|b@1.0.0",
                path
            ),
            // A query is no part of a package URI, whatever it holds.
            (
                Some(17),
                "package://pkg.pkl-lang.org/pkl-pantry/pkl.toml@1.0.0",
                path
            ),
            (
                Some(18),
                "package://pkg.pkl-lang.org/pkl-pantry/pkl.toml@1.0.0",
                path
            ),
            (
                Some(19),
                "package://pkg.pkl-lang.org/github.com/foo@1.0.0",
                path
            ),
            (Some(20), "package://example.com", host),
        ]
    );
}

fn replace(text: &str, value: &str) -> Result<String, String> {
    let extraction = extract("hk.pkl", text);
    assert!(!extraction.dependencies.is_empty());
    let mut changes = Vec::new();
    for dep in &extraction.dependencies {
        changes.extend(edits(dep, text, value, None)?);
    }
    apply(text, changes)
}

#[test]
fn an_update_moves_every_copy_of_the_version() {
    let text = "amends \"package://github.com/jdx/hk/releases/download/v2.9.0/hk@2.9.0#/Config.pkl\"\nimport \"package://github.com/jdx/hk/releases/download/v2.9.0/hk@2.9.0#/Builtins.pkl\"\n";
    assert_eq!(
        replace(text, "2.11.1").unwrap(),
        text.replace("v2.9.0/hk@2.9.0", "v2.11.1/hk@2.11.1")
    );
    let pantry = "import \"package://pkg.pkl-lang.org/pkl-pantry/pkl.toml@1.0.0#/toml.pkl\"\n";
    assert_eq!(
        replace(pantry, "1.0.3").unwrap(),
        pantry.replace("@1.0.0", "@1.0.3")
    );
    let suffixed =
        "import \"package://github.com/a/b/releases/download/b-1.2.3-final/b@1.2.3#/B.pkl\"\n";
    assert_eq!(
        replace(suffixed, "1.3.0").unwrap(),
        suffixed.replace("b-1.2.3-final/b@1.2.3", "b-1.3.0-final/b@1.3.0")
    );
}

#[test]
fn an_unsafe_replacement_is_refused() {
    let text =
        "import \"package://github.com/jdx/hk/releases/download/v2.9.0/hk@2.9.0#/Config.pkl\"\n";
    for value in ["2.11.1\"", "2.11.1 x", "2.11.1\\"] {
        assert!(replace(text, value).is_err(), "{value}");
    }
}

#[test]
fn an_edit_refuses_a_file_that_changed_under_it() {
    let text =
        "import \"package://github.com/jdx/hk/releases/download/v2.9.0/hk@2.9.0#/Config.pkl\"\n";
    let dep = extract("hk.pkl", text).dependencies.remove(0);
    let moved = text.replace("v2.9.0/hk@2.9.0", "v2.9.1/hk@2.9.1");
    assert!(edits(&dep, &moved, "2.11.1", None).is_err());
}

/// Every Pkl module is read except the repository's own config, whose
/// imports config loading already limits to local modules.
#[test]
fn every_module_but_the_repository_config_is_read() {
    let scanner = Scanner::new(&config()).unwrap();
    for (path, needed) in [
        ("hk.pkl", true),
        ("deep/dir/module.pkl", true),
        ("sub/patchula.pkl", true),
        ("patchula.pkl", false),
        ("PklProject", false),
        ("module.pkl.txt", false),
    ] {
        assert_eq!(scanner.needs(path), needed, "{path}");
    }
}

/// A module pklr cannot lex is skipped with a warning, never read as empty.
#[test]
fn an_unlexable_module_is_skipped_with_a_warning() {
    let extraction = extract(
        "x.pkl",
        "x = \"\\(import(\"package://pkg.pkl-lang.org/pkl-pantry/pkl.toml@1.0.0\"))",
    );
    assert!(extraction.dependencies.is_empty());
    assert_eq!(extraction.warnings.len(), 1, "{:?}", extraction.warnings);
    assert!(
        extraction.warnings[0].starts_with("x.pkl: skipped:"),
        "{:?}",
        extraction.warnings
    );
}

/// Text `.pkl` files that are no Pkl modules import nothing and warn
/// nothing, so they leave auto-merge and cleanup on: Git LFS pointers, and
/// pickles of protocols 0 and 1, which can be valid UTF-8 and always end
/// with `.`. A module that lexes is read whatever it ends with.
#[test]
fn lfs_pointers_and_text_pickles_are_not_modules() {
    for text in [
        "version https://git-lfs.github.com/spec/v1\noid sha256:4ef92c37e5c5c0d5a4e6b9c8f1d2e3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0\nsize 52428800\n",
        "}q\u{0}U\u{1}aq\u{1}K\u{1}s.",
        "(dp0\nS'key'\np1\nI42\ns.",
        "Vsay \"hi\np0\n.",
    ] {
        let extraction = extract("model.pkl", text);
        assert!(extraction.dependencies.is_empty(), "{text:?}");
        assert!(
            extraction.warnings.is_empty(),
            "{text:?}: {:?}",
            extraction.warnings
        );
    }
    let extraction = extract(
        "x.pkl",
        "import \"package://pkg.pkl-lang.org/pkl-pantry/pkl.toml@1.0.0\"\n// the end.",
    );
    assert_eq!(extraction.dependencies.len(), 1);
}

/// A URI the module only reaches through a decoded string is skipped, and a
/// decoded value never carries a line break or terminal escape into a report.
#[test]
fn decoded_uris_are_skipped_and_shown_safely() {
    let text = concat!(
        "import \"package://example.com/a\\nb\\u{1b}[31m/c@1.0.0\"\n",
        "import \"\"\"\n  package://pkg.pkl-lang.org/pkl-pantry/pkl.toml@1.0.0\n  \"\"\"\n",
    );
    let extraction = extract("x.pkl", text);
    assert!(extraction.dependencies.is_empty());
    let skipped: Vec<_> = extraction
        .skipped
        .iter()
        .map(|s| (s.line, s.value.as_str(), s.reason.as_str()))
        .collect();
    assert_eq!(
        skipped,
        [
            (
                Some(1),
                "<unreadable URL>",
                "only github.com and pkg.pkl-lang.org package URIs are supported"
            ),
            (
                Some(2),
                "package://pkg.pkl-lang.org/pkl-pantry/pkl.toml@1.0.0",
                "package URIs written with escapes or as a text block are not supported"
            ),
        ]
    );
}
