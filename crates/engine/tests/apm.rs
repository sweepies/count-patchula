//! A local checkout's APM manifest looked up as forge tags on gitlab.com, read
//! unauthenticated. Recorded by patchula; replay is offline.
use patchula_cache::Cache;
use patchula_engine::{Engine, Options};
use patchula_http::Network;
use std::{collections::BTreeMap, path::Path, sync::Arc};

const MANIFEST: &str = "name: demo\nversion: 1.0.0\ndependencies:\n  apm:\n    - gitlab.com/components/sast#3.0.0\n    - owner/branch#main\n";

#[tokio::test]
async fn apm_tags_pin_to_commits_and_update_to_exact_tags() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/engine/apm.json");
    let transport = patchula_testkit::from_env(&path, patchula_testkit::Options::default(), || {
        Ok(Arc::new(Network::new()?))
    })
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::open(
        dir.path().join("cache.sqlite"),
        patchula_cache::Options::default(),
    )
    .await
    .unwrap();
    let checkout = dir.path().join("checkout");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::write(checkout.join("apm.yml"), MANIFEST).unwrap();
    let config = patchula_config::load_files(
        "/repo/patchula.pkl",
        BTreeMap::from([(
            "/repo/patchula.pkl".into(),
            "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"apm\" }\ndefaults { waitHours = 0 }\nsecurity { advisories = false }\n".into(),
        )]),
    )
    .unwrap();
    let engine = Engine::with_transport("offline", cache, 4, transport)
        .unwrap()
        .with_versions_host(false);
    let report = engine
        .lookup(Options {
            config: Some(config),
            local: Some(checkout),
            ..Default::default()
        })
        .await
        .unwrap();
    let repo = &report.repositories[0];
    assert!(repo.warnings.is_empty(), "{:?}", repo.warnings);
    let skipped: Vec<_> = repo
        .skipped
        .iter()
        .map(|s| (s.manager.as_str(), s.line, s.value.as_str()))
        .collect();
    assert_eq!(skipped, [("apm", Some(6), "owner/branch#main")]);
    let sast = &repo.package_files[0].dependencies[0];
    assert_eq!(sast.package, "gitlab.com/components/sast");
    let commit = |d: Option<&str>| {
        d.is_some_and(|d| d.len() == 40 && d.bytes().all(|b| b.is_ascii_hexdigit()))
    };
    let pin = sast
        .updates
        .iter()
        .find(|u| u.update_type == "pin")
        .expect("the tag is pinned to its commit");
    assert_eq!(pin.new_value.as_deref(), Some("3.0.0"));
    assert!(commit(pin.new_digest.as_deref()), "{pin:?}");
    let minor = sast
        .updates
        .iter()
        .find(|u| u.update_type == "minor")
        .expect("a newer tag");
    assert_eq!(minor.new_value, minor.new_version);
    assert!(commit(minor.new_digest.as_deref()), "{minor:?}");
}
