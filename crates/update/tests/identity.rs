//! The persisted key recipes: declaration, retry and held keys reach GitHub
//! and caches, so their bytes are pinned.
use patchula_manager::{Dependency as ManagerDependency, EditForm, Location, Syntax};
use patchula_report::{Current, Dependency as ReportDependency, DependencyMetadata};
use patchula_update::{Change, identity};
use patchula_words::{DependencyKind, Manager, Scheme, Source, Update as UpdateKind};

fn manager_dep() -> ManagerDependency {
    ManagerDependency {
        manager: Manager::Npm,
        file: "package.json".into(),
        location: Location {
            start: 1,
            end: 9,
            line: 2,
            column: 3,
            pointer: "/dependencies/fixture".into(),
        },
        form: EditForm::Scalar(Syntax::Json),
        name: "fixture".into(),
        package: "fixture".into(),
        kind: DependencyKind::Production,
        current: "^2.0.0".into(),
        resolved: Some("2.1.0".into()),
        digest: Some(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        ),
        source: Source::Npm,
        scheme: Scheme::Semver,
        extract: None,
        included_by: None,
        registry: Some("https://npm.corp.example.com".into()),
    }
}

fn report_dep() -> ReportDependency {
    ReportDependency {
        manager: Manager::Npm,
        name: "fixture".into(),
        occurrence: None,
        package: "fixture".into(),
        registry: Some("https://npm.corp.example.com".into()),
        current: Current {
            value: Some("^2.0.0".into()),
            version: Some("2.1.0".into()),
            digest: Some(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            ),
        },
        updates: vec![],
        metadata: Some(DependencyMetadata {
            tag_digests: Default::default(),
            source: Source::Npm,
            kinds: vec![],
            locations: vec![],
            held_back: vec![],
            stale: Default::default(),
            lookup: None,
            resolution: None,
            advisories: vec![],
            image: None,
            credentialed: false,
        }),
    }
}

#[test]
fn declaration_keys_are_pinned() {
    // The extracted declaration key.
    assert_eq!(
        identity::extracted_key(&manager_dep()),
        "63a8aa75b4cc2953dd4f164828b593805d2f64027462bdd9992114a82e933848"
    );
    // A report dependency's key agrees with the extracted one.
    assert_eq!(
        identity::dependency_key("package.json", &report_dep()),
        "63a8aa75b4cc2953dd4f164828b593805d2f64027462bdd9992114a82e933848"
    );
    // Without metadata or registry the source serializes as "" and the
    // registry is absent, as the recipe always wrote it.
    let mut bare = report_dep();
    bare.metadata = None;
    bare.registry = None;
    bare.current.digest = None;
    assert_eq!(
        identity::dependency_key("package.json", &bare),
        "64bfbf366d3203fb54f4d58e01fac573cd41ed9305e24b60b4bccaaadd9a30bb"
    );
    let mut nulls = report_dep();
    nulls.registry = None;
    nulls.current.value = None;
    nulls.current.digest = None;
    if let Some(m) = &mut nulls.metadata {
        m.source = Source::Docker;
    }
    assert_eq!(
        identity::dependency_key("docker/Dockerfile", &nulls),
        "8bbfa97ceba0da6194dc72904d7c7610d8536c9b5be0d376d11a74a949570ac6"
    );
}

#[test]
fn retry_and_held_keys_are_pinned() {
    assert_eq!(
        identity::extracted_retry_key(&manager_dep()),
        "90eec713fdec474f9685dff45cf737fff80ce0b91d4b0a087b152c10127b5a47"
    );
    assert_eq!(
        identity::retry_key("npm", "fixture", "npm"),
        "90eec713fdec474f9685dff45cf737fff80ce0b91d4b0a087b152c10127b5a47"
    );
    assert_eq!(
        identity::held_key("package.json", &report_dep(), "3.0.0", "major"),
        "b643054ae12cf7c521c98883810258c7406e5d4c1d4bdcc09d94cd7ac5e48d1e"
    );
}

fn change(registry: Option<&str>) -> Change {
    Change {
        source: Source::Npm,
        package: "fixture".into(),
        registry: registry.map(patchula_config::Registry::from_recorded),
        file: "package.json".into(),
        current: "^2.0.0".into(),
        current_version: None,
        value: Some("^3.0.0".into()),
        version: Some("3.0.0".into()),
        digest: None,
        kind: UpdateKind::Major,
        merge: false,
        labels: vec![],
        release_notes: true,
        body_notes: vec![],
        advisories: vec![],
        image: None,
    }
}

#[test]
fn touches_separates_registries() {
    let a = change(Some("https://npm.corp.example.com"));
    // Same file, source, package and registry: the same declaration.
    assert!(a.touches(
        "package.json",
        Source::Npm,
        "fixture",
        Some("https://npm.corp.example.com")
    ));
    // A same-named package from another registry is a different declaration.
    assert!(!a.touches(
        "package.json",
        Source::Npm,
        "fixture",
        Some("https://npm.other.example.com")
    ));
    assert!(!a.touches("package.json", Source::Npm, "fixture", None));
    let public = change(None);
    assert!(!public.touches(
        "package.json",
        Source::Npm,
        "fixture",
        Some("https://npm.corp.example.com")
    ));
    // The declaration keys differ the same way.
    let mut other = manager_dep();
    other.registry = Some("https://npm.other.example.com".into());
    assert_ne!(manager_dep().declaration().key(), other.declaration().key());
}
