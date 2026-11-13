//! Proposal behaviour for the version files, Bun's npm rules and pre-commit
//! branch revs that the per-manager proposal-facts move must keep.
use patchula_datasource::{Catalog, Version};
use patchula_lookup::Evaluator;
use patchula_manager::{Dependency, EditForm, Location, Syntax};
use patchula_words::{DependencyKind, Manager, Scheme, Source};

fn config() -> patchula_config::Config {
    patchula_config::load(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus"),
    )
    .unwrap()
}
fn dependency(name: &str) -> Dependency {
    Dependency {
        manager: Manager::Npm,
        file: "package.json".into(),
        location: Location {
            start: 0,
            end: 1,
            line: 1,
            column: 1,
            pointer: String::new(),
        },
        form: EditForm::Scalar(Syntax::Json),
        name: name.into(),
        package: name.into(),
        kind: DependencyKind::Production,
        current: "1.0.0".into(),
        resolved: Some("1.0.0".into()),
        digest: None,
        source: Source::Npm,
        scheme: Scheme::Semver,
        extract: None,
        included_by: None,
        registry: None,
        cooldown: None,
    }
}
fn catalog(values: &[&str]) -> Catalog {
    Catalog {
        versions: values
            .iter()
            .map(|v| Version {
                version: (*v).into(),
                published_at: None,
                digest: Some("a".repeat(40)),
                deprecated: false,
                yanked: false,
            })
            .collect(),
        dist_tags: Default::default(),
    }
}

/// Bug 1: a version file such as `.nvmrc` or `.terraform-version` records
/// one bare version — the installed pin — so it is updated like any other
/// pin. Only ranged runtime constraints (`engines.node`,
/// `required_version`) keep the `preserve` default.
#[test]
fn version_files_update_like_pins() {
    let eval = Evaluator::new(&config()).unwrap();
    let mut dep = dependency("node");
    dep.file = ".nvmrc".into();
    dep.form = EditForm::Scalar(Syntax::Bare);
    dep.kind = DependencyKind::Toolchain;
    dep.source = Source::Mise;
    dep.scheme = Scheme::Loose;
    dep.current = "20.1.0".into();
    dep.resolved = Some("20.1.0".into());
    let proposals = eval
        .propose(&dep, &catalog(&["20.1.1", "20.2.0", "21.0.0"]))
        .unwrap();
    for version in ["20.1.1", "20.2.0", "21.0.0"] {
        let update = proposals
            .iter()
            .find(|p| p.new_version.as_deref() == Some(version))
            .unwrap_or_else(|| panic!("a version file updates like a pin: {proposals:?}"));
        assert_eq!(update.new_value.as_deref(), Some(version));
    }

    let mut dep = dependency("terraform");
    dep.manager = Manager::Terraform;
    dep.file = ".terraform-version".into();
    dep.form = EditForm::Scalar(Syntax::Bare);
    dep.kind = DependencyKind::Toolchain;
    dep.source = Source::Mise;
    dep.scheme = Scheme::Loose;
    dep.current = "1.14.7".into();
    dep.resolved = Some("1.14.7".into());
    let proposals = eval
        .propose(&dep, &catalog(&["1.14.8", "1.15.0", "2.0.0"]))
        .unwrap();
    for version in ["1.14.8", "1.15.0", "2.0.0"] {
        let update = proposals
            .iter()
            .find(|p| p.new_version.as_deref() == Some(version))
            .unwrap_or_else(|| panic!("a version file updates like a pin: {proposals:?}"));
        assert_eq!(update.new_value.as_deref(), Some(version));
    }
}

/// Bun shares npm's files, so a bun-enabled repository's toolchain and peer
/// declarations follow npm's rules: a toolchain range keeps the `preserve`
/// default, and a ranged peer is widened rather than rewritten exact.
#[test]
fn bun_follows_npm_toolchain_and_peer_rules() {
    let eval = Evaluator::new(&config()).unwrap();
    let mut toolchain = dependency("node");
    toolchain.manager = Manager::Bun;
    toolchain.kind = DependencyKind::Toolchain;
    toolchain.source = Source::Mise;
    toolchain.current = "^20".into();
    toolchain.resolved = Some("20.1.0".into());
    let proposals = eval
        .propose(&toolchain, &catalog(&["20.1.0", "20.1.1", "21.0.0"]))
        .unwrap();
    assert!(
        proposals.is_empty(),
        "a bun toolchain range is a supported-runtime range: {proposals:?}"
    );
    let mut peer = dependency("react");
    peer.manager = Manager::Bun;
    peer.kind = DependencyKind::Peer;
    peer.current = "^18".into();
    peer.resolved = Some("18.2.0".into());
    let proposals = eval
        .propose(&peer, &catalog(&["18.2.0", "18.3.0", "19.0.0"]))
        .unwrap();
    let update = proposals
        .iter()
        .find(|p| p.new_version.as_deref() == Some("19.0.0"))
        .expect("the peer updates");
    assert_eq!(update.new_value.as_deref(), Some("^18 || ^19.0.0"));
}

/// A pre-commit `rev` that names a branch is an opaque ref, as a GitHub
/// Actions branch ref is: it has no version to compare, so it is never
/// looked up and never errors. Only a version-shaped `rev` is looked up.
#[test]
fn a_precommit_branch_rev_is_an_opaque_ref() {
    let eval = Evaluator::new(&config()).unwrap();
    let mut dep = dependency("pre-commit/pre-commit-hooks");
    dep.manager = Manager::PreCommit;
    dep.file = ".pre-commit-config.yaml".into();
    dep.kind = DependencyKind::Tool;
    dep.current = "main".into();
    dep.resolved = Some("main".into());
    dep.source = Source::GithubTags;
    dep.scheme = Scheme::Loose;
    assert!(
        !eval.needs_lookup(&dep),
        "a branch rev has no version to look up"
    );
    let evaluation = eval
        .evaluate_at(&dep, &catalog(&["v4.5.0", "v5.0.0"]), now())
        .expect("a branch rev is preserved, not an error");
    assert!(
        evaluation.updates.is_empty(),
        "a branch rev gets no proposals: {:?}",
        evaluation.updates
    );
    dep.current = "v4.5.0".into();
    dep.resolved = Some("v4.5.0".into());
    assert!(eval.needs_lookup(&dep), "a tag rev is looked up");
}

fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc)
}
