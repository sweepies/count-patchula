//! A local checkout's GitLab CI file looked up against gitlab.com and Docker
//! Hub, both read unauthenticated. Recorded by patchula; replay is offline.
use patchula_cache::Cache;
use patchula_engine::{Engine, Options};
use patchula_http::Network;
use std::{collections::BTreeMap, path::Path, sync::Arc};

const CI: &str = r#"include:
  - project: components/sast
    ref: 3.0.0
    file: templates/sast.yml
  - component: $CI_SERVER_FQDN/components/sast/sast@3.0.0
image: busybox:1.36.0
test:
  image: $CI_REGISTRY_IMAGE:latest
release:
  services:
    - name: registry.gitlab.com/gitlab-org/release-cli:v0.18.0
"#;

#[tokio::test]
async fn gitlab_ci_images_pin_to_digests_and_refs_follow_gitlab_tags() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/engine/gitlab-ci.json");
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
    std::fs::write(checkout.join(".gitlab-ci.yml"), CI).unwrap();
    let config = patchula_config::load_files(
        "/repo/patchula.pkl",
        BTreeMap::from([(
            "/repo/patchula.pkl".into(),
            "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"gitlabci\"; \"gitlabci-include\" }\ndefaults { waitHours = 0 }\nsecurity { advisories = false }\n".into(),
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
    assert_eq!(
        skipped,
        [("gitlabci", Some(8), "$CI_REGISTRY_IMAGE:latest")]
    );
    let deps: BTreeMap<_, _> = repo.package_files[0]
        .dependencies
        .iter()
        .map(|d| ((d.manager.as_str(), d.package.as_str()), d))
        .collect();
    let busybox = deps[&("gitlabci", "busybox")];
    let pin = busybox
        .updates
        .iter()
        .find(|u| u.update_type == "pin")
        .expect("busybox is pinned");
    assert_eq!(pin.new_value.as_deref(), Some("1.36.0"));
    assert!(pin.new_digest.as_deref().unwrap().starts_with("sha256:"));
    assert!(busybox.updates.iter().any(|u| u.update_type == "minor"));
    // GitLab's registry issues its anonymous tokens from gitlab.com.
    let release = deps[&("gitlabci", "registry.gitlab.com/gitlab-org/release-cli")];
    assert!(release.updates.iter().all(|u| {
        u.new_digest
            .as_deref()
            .is_some_and(|d| d.starts_with("sha256:"))
    }));
    assert!(release.updates.iter().any(|u| u.update_type == "pin"));
    for manager in ["gitlabci", "gitlabci-include"] {
        let sast = deps[&(manager, "gitlab.com/components/sast")];
        assert!(
            sast.updates.iter().any(|u| u.update_type == "minor"
                && u.new_version.as_deref() == Some("3.5.0")
                && u.new_digest.is_none()),
            "{manager}: {:?}",
            sast.updates
        );
    }
}
