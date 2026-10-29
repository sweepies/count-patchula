//! Multi-repository recording uses only public jdx repositories and registries.
use patchula_cache::Cache;
use patchula_engine::{Engine, Options};
use patchula_http::Network;
use patchula_process::{Executor, SystemExecutor};
use patchula_report::{Report, Status};
use patchula_testkit::{MiseRecorder, MiseReplay, Options as FixtureOptions, Redactor, Replay};
use std::{path::Path, sync::Arc};
fn config() -> patchula_config::Config {
    patchula_config::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/public.pkl")).unwrap()
}
#[tokio::test]
async fn public_multi_repo_report_cold_and_warm() {
    let recording = std::env::var("PATCHULA_RECORD").as_deref() == Ok("1");
    let token = if recording {
        std::env::var("GH_TOKEN")
            .or_else(|_| std::env::var("GITHUB_TOKEN"))
            .expect("recording requires token")
    } else {
        "offline-public".into()
    };
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/engine/public-batch.json");
    let fixture_options = FixtureOptions {
        redactor: Redactor::default().with_literals([token.clone()]),
        ..Default::default()
    };
    let replay = if recording {
        None
    } else {
        Some(Arc::new(
            Replay::open(&path, fixture_options.clone()).unwrap(),
        ))
    };
    let transport = if let Some(replay) = &replay {
        replay.clone() as Arc<dyn patchula_http::Transport>
    } else {
        patchula_testkit::from_env(&path, fixture_options, || Ok(Arc::new(Network::new()?)))
            .unwrap()
    };
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::open(dir.path().join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    // The repositories' mise tools are listed by mise, replayed from what it
    // printed when this was recorded.
    let mise_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/mise/public-batch.json");
    let tools = dir.path().join("cache.sqlite");
    let probe = patchula_update::lockfiles::Runner::new(tools.clone(), &token);
    let executor: Arc<dyn Executor> = if recording {
        Arc::new(MiseRecorder::new(
            &mise_path,
            Arc::new(SystemExecutor),
            probe.mise_lookup().version(),
            "Recorded from the pinned mise with `mise run fixtures:record-engine`: what `mise ls-remote --json` printed for the mise tools of jdx/usage and jdx/mr-boxington",
        ))
    } else {
        Arc::new(MiseReplay::open(&mise_path).unwrap())
    };
    let engine = Engine::with_transport(&token, cache, 4, transport)
        .unwrap()
        .with_lockfile_runner(probe.with_executor(executor))
        .with_versions_host(false);
    let options = || Options {
        repositories: vec!["jdx/usage".into(), "jdx/mr-boxington".into()],
        config: Some(config()),
        ..Default::default()
    };
    let now = chrono::Utc::now();
    let cold = engine.lookup_at(options(), now).await.unwrap();
    for repo in &cold.repositories {
        assert_eq!(repo.status, Status::Complete, "{repo:?}");
        assert!(repo.commit.is_some() && repo.default_branch.is_some());
        assert!(!repo.warnings.iter().any(|w| w.contains("not implemented")));
        assert!(
            repo.package_files
                .iter()
                .any(|f| f.path.ends_with("Cargo.toml"))
        );
        assert!(repo.package_files.iter().any(|f| f.path == "mise.toml"));
        assert!(
            repo.package_files
                .iter()
                .any(|f| f.path.ends_with("package.json"))
        );
    }
    let bytes = serde_json::to_vec(&cold).unwrap();
    Report::from_json(&bytes).unwrap();
    let warm = engine.lookup_at(options(), now).await.unwrap();
    assert_eq!(cold.repositories, warm.repositories);
    let stats = warm.stats.as_ref().unwrap();
    assert_eq!(stats.extraction_hits, 2);
    // Cold discovery uses GraphQL. First warm REST metadata/branch/release
    // reads prime their validators; the tag namespace already revalidates.
    if !recording {
        assert_eq!(
            stats.requests, 6,
            "four conditional repository reads, actions/checkout's release head, one tag namespace; mise lists the repositories' tools"
        );
        assert_eq!(stats.not_modified, 1);
    }
    assert!(stats.lookups > 0);
    assert!(
        stats.lookups
            < cold
                .repositories
                .iter()
                .flat_map(|r| &r.package_files)
                .flat_map(|f| &f.dependencies)
                .count() as u64
    );
    if let Some(replay) = replay {
        replay.assert_finished().unwrap();
    }
    let cassette: patchula_testkit::Cassette =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for repo in ["jdx/usage", "jdx/mr-boxington"] {
        let metadata = cassette
            .interactions
            .iter()
            .find(|i| matches!(&i.request.body, patchula_testkit::Body::Json(value)
                if value["query"] == patchula_engine::REPOSITORY_HEAD_QUERY
                    && format!("{}/{}", value["variables"]["owner"].as_str().unwrap(), value["variables"]["name"].as_str().unwrap()) == repo))
            .expect("public repo metadata");
        let patchula_testkit::Body::Json(value) = &metadata.response.body else {
            panic!("JSON metadata")
        };
        assert_eq!(
            value["data"]["repository"]["isPrivate"], false,
            "only public repo recordings may be committed"
        );
    }
    eprintln!(
        "engine cold={}us warm={}us cold_requests={} warm_requests={} warm_304={} unique_lookups={} extraction_hits={}",
        cold.stats.as_ref().unwrap().phase_micros["total"],
        stats.phase_micros["total"],
        cold.stats.as_ref().unwrap().requests,
        stats.requests,
        stats.not_modified,
        stats.lookups,
        stats.extraction_hits
    );
    eprintln!(
        "engine cold_phases={:?} warm_phases={:?}",
        cold.stats.as_ref().unwrap().phase_micros,
        stats.phase_micros
    );
}
