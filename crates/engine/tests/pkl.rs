//! A local checkout's Pkl package imports looked up in the `pkl` source:
//! hk from its own releases, and two packages of the `apple/pkl-pantry`
//! monorepo from that repository's releases, read once. Recorded by
//! patchula from public repositories; replay is offline.
use patchula_cache::Cache;
use patchula_engine::{Engine, Options};
use patchula_http::Network;
use patchula_testkit::Redactor;
use std::{collections::BTreeMap, path::Path, sync::Arc};

const HK: &str = concat!(
    "amends \"package://github.com/jdx/hk/releases/download/v2.9.0/hk@2.9.0#/Config.pkl\"\n",
    "import \"package://github.com/jdx/hk/releases/download/v2.9.0/hk@2.9.0#/Builtins.pkl\"\n",
);
const PANTRY: &str = concat!(
    "import \"package://pkg.pkl-lang.org/pkl-pantry/pkl.toml@1.0.0#/toml.pkl\"\n",
    "import \"package://pkg.pkl-lang.org/pkl-pantry/k8s.contrib@1.0.0#/convert.pkl\"\n",
);

#[tokio::test]
async fn pkl_packages_follow_their_own_release_tags() {
    let recording = patchula_testkit::recording();
    let token = if recording {
        std::env::var(patchula_testkit::GH_TOKEN_VAR)
            .or_else(|_| std::env::var(patchula_testkit::GITHUB_TOKEN_VAR))
            .expect("recording requires token")
    } else {
        "offline-pkl".into()
    };
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/engine/pkl.json");
    let redactor = Redactor::default().with_literals([token.clone()]);
    let transport =
        patchula_testkit::from_env(&path, redactor, || Ok(Arc::new(Network::new()?))).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::open(dir.path().join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let checkout = dir.path().join("checkout");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::write(checkout.join("hk.pkl"), HK).unwrap();
    std::fs::write(checkout.join("config.pkl"), PANTRY).unwrap();
    let config = patchula_config::load_files(
        "/repo/patchula.pkl",
        BTreeMap::from([(
            "/repo/patchula.pkl".into(),
            "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"pkl\" }\ndefaults { waitHours = 0 }\nsecurity { advisories = false }\n".into(),
        )]),
    )
    .unwrap();
    let engine = Engine::with_transport(&token, cache, 4, transport)
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
    assert!(repo.skipped.is_empty(), "{:?}", repo.skipped);
    assert!(
        repo.lookup_failures.is_empty(),
        "{:?}",
        repo.lookup_failures
    );
    let dependencies: BTreeMap<&str, _> = repo
        .package_files
        .iter()
        .flat_map(|file| &file.dependencies)
        .map(|dep| (dep.name.as_str(), dep))
        .collect();
    assert_eq!(
        dependencies.keys().copied().collect::<Vec<_>>(),
        ["hk", "k8s.contrib", "pkl.toml"]
    );
    assert_eq!(
        dependencies["hk"].package,
        "github.com/jdx/hk/releases/download/v%version%/hk"
    );
    let cassette: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    // Every release tag GitHub answered with, from every repository.
    fn tags(value: &serde_json::Value, out: &mut std::collections::BTreeSet<String>) {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(serde_json::Value::String(tag)) = map.get("tagName") {
                    out.insert(tag.clone());
                }
                map.values().for_each(|v| tags(v, out));
            }
            serde_json::Value::Array(items) => items.iter().for_each(|v| tags(v, out)),
            _ => {}
        }
    }
    let mut released = std::collections::BTreeSet::new();
    tags(&cassette, &mut released);
    for (name, current) in [
        ("hk", "2.9.0"),
        ("pkl.toml", "1.0.0"),
        ("k8s.contrib", "1.0.0"),
    ] {
        let dep = dependencies[name];
        assert_eq!(dep.current.version.as_deref(), Some(current), "{name}");
        assert!(!dep.updates.is_empty(), "{name} has newer releases");
        let stream = patchula_config::PklPackage::parse(&dep.package).unwrap();
        for update in &dep.updates {
            let version = update.new_version.as_deref().unwrap();
            assert_eq!(update.new_value.as_deref(), Some(version), "{name}");
            // Each proposal is a release tagged as this very package, never a
            // monorepo neighbour's version.
            let tag = format!("{}{version}{}", stream.prefix, stream.suffix);
            assert!(released.contains(&tag), "{name}: no release {tag}");
        }
    }
    // `pkl.toml` and `k8s.contrib` are tagged in one repository, whose
    // releases the run walked once for both: one first page names it, and no
    // later page is asked for twice.
    let mut first_pages = 0;
    let mut pages = std::collections::BTreeSet::new();
    for interaction in cassette["interactions"].as_array().unwrap() {
        let variables = interaction["request"]["body"]["value"]["variables"]
            .as_object()
            .unwrap();
        let named = |owner: &str, name: &str| {
            variables.get(owner).and_then(|v| v.as_str()) == Some("apple")
                && variables.get(name).and_then(|v| v.as_str()) == Some("pkl-pantry")
        };
        match variables.get("cursor").filter(|cursor| !cursor.is_null()) {
            Some(cursor) => {
                let page = (
                    variables["owner"].to_string(),
                    variables["name"].to_string(),
                    cursor.to_string(),
                );
                assert!(pages.insert(page), "a release page read twice");
            }
            None if named("owner", "name") => first_pages += 1,
            // A batched first page names each repository as `o<i>`/`n<i>`.
            None => {
                first_pages += variables
                    .keys()
                    .filter_map(|key| key.strip_prefix('o'))
                    .filter(|i| named(&format!("o{i}"), &format!("n{i}")))
                    .count();
            }
        }
    }
    assert_eq!(first_pages, 1, "apple/pkl-pantry's first page read once");
}
