//! How the report names each way a current version was resolved: the
//! lockfile's entry for every manager whose lockfile records the resolved
//! version (npm, Bun, Cargo, Bundler, Composer, Pyproject, Go, Terraform),
//! the declared value for every other manager. The remaining kinds are
//! pinned by the actions tests (`floating-ref` for a branch ref,
//! `pinned-commit` for a commit pin, `release-line` for a tool,
//! `supported-range` for a toolchain constraint).
use patchula_cache::Cache;
use patchula_engine::{Engine, Options};
use patchula_testkit::{
    Body, Cassette, Interaction, RecordedRequest, RecordedResponse, Redactor, Replay,
};
use serde_json::json;
use std::sync::Arc;

async fn report(tree: &std::path::Path, cassette: Cassette) -> patchula_report::Report {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spec.json");
    patchula_testkit::save(&path, &cassette, &Redactor::default()).unwrap();
    let replay = Arc::new(Replay::open(&path, patchula_testkit::Options::default()).unwrap());
    let cache = Cache::open(
        dir.path().join("cache.sqlite"),
        patchula_cache::Options::default(),
    )
    .await
    .unwrap();
    let engine = Engine::with_transport("offline", cache, 4, replay.clone())
        .unwrap()
        .with_versions_host(false);
    let report = engine
        .run_at(
            Options {
                local: Some(tree.to_path_buf()),
                ..Options::default()
            },
            true,
            false,
            "2026-02-01T00:00:00Z".parse().unwrap(),
        )
        .await
        .unwrap();
    replay.assert_finished().unwrap();
    report
}

fn interaction(url: &str, accept: &str, body: serde_json::Value) -> Interaction {
    raw(url, accept, Body::Json(body))
}

fn raw(url: &str, accept: &str, body: Body) -> Interaction {
    Interaction {
        request: RecordedRequest {
            method: "GET".into(),
            url: url.into(),
            headers: [("accept".to_owned(), vec![accept.to_owned()])].into(),
            body: Body::Text("".into()),
        },
        response: RecordedResponse {
            status: 200,
            headers: Default::default(),
            body,
        },
    }
}

fn write(tree: &std::path::Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let path = tree.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

/// A `package-lock.json` records where npm resolved the declaration, so the
/// report says `lockfile`.
#[tokio::test]
async fn an_npm_version_recorded_by_its_lockfile_reports_lockfile() {
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree");
    write(
        &tree,
        &[
            (
                "patchula.pkl",
                "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"npm\" }\n",
            ),
            ("package.json", r#"{"dependencies":{"left-pad":"^1.0.0"}}"#),
            (
                "package-lock.json",
                r#"{"packages":{"node_modules/left-pad":{"version":"1.3.0","resolved":"https://registry.npmjs.org/left-pad/-/left-pad-1.3.0.tgz"}}}"#,
            ),
        ],
    );
    let cassette = Cassette {
        schema_version: 1,
        provenance: "Simulated from the npm registry metadata format".into(),
        interactions: vec![interaction(
            "https://registry.npmjs.org/left-pad",
            "application/json",
            json!({
                "versions": {"1.0.0": {}, "1.3.0": {}, "1.3.1": {}, "1.4.0": {}},
                "dist-tags": {"latest": "1.4.0"},
            }),
        )],
    };
    let report = report(&tree, cassette).await;
    let repo = &report.repositories[0];
    assert_eq!(repo.status, patchula_report::Status::Complete, "{repo:?}");
    let dep = &repo.package_files[0].dependencies[0];
    let resolution = dep
        .metadata
        .as_ref()
        .and_then(|m| m.resolution.as_ref())
        .unwrap_or_else(|| panic!("no resolution recorded: {dep:?}"));
    assert_eq!(resolution.kind, patchula_words::ResolutionKind::Lockfile);
    assert_eq!(resolution.version, "1.3.0");
}

/// A `composer.lock` records where Composer resolved the declaration, so the
/// report says `lockfile`, as for npm and Cargo.
#[tokio::test]
async fn a_composer_version_recorded_by_its_lockfile_reports_lockfile() {
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree");
    write(
        &tree,
        &[
            (
                "patchula.pkl",
                "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"composer\" }\n",
            ),
            ("composer.json", r#"{"require":{"acme/lib":"^2.0"}}"#),
            (
                "composer.lock",
                r#"{"packages":[{"name":"acme/lib","version":"2.5.0","notification-url":"https://packagist.org/downloads/"}]}"#,
            ),
        ],
    );
    let cassette = Cassette {
        schema_version: 1,
        provenance: "Simulated from the Packagist p2 metadata format".into(),
        interactions: vec![interaction(
            "https://repo.packagist.org/p2/acme/lib.json",
            "application/json",
            json!({"packages": {"acme/lib": [
                {"version": "2.6.0", "time": "2026-01-01T00:00:00+00:00"},
                {"version": "2.5.0", "time": "2025-12-01T00:00:00+00:00"}
            ]}}),
        )],
    };
    let report = report(&tree, cassette).await;
    let repo = &report.repositories[0];
    assert_eq!(repo.status, patchula_report::Status::Complete, "{repo:?}");
    let dep = &repo.package_files[0].dependencies[0];
    let resolution = dep
        .metadata
        .as_ref()
        .and_then(|m| m.resolution.as_ref())
        .unwrap_or_else(|| panic!("no resolution recorded: {dep:?}"));
    assert_eq!(resolution.kind, patchula_words::ResolutionKind::Lockfile);
    assert_eq!(resolution.version, "2.5.0");
}

/// A `bun.lock` records where Bun resolved the declaration, so the report
/// says `lockfile`, as for npm's `package-lock.json`.
#[tokio::test]
async fn a_bun_version_recorded_by_its_lockfile_reports_lockfile() {
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree");
    write(
        &tree,
        &[
            (
                "patchula.pkl",
                "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"bun\" }\n",
            ),
            ("package.json", r#"{"dependencies":{"left-pad":"^1.0.0"}}"#),
            (
                "bun.lock",
                r#"{"lockfileVersion":1,"workspaces":{"":{"name":"app"}},"packages":{"left-pad":["left-pad@1.3.0","https://registry.npmjs.org/left-pad/-/left-pad-1.3.0.tgz",{},"sha512-x"]}}"#,
            ),
        ],
    );
    let cassette = Cassette {
        schema_version: 1,
        provenance: "Simulated from the npm registry metadata format".into(),
        interactions: vec![interaction(
            "https://registry.npmjs.org/left-pad",
            "application/json",
            json!({
                "versions": {"1.0.0": {}, "1.3.0": {}, "1.3.1": {}, "1.4.0": {}},
                "dist-tags": {"latest": "1.4.0"},
            }),
        )],
    };
    let report = report(&tree, cassette).await;
    let repo = &report.repositories[0];
    assert_eq!(repo.status, patchula_report::Status::Complete, "{repo:?}");
    let dep = &repo.package_files[0].dependencies[0];
    let resolution = dep
        .metadata
        .as_ref()
        .and_then(|m| m.resolution.as_ref())
        .unwrap_or_else(|| panic!("no resolution recorded: {dep:?}"));
    assert_eq!(resolution.kind, patchula_words::ResolutionKind::Lockfile);
    assert_eq!(resolution.version, "1.3.0");
}

/// A `Gemfile.lock` records where Bundler resolved the requirement, so the
/// report says `lockfile`.
#[tokio::test]
async fn a_gem_version_recorded_by_its_lockfile_reports_lockfile() {
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree");
    write(
        &tree,
        &[
            (
                "patchula.pkl",
                "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"bundler\" }\n",
            ),
            (
                "Gemfile",
                "source \"https://rubygems.org\"\ngem \"rake\", \"~> 13.0\"\n",
            ),
            (
                "Gemfile.lock",
                "GEM\n  remote: https://rubygems.org/\n  specs:\n    rake (13.2.0)\n\nDEPENDENCIES\n  rake (~> 13.0)\n",
            ),
        ],
    );
    let cassette = Cassette {
        schema_version: 1,
        provenance: "Simulated from the rubygems.org versions format".into(),
        interactions: vec![interaction(
            "https://rubygems.org/api/v1/versions/rake.json",
            "application/json",
            json!([
                {"number": "13.2.1", "platform": "ruby", "created_at": "2026-01-01T00:00:00Z"},
                {"number": "13.2.0", "platform": "ruby", "created_at": "2025-12-01T00:00:00Z"},
                {"number": "13.0.0", "platform": "ruby", "created_at": "2025-01-01T00:00:00Z"}
            ]),
        )],
    };
    let report = report(&tree, cassette).await;
    let repo = &report.repositories[0];
    assert_eq!(repo.status, patchula_report::Status::Complete, "{repo:?}");
    let dep = &repo.package_files[0].dependencies[0];
    let resolution = dep
        .metadata
        .as_ref()
        .and_then(|m| m.resolution.as_ref())
        .unwrap_or_else(|| panic!("no resolution recorded: {dep:?}"));
    assert_eq!(resolution.kind, patchula_words::ResolutionKind::Lockfile);
    assert_eq!(resolution.version, "13.2.0");
}

/// A `uv.lock` records where uv resolved the requirement, so the report says
/// `lockfile`.
#[tokio::test]
async fn a_pypi_version_recorded_by_its_lockfile_reports_lockfile() {
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree");
    write(
        &tree,
        &[
            (
                "patchula.pkl",
                "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"pyproject\" }\n",
            ),
            (
                "pyproject.toml",
                "[project]\nname = \"demo\"\nversion = \"0.1.0\"\ndependencies = [\"requests>=2.0\"]\n",
            ),
            (
                "uv.lock",
                "version = 1\n\n[[package]]\nname = \"requests\"\nversion = \"2.32.3\"\nsource = { registry = \"https://pypi.org/simple\" }\n",
            ),
        ],
    );
    let cassette = Cassette {
        schema_version: 1,
        provenance: "Simulated from the PyPI JSON API format".into(),
        interactions: vec![interaction(
            "https://pypi.org/pypi/requests/json",
            "application/json",
            json!({"releases": {
                "2.32.3": [{"upload_time_iso_8601": "2025-12-01T00:00:00Z"}],
                "2.33.0": [{"upload_time_iso_8601": "2026-01-01T00:00:00Z"}]
            }}),
        )],
    };
    let report = report(&tree, cassette).await;
    let repo = &report.repositories[0];
    assert_eq!(repo.status, patchula_report::Status::Complete, "{repo:?}");
    let dep = &repo.package_files[0].dependencies[0];
    let resolution = dep
        .metadata
        .as_ref()
        .and_then(|m| m.resolution.as_ref())
        .unwrap_or_else(|| panic!("no resolution recorded: {dep:?}"));
    assert_eq!(resolution.kind, patchula_words::ResolutionKind::Lockfile);
    assert_eq!(resolution.version, "2.32.3");
}

/// A `require` version in `go.mod` is the version Go's resolution recorded,
/// so the report says `lockfile`. The `go` directive declares a language
/// version instead, and stays `declared`.
#[tokio::test]
async fn a_go_version_recorded_by_go_mod_reports_lockfile() {
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree");
    write(
        &tree,
        &[
            (
                "patchula.pkl",
                "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"go\" }\n",
            ),
            (
                "go.mod",
                "module example.com/app\n\ngo 1.23\n\nrequire github.com/acme/lib v1.2.0\n",
            ),
        ],
    );
    let cassette = Cassette {
        schema_version: 1,
        provenance: "Simulated from the Go module proxy list format".into(),
        interactions: vec![raw(
            "https://proxy.golang.org/github.com/acme/lib/@v/list",
            "text/plain",
            Body::Text("v1.2.0\nv1.2.1\nv1.3.0\n".into()),
        )],
    };
    let report = report(&tree, cassette).await;
    let repo = &report.repositories[0];
    assert_eq!(repo.status, patchula_report::Status::Complete, "{repo:?}");
    let resolution = |name: &str| {
        let dep = repo
            .package_files
            .iter()
            .flat_map(|f| &f.dependencies)
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("no {name} dependency: {repo:?}"));
        dep.metadata
            .as_ref()
            .and_then(|m| m.resolution.as_ref())
            .unwrap_or_else(|| panic!("no resolution recorded for {name}: {dep:?}"))
    };
    let lib = resolution("github.com/acme/lib");
    assert_eq!(lib.kind, patchula_words::ResolutionKind::Lockfile);
    assert_eq!(lib.version, "v1.2.0");
    let directive = resolution("go");
    assert_eq!(directive.kind, patchula_words::ResolutionKind::Declared);
    assert_eq!(directive.version, "1.23");
}

/// A `.terraform.lock.hcl` records where Terraform resolved the provider
/// constraint, so the report says `lockfile`.
#[tokio::test]
async fn a_provider_version_recorded_by_terraform_lock_reports_lockfile() {
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree");
    write(
        &tree,
        &[
            (
                "patchula.pkl",
                "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"terraform\" }\n",
            ),
            (
                "versions.tf",
                "terraform {\n  required_providers {\n    aws = {\n      source  = \"hashicorp/aws\"\n      version = \">= 6.59\"\n    }\n  }\n}\n",
            ),
            (
                ".terraform.lock.hcl",
                "provider \"registry.terraform.io/hashicorp/aws\" {\n  version     = \"6.59.0\"\n  constraints = \">= 6.59\"\n  hashes = [\n    \"h1:abc\",\n  ]\n}\n",
            ),
        ],
    );
    let cassette = Cassette {
        schema_version: 1,
        provenance: "Simulated from the Terraform registry versions format".into(),
        interactions: vec![interaction(
            "https://registry.terraform.io/v1/providers/hashicorp/aws/versions",
            "application/json",
            json!({"versions": [
                {"version": "6.60.0"},
                {"version": "6.59.1"},
                {"version": "6.59.0"}
            ]}),
        )],
    };
    let report = report(&tree, cassette).await;
    let repo = &report.repositories[0];
    assert_eq!(repo.status, patchula_report::Status::Complete, "{repo:?}");
    let dep = &repo.package_files[0].dependencies[0];
    let resolution = dep
        .metadata
        .as_ref()
        .and_then(|m| m.resolution.as_ref())
        .unwrap_or_else(|| panic!("no resolution recorded: {dep:?}"));
    assert_eq!(resolution.kind, patchula_words::ResolutionKind::Lockfile);
    assert_eq!(resolution.version, "6.59.0");
}

/// A `Cargo.lock` records where Cargo resolved the range, so the report says
/// `lockfile` — the same kind as npm. The toolchain pin beside it is declared
/// in a version file, not resolved by the lock, so it stays `declared`.
#[tokio::test]
async fn a_cargo_version_recorded_by_its_lockfile_reports_lockfile() {
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree");
    write(
        &tree,
        &[
            (
                "patchula.pkl",
                "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"cargo\" }\n",
            ),
            (
                "Cargo.toml",
                "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = \"1\"\n",
            ),
            (
                "Cargo.lock",
                "version = 3\n\n[[package]]\nname = \"demo\"\nversion = \"0.1.0\"\ndependencies = [\"serde\"]\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.219\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
            ),
            ("rust-toolchain", "1.75.0\n"),
        ],
    );
    let cassette = Cassette {
        schema_version: 1,
        provenance: "Simulated from the crates.io sparse index format".into(),
        interactions: vec![raw(
            "https://index.crates.io/se/rd/serde",
            "text/plain",
            Body::Text(
                "{\"vers\":\"1.0.219\",\"yanked\":false}\n{\"vers\":\"1.0.220\",\"yanked\":false}\n"
                    .into(),
            ),
        )],
    };
    let report = report(&tree, cassette).await;
    let repo = &report.repositories[0];
    assert_eq!(repo.status, patchula_report::Status::Complete, "{repo:?}");
    let resolution = |name: &str| {
        let dep = repo
            .package_files
            .iter()
            .flat_map(|f| &f.dependencies)
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("no {name} dependency: {repo:?}"));
        dep.metadata
            .as_ref()
            .and_then(|m| m.resolution.as_ref())
            .unwrap_or_else(|| panic!("no resolution recorded for {name}: {dep:?}"))
    };
    let serde = resolution("serde");
    assert_eq!(serde.kind, patchula_words::ResolutionKind::Lockfile);
    assert_eq!(serde.version, "1.0.219");
    let rust = resolution("rust");
    assert_eq!(rust.kind, patchula_words::ResolutionKind::Declared);
    assert_eq!(rust.version, "1.75.0");
}
