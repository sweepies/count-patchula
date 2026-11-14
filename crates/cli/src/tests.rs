//! CLI dispatch and persistent cache regression for installation-token receipts.
use super::{App, Services, run};
use patchula_cache::Cache;
use patchula_testkit::github::{self, FakeGitHub, Hosts};
use std::ffi::OsStr;

use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

/// A `demo/sandbox` GitHub holding the files every run starts with, behind
/// a router scripting the npm registry and OSV. Three pull requests exist:
/// `owner-closed-a` and `owner-closed-b` are closed, the third is an open
/// PR from a foreign head.
fn github() -> (Arc<FakeGitHub>, Arc<Hosts>) {
    let github = Arc::new(FakeGitHub::new("demo/sandbox"));
    let hosts = Arc::new(Hosts::new(github.clone()));
    hosts.script("api.osv.dev", |_, body| {
        let queries = body["queries"].as_array().map_or(0, Vec::len);
        github::json(200, json!({"results": vec![json!({}); queries]}))
    });
    hosts.script("registry.npmjs.org", |_, _| {
        github::json(
            200,
            json!({"dist-tags":{"latest":"2.0.0"},"versions":{"1.0.0":{},"2.0.0":{}}}),
        )
    });
    github.seed_files(
        "main",
        "base",
        &BTreeMap::from([
            ("patchula.pkl".into(),"amends \"PatchulaConfig.pkl\"\nprs { dashboard = true }\ndefaults { waitHours = 0 }\nmanagers = new Listing { \"npm\" }\n".into()),
            ("package.json".into(),"{\n  \"dependencies\": {\"fixture\": \"1.0.0\", \"owner-closed-a\": \"1.0.0\", \"owner-closed-b\": \"1.0.0\"}\n}\n".into()),
        ]),
        vec![],
    );
    {
        let mut state = github.state();
        let repo = state.repo_mut();
        repo.merge.auto_merge = true;
        repo.merge.rebase = false;
        repo.commits.get_mut("base").unwrap()["message"] = json!("base");
        repo.blobs.insert("unselected-lock".into(), "{}".into());
        repo.refs
            .insert("refs/heads/someone/feature".into(), "old-commit".into());
        repo.commits.insert(
            "old-commit".into(),
            json!({"sha":"old-commit","tree":{"sha":"foreign-tree"},"actor":"someone"}),
        );
        let mut foreign = existing_pr("foreign", 3, false);
        foreign["head"]["ref"] = json!("someone/feature");
        foreign["user"]["login"] = json!("someone");
        foreign["body"] = json!("unrelated private PR text");
        repo.prs = BTreeMap::from([
            (1, existing_pr("owner-closed-a", 1, true)),
            (2, existing_pr("owner-closed-b", 2, true)),
            (3, foreign),
        ]);
    }
    (github, hosts)
}

fn existing_pr(package: &str, number: u64, closed: bool) -> Value {
    use sha2::{Digest, Sha256};
    // Same branch and target tuple encoded by update_key; these historical PRs
    // existed before this process/cache. The identity is deliberately unrelated.
    let targets = json!([
        format!("patchula/{package}-2.x"),
        [["npm", package, "2.0.0", "2.0.0", null]]
    ]);
    let update = Sha256::digest(serde_json::to_vec(&targets).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let marker = json!({"version":1,"identity":"0".repeat(64),"update":update});
    json!({
        "number":number,"node_id":format!("closed-{number}"),"html_url":format!("https://github.com/demo/sandbox/pull/{number}"),
        "state":if closed {"closed"} else {"open"},"merged_at":null,
        "body":format!("<!-- patchula:{marker} -->"),
        "title":"Existing PR", "user":{"login":"patchula[bot]"},
        "head":{"ref":format!("patchula/{package}-2.x"),"sha":"old-commit","repo":{"full_name":"demo/sandbox"}},
        "base":{"ref":"main","repo":{"full_name":"demo/sandbox"}},
        "created_at":"2026-01-01T00:00:00Z",
        "labels":[],"auto_merge":null
    })
}

#[derive(Clone)]
struct LogWriter(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn cli_run_reopens_disk_receipts_with_bot_authors_and_mixed_pr_inventory() {
    use tracing_subscriber::{filter::Targets, prelude::*};
    let logs = LogWriter(Default::default());
    let writer = logs.clone();
    // A global subscriber, not a scoped default: other tests exercise the same
    // write path in parallel, and a callsite first registered on a thread with
    // no dispatcher caches Interest::never — its events never reach a scoped
    // subscriber afterwards.
    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(move || writer.clone())
                .with_filter(Targets::new().with_target("patchula", tracing::Level::DEBUG)),
        )
        .try_init();
    let (github, hosts) = github();
    let directory = tempfile::tempdir().unwrap();
    let cache_path = directory.path().join("nested/cache");
    let args: Vec<_> = [
        OsStr::new("run"),
        OsStr::new("-vv"),
        OsStr::new("--json"),
        OsStr::new("--cache"),
        cache_path.as_os_str(),
        OsStr::new("--recheck"),
        OsStr::new("next-run"),
        OsStr::new("demo/sandbox"),
    ]
    .into();
    let app = App::parse_from(&args).unwrap();
    assert_eq!(app.verbose, 2);
    for second in [false, true] {
        hosts.clear();
        {
            let mut state = github.state();
            state.requests.clear();
            state.writes.clear();
        }
        // Exactly the main CLI dispatch: only network I/O and token lookup are
        // substituted. batch opens/closes a fresh disk cache and engine each run.
        let services = Services {
            token: Some("offline-installation".into()),
            transport: Some(hosts.clone()),
            ..Services::default()
        };
        assert_eq!(run(&app, &services).await.unwrap(), 0);
        let cache = Cache::open_dir(&cache_path, Default::default())
            .await
            .unwrap();
        let receipts = patchula_update::stored_receipts(&cache, "demo/sandbox", "patchula/")
            .await
            .unwrap()
            .expect("CLI must persist receipts before returning");
        assert_eq!(receipts["writes"].as_object().unwrap().len(), 1);
        assert_eq!(receipts["closed"].as_object().unwrap().len(), 2);
        drop(cache);
        if second {
            let requests = hosts.log();
            let inventories = requests
                .iter()
                .filter(|(_, _, b)| b["query"] == patchula_update::INVENTORY_QUERY)
                .count();
            assert_eq!(
                inventories, 1,
                "the read beside the scan is kept, not repeated"
            );
            assert!(
                requests.iter().all(|(method, path, body)| method == "GET"
                    || body["query"] == patchula_update::INVENTORY_QUERY
                    || path == "/v1/querybatch"),
                "only the inventory and advisory queries may use POST: {requests:?}"
            );
            assert!(github.writes().is_empty());
        }
    }
    let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("reason=\"missing-receipt\""), "{logs}");
    assert!(
        logs.contains("write receipts saved") && logs.contains("count=3 writes=1 closed=2"),
        "{logs}"
    );
    assert!(logs.contains("restored=3 complete=true"), "{logs}");
    assert!(!logs.contains("offline-installation"));
    assert!(!logs.contains("unrelated private PR text"));
    let snapshot = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/transcripts.tsv");
    patchula_testkit::transcripts::check(&snapshot);
}

/// A repository that changed under the run exits 5 — an error would be 1 —
/// and once the change settles the same run exits 0.
#[tokio::test(start_paused = true)]
async fn a_raced_run_exits_five_and_completes_when_it_settles() {
    let (github, hosts) = github();
    let directory = tempfile::tempdir().unwrap();
    let cache_path = directory.path().join("cache");
    github
        .on(patchula_testkit::github::Matcher::Route {
            method: reqwest::Method::GET,
            path: "/repos/demo/sandbox/pulls".into(),
        })
        .times(1)
        .mutate(|state, _| state.repo_mut().move_base("moved"));
    let args: Vec<_> = [
        OsStr::new("run"),
        OsStr::new("--json"),
        OsStr::new("--cache"),
        cache_path.as_os_str(),
        OsStr::new("demo/sandbox"),
    ]
    .into();
    let app = App::parse_from(&args).unwrap();
    let services = Services {
        token: Some("offline-installation".into()),
        transport: Some(hosts.clone()),
        ..Services::default()
    };
    assert_eq!(run(&app, &services).await.unwrap(), 5);
    // The base stayed moved and consistent: the rerun writes the update.
    assert_eq!(run(&app, &services).await.unwrap(), 0);
    let snapshot = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/transcripts.tsv");
    patchula_testkit::transcripts::check(&snapshot);
}

#[test]
fn github_remotes_name_their_repository() {
    use super::github_repository;
    for url in [
        "https://github.com/sweepies/count-patchula",
        "https://github.com/sweepies/count-patchula.git",
        "https://github.com/sweepies/count-patchula/",
        "https://x-access-token:secret@github.com/sweepies/count-patchula.git", // betterleaks:allow
        "http://GitHub.com/sweepies/count-patchula",
        "git@github.com:sweepies/count-patchula.git",
        "github.com:sweepies/count-patchula",
        "ssh://git@github.com/sweepies/count-patchula.git",
        "ssh://git@ssh.github.com:443/sweepies/count-patchula.git",
        "git://github.com/sweepies/count-patchula",
    ] {
        assert_eq!(
            github_repository(url).as_deref(),
            Some("sweepies/count-patchula"),
            "{url}"
        );
    }
    for url in [
        "https://gitlab.com/sweepies/count-patchula.git",
        "git@example.com:sweepies/count-patchula.git",
        "https://github.com/sweepies",
        "https://github.com/sweepies/count-patchula/tree/main",
        "file:///srv/git/count-patchula.git",
        "/srv/git/count-patchula.git",
        "",
    ] {
        assert_eq!(github_repository(url), None, "{url}");
    }
}

/// How the `gh` stand-in fails.
#[derive(Clone, Copy, Debug)]
enum GhFailure {
    Missing,
    SignedOut,
    Hung,
}
/// Stands in for `gh`: records each command and answers with `result`.
struct Gh {
    result: Result<&'static str, GhFailure>,
    calls: Mutex<Vec<(String, Vec<String>, std::time::Duration)>>,
}
impl Gh {
    fn answering(result: Result<&'static str, GhFailure>) -> Self {
        Self {
            result,
            calls: Mutex::default(),
        }
    }
}
impl patchula_process::Executor for Gh {
    fn run(
        &self,
        command: patchula_process::CommandSpec,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<u8>, patchula_process::Failure>>
                + Send
                + '_,
        >,
    > {
        self.calls.lock().unwrap().push((
            command.program.display().to_string(),
            command.args,
            command.timeout,
        ));
        let result = self
            .result
            .map(|out| out.as_bytes().to_vec())
            .map_err(|kind| match kind {
                GhFailure::Missing => patchula_process::Failure::Spawn {
                    program: "gh".into(),
                    message: "No such file or directory".into(),
                },
                GhFailure::SignedOut => patchula_process::Failure::Exited {
                    status: "exit status: 1".into(),
                    stdout: String::new(),
                    stderr: "no oauth token found for github.com".into(),
                },
                GhFailure::Hung => patchula_process::Failure::TimedOut {
                    limit: command.timeout,
                },
            });
        Box::pin(async move { result })
    }
}

#[tokio::test]
async fn token_variables_win_in_order_and_skip_gh() {
    let gh = Gh::answering(Ok("gho_cli\n"));
    let env = |set: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            set.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    };
    let all = &[
        ("PATCHULA_TOKEN", "p"),
        ("GITHUB_TOKEN", "g"),
        ("GH_TOKEN", "h"),
    ];
    assert_eq!(
        super::resolve_token(env(all), &gh).await.as_deref(),
        Some("p")
    );
    let later = &[
        ("PATCHULA_TOKEN", ""),
        ("GITHUB_TOKEN", "g"),
        ("GH_TOKEN", "h"),
    ];
    assert_eq!(
        super::resolve_token(env(later), &gh).await.as_deref(),
        Some("g")
    );
    assert_eq!(
        super::resolve_token(env(&[("GH_TOKEN", "h")]), &gh)
            .await
            .as_deref(),
        Some("h")
    );
    assert!(gh.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn without_variables_the_github_cli_token_for_github_com_is_used() {
    let gh = Gh::answering(Ok("gho_cli\n"));
    let token = super::resolve_token(|_| Some(String::new()), &gh).await;
    assert_eq!(token.as_deref(), Some("gho_cli"));
    let calls = gh.calls.lock().unwrap();
    let (program, args, timeout) = &calls[0];
    assert_eq!(program, "gh");
    assert_eq!(args, &["auth", "token", "--hostname", "github.com"]);
    assert!(*timeout <= std::time::Duration::from_secs(10));
}

#[tokio::test]
async fn a_missing_signed_out_or_hung_github_cli_gives_no_token() {
    for result in [
        Err(GhFailure::Missing),
        Err(GhFailure::SignedOut),
        Err(GhFailure::Hung),
        Ok("  \n"),
    ] {
        let gh = Gh::answering(result);
        assert_eq!(
            super::resolve_token(|_| None, &gh).await,
            None,
            "{result:?}"
        );
        assert_eq!(gh.calls.lock().unwrap().len(), 1);
    }
}

#[test]
fn a_working_tree_without_config_asks_once_and_never_without_a_terminal() {
    use super::{Missing, missing_config};
    let pressing = |key: Option<char>| Services {
        key,
        ..Default::default()
    };
    let defaults = serde_json::to_value(patchula_config::defaults().unwrap()).unwrap();
    let as_json = |config: Option<patchula_config::Config>| {
        serde_json::to_value(config.expect("a config for this run")).unwrap()
    };

    // d and Enter use the defaults for this run and write nothing.
    for key in ['d', 'D', '\n'] {
        let dir = tempfile::tempdir().unwrap();
        let config = missing_config(
            dir.path(),
            false,
            &pressing(Some(key)),
            &super::output::Style::PLAIN,
        )
        .unwrap();
        assert_eq!(as_json(config), defaults, "{key:?}");
        assert!(!dir.path().join("patchula.pkl").exists());
    }

    // w writes the starter file and leaves the engine to read it.
    let dir = tempfile::tempdir().unwrap();
    assert!(
        missing_config(
            dir.path(),
            false,
            &pressing(Some('w')),
            &super::output::Style::PLAIN
        )
        .unwrap()
        .is_none()
    );
    let written = std::fs::read_to_string(dir.path().join("patchula.pkl")).unwrap();
    assert_eq!(written, patchula_config::STARTER);
    // With the file there, nobody is asked again.
    assert!(
        missing_config(
            dir.path(),
            false,
            &pressing(Some('q')),
            &super::output::Style::PLAIN
        )
        .unwrap()
        .is_none()
    );

    // q quits without writing.
    let dir = tempfile::tempdir().unwrap();
    let (code, error) = missing_config(
        dir.path(),
        false,
        &pressing(Some('q')),
        &super::output::Style::PLAIN,
    )
    .unwrap_err();
    assert_eq!(code, 2);
    assert!(
        error.to_string().contains("the Count stays outside"),
        "{error}"
    );
    assert!(!dir.path().join("patchula.pkl").exists());

    // Without a terminal it never asks, and says how to go on.
    let (code, error) = missing_config(
        dir.path(),
        false,
        &pressing(None),
        &super::output::Style::PLAIN,
    )
    .unwrap_err();
    assert_eq!(code, 2);
    assert!(error.to_string().contains("has no patchula.pkl"), "{error}");
    let help = error.help().unwrap().to_string();
    assert!(
        help.contains("--default-config") && help.contains("amends \"PatchulaConfig.pkl\""),
        "{help}"
    );

    // --default-config answers for it, at a terminal or not.
    for services in [pressing(None), pressing(Some('q'))] {
        let config =
            missing_config(dir.path(), true, &services, &super::output::Style::PLAIN).unwrap();
        assert_eq!(as_json(config), defaults);
    }
    assert!(!dir.path().join("patchula.pkl").exists());

    assert_eq!(Missing::from_key('W'), Some(Missing::Write));
    assert_eq!(Missing::from_key('n'), Some(Missing::Quit));
    assert_eq!(Missing::from_key('x'), None);
}

/// A working tree with a Dockerfile image to pin and npm packages to update.
fn apply_tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, content) in [
        (
            "patchula.pkl",
            "amends \"PatchulaConfig.pkl\"\ndefaults { waitHours = 0 }\n",
        ),
        ("Dockerfile", "FROM alpine:3.19.0\n"),
        (
            "package.json",
            "{\n  \"dependencies\": {\"is-number\": \"6.0.0\", \"ms\": \"2.1.2\"}\n}\n",
        ),
    ] {
        std::fs::write(dir.path().join(path), content).unwrap();
    }
    dir
}

#[tokio::test]
async fn apply_writes_the_working_tree_and_exits_by_what_is_left() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/cli/apply.json");
    let transport = patchula_testkit::from_env(&path, Default::default(), || {
        Ok(Arc::new(patchula_http::Network::new()?))
    })
    .unwrap();
    let tree = apply_tree();
    let cache = tempfile::tempdir().unwrap();
    let cache = cache.path().join("cache");
    let services = Services {
        token: Some("offline".into()),
        transport: Some(transport),
        dir: Some(tree.path().to_owned()),
        ..Services::default()
    };
    let apply = async |args: &[&str]| {
        let mut words = vec![
            OsStr::new("apply"),
            OsStr::new("--cache"),
            cache.as_os_str(),
        ];
        words.extend(args.iter().map(OsStr::new));
        let app = App::parse_from(&words).unwrap();
        let Some(super::Command::Apply(options)) = &app.command else {
            panic!("apply")
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = super::apply::command(
            &app,
            options,
            &services,
            &mut out,
            &mut err,
            &super::output::Style::PLAIN,
        )
        .await
        .unwrap();
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    };
    let read = |file: &str| std::fs::read_to_string(tree.path().join(file)).unwrap();
    let dockerfile = read("Dockerfile");
    let manifest = read("package.json");
    let pinned = "FROM alpine:3.19.0@sha256:51b67269f354137895d43f3b3d810bfacd3945438e94dc5ac55fdac340352f48\n";
    let updated = "FROM alpine:3.24.3@sha256:be2656061b536c1883c4034790e21d0a09c5aaaf51968b89368a3c0371ea2c9a\n";

    // Checks and dry runs write nothing and exit 3 while updates wait.
    let (code, out, err) = apply(&["--check"]).await;
    assert_eq!(code, 3, "{err}");
    assert!(out.is_empty());
    assert!(err.contains("3 updates await in 2 files"), "{err}");
    assert!(
        err.contains("  minor  alpine     3.19.0 → 3.24.3@be2656061b53  Dockerfile\n"),
        "{err}"
    );
    let (code, out, err) = apply(&["--dry-run"]).await;
    assert_eq!(code, 3, "{err}");
    assert!(
        out.contains("--- a/Dockerfile\n+++ b/Dockerfile\n"),
        "{out}"
    );
    assert!(
        out.contains(&format!("-FROM alpine:3.19.0\n+{updated}")),
        "{out}"
    );
    assert!(
        out.contains("+  \"dependencies\": {\"is-number\": \"7.0.0\", \"ms\": \"2.1.3\"}"),
        "{out}"
    );
    assert!(err.contains("would apply 3 updates to 2 files"), "{err}");
    assert_eq!(
        (read("Dockerfile"), read("package.json")),
        (dockerfile, manifest.clone())
    );

    // Filters choose a subset; a digest pin is just another type.
    let (code, out, err) = apply(&["--manager", "dockerfile", "--type", "pin"]).await;
    assert_eq!(code, 0, "{err}");
    assert!(out.is_empty());
    assert!(err.contains("applied 1 update to 1 file"), "{err}");
    assert!(err.contains("4 more updates stayed out of it"), "{err}");
    assert_eq!(read("Dockerfile"), pinned);
    assert_eq!(read("package.json"), manifest);
    let (code, out, _) = apply(&["--dependency", "glob:m*", "--json"]).await;
    assert_eq!(code, 0);
    let result: Value = serde_json::from_str(&out).unwrap();
    assert!(result["written"].as_bool().unwrap());
    assert_eq!(result["updates"].as_array().unwrap().len(), 1);
    assert_eq!(result["updates"][0]["name"], "ms");
    assert_eq!(result["files"][0]["path"], "package.json");
    assert_eq!(result["exitCode"], 0);
    assert_eq!(read("package.json"), manifest.replace("2.1.2", "2.1.3"));

    // The largest update for each declaration is applied; the patch update
    // that loses its declaration is reported as skipped, as the TUI reports
    // it, so something is still waiting.
    let (code, _, err) = apply(&[]).await;
    assert_eq!(code, 3, "{err}");
    assert!(err.contains("applied 2 updates to 2 files"), "{err}");
    assert!(err.contains("already takes minor 3.24.3"), "{err}");
    assert_eq!(read("Dockerfile"), updated);
    assert_eq!(
        read("package.json"),
        manifest.replace("2.1.2", "2.1.3").replace("6.0.0", "7.0.0")
    );
    let (code, _, err) = apply(&["--check"]).await;
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("nothing to apply"), "{err}");
}

#[tokio::test]
async fn failed_lookups_fail_apply_and_lookup_with_their_own_code() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/cli/apply-lookup-failure.json");
    let transport = patchula_testkit::from_env(&path, Default::default(), || {
        Ok(Arc::new(patchula_http::Network::new()?))
    })
    .unwrap();
    let tree = tempfile::tempdir().unwrap();
    for (path, content) in [
        (
            "patchula.pkl",
            "amends \"PatchulaConfig.pkl\"\ndefaults { waitHours = 0 }\n",
        ),
        (
            "package.json",
            "{\n  \"dependencies\": {\"is-number\": \"6.0.0\", \"ms\": \"2.1.2\"}\n}\n",
        ),
        (
            ".woodpecker.yaml",
            "steps:\n  build:\n    image: ${IMAGE}\n",
        ),
    ] {
        std::fs::write(tree.path().join(path), content).unwrap();
    }
    let cache = tempfile::tempdir().unwrap();
    let cache = cache.path().join("cache");
    let services = Services {
        token: Some("offline".into()),
        transport: Some(transport),
        dir: Some(tree.path().to_owned()),
        ..Services::default()
    };
    let apply = async |args: &[&str]| {
        let mut words = vec![
            OsStr::new("apply"),
            OsStr::new("--cache"),
            cache.as_os_str(),
        ];
        words.extend(args.iter().map(OsStr::new));
        let app = App::parse_from(&words).unwrap();
        let Some(super::Command::Apply(options)) = &app.command else {
            panic!("apply")
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = super::apply::command(
            &app,
            options,
            &services,
            &mut out,
            &mut err,
            &super::output::Style::PLAIN,
        )
        .await
        .unwrap();
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    };
    let manifest = std::fs::read_to_string(tree.path().join("package.json")).unwrap();

    // A failed lookup outranks pending updates, and the failure is typed.
    let (code, out, err) = apply(&["--check", "--json"]).await;
    assert_eq!(code, 4, "{err}");
    let result: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(result["exitCode"], 4);
    assert_eq!(result["updates"][0]["name"], "ms");
    assert_eq!(
        result["lookupFailures"],
        serde_json::json!([{
            "manager": "npm",
            "path": "package.json",
            "name": "is-number",
            "package": "is-number",
            "datasource": "npm",
            "kind": "network",
            "message": "registry HTTP 503",
        }])
    );
    assert!(
        !result["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("is-number")),
        "{result}"
    );
    let (code, _, err) = apply(&["--check"]).await;
    assert_eq!(code, 4, "{err}");
    assert!(
        err.contains("Lookup failures (1)\n  is-number  package.json  npm, network\n"),
        "{err}"
    );
    assert!(err.contains("1 lookup failed"), "{err}");

    // A plain apply still writes what it could look up.
    let (code, _, err) = apply(&[]).await;
    assert_eq!(code, 4, "{err}");
    assert!(err.contains("applied 1 update to 1 file"), "{err}");
    assert_eq!(
        std::fs::read_to_string(tree.path().join("package.json")).unwrap(),
        manifest.replace("2.1.2", "2.1.3")
    );

    // lookup exits with the same code, and the skipped image is no failure.
    let app = App::parse_from(&[
        OsStr::new("lookup"),
        OsStr::new("--cache"),
        cache.as_os_str(),
    ])
    .unwrap();
    let (report, _) = crate::report(
        &app,
        match &app.command {
            Some(super::Command::Lookup(options)) => options,
            _ => panic!("lookup"),
        },
        None,
        &services,
    )
    .await
    .unwrap();
    let repo = &report.repositories[0];
    assert_eq!(repo.skipped.len(), 1, "{repo:?}");
    assert_eq!(repo.lookup_failures.len(), 1, "{repo:?}");
    assert_eq!(repo.lookup_failures[0].name, "is-number");
    assert_eq!(run(&app, &services).await.unwrap(), 4);
}

#[tokio::test]
async fn apply_is_always_local_and_validates_its_filters() {
    let tree = apply_tree();
    let services = Services {
        dir: Some(tree.path().to_owned()),
        ..Services::default()
    };
    for (args, message) in [
        (&["acme/app"][..], "takes no repository"),
        (&["--all-repos"], "takes no --all-repos"),
        (&["--remote"], "drop --remote"),
        (
            &["--manager", "npn"],
            "--manager npn is not a known manager",
        ),
        (&["--dependency", "regex:("], "--dependency"),
    ] {
        let mut words = vec!["apply"];
        words.extend(args);
        let app = App::parse_from(&words.iter().map(OsStr::new).collect::<Vec<_>>()).unwrap();
        let (code, error) = run(&app, &services).await.unwrap_err();
        assert_eq!(code, 2, "{args:?}");
        assert!(format!("{error}").contains(message), "{args:?}: {error}");
    }
    for args in [
        &["apply", "--here"][..],
        &["apply", "--check", "--dry-run"],
        &["apply", "--type", "huge"],
    ] {
        assert!(
            App::parse_from(&args.iter().map(OsStr::new).collect::<Vec<_>>()).is_err(),
            "{args:?}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(tree.path().join("Dockerfile")).unwrap(),
        "FROM alpine:3.19.0\n"
    );
}

#[test]
fn remote_flags_cover_every_local_only_policy() {
    use crate::cli::{Command, REMOTE_POLICY, RemotePolicy};
    let words = ["lookup", "--remote", "demo/app"];
    let app = App::parse_from(&words.iter().map(OsStr::new).collect::<Vec<_>>()).unwrap();
    let Some(Command::Lookup(options)) = &app.command else {
        panic!("lookup")
    };
    let names: Vec<_> = super::remote_flags(&app, options, None)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    for (flag, policy) in REMOTE_POLICY {
        assert_eq!(
            names.contains(flag),
            *policy == RemotePolicy::Local,
            "--{flag}: remote_flags and REMOTE_POLICY disagree"
        );
    }
}

#[tokio::test]
async fn apply_without_a_config_asks_or_takes_default_config() {
    let tree = apply_tree();
    std::fs::remove_file(tree.path().join("patchula.pkl")).unwrap();
    let apply = |args: &[&str]| {
        let mut words = vec!["apply"];
        words.extend(args);
        App::parse_from(&words.iter().map(OsStr::new).collect::<Vec<_>>()).unwrap()
    };
    // Without a terminal it never asks, and names the flag.
    let services = Services {
        dir: Some(tree.path().to_owned()),
        ..Services::default()
    };
    let (code, error) = run(&apply(&[]), &services).await.unwrap_err();
    assert_eq!(code, 2);
    assert!(
        error
            .help()
            .unwrap()
            .to_string()
            .contains("--default-config")
    );
    // At a terminal, q leaves the tree alone.
    let services = Services {
        key: Some('q'),
        ..services
    };
    let (code, error) = run(&apply(&["--check"]), &services).await.unwrap_err();
    assert_eq!(code, 2);
    assert!(
        error.to_string().contains("the Count stays outside"),
        "{error}"
    );
    assert!(!tree.path().join("patchula.pkl").exists());

    // --default-config looks up with the Count's defaults and writes no config.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/cli/apply-defaults.json");
    let transport = patchula_testkit::from_env(&path, Default::default(), || {
        Ok(Arc::new(patchula_http::Network::new()?))
    })
    .unwrap();
    let cache = tempfile::tempdir().unwrap();
    let cache = cache.path().join("cache");
    let services = Services {
        token: Some("offline".into()),
        transport: Some(transport),
        dir: Some(tree.path().to_owned()),
        ..Services::default()
    };
    let app = apply(&[
        "--default-config",
        "--check",
        "--cache",
        cache.to_str().unwrap(),
    ]);
    let Some(super::Command::Apply(options)) = &app.command else {
        panic!("apply")
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = super::apply::command(
        &app,
        options,
        &services,
        &mut out,
        &mut err,
        &super::output::Style::PLAIN,
    )
    .await
    .unwrap();
    let err = String::from_utf8(err).unwrap();
    assert_eq!(code, 3, "{err}");
    assert!(err.contains("major  is-number  6.0.0 → 7.0.0"), "{err}");
    assert!(!tree.path().join("patchula.pkl").exists());
    assert_eq!(
        std::fs::read_to_string(tree.path().join("Dockerfile")).unwrap(),
        "FROM alpine:3.19.0\n"
    );
}

#[test]
fn cache_flag_names_a_directory() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("cache.sqlite");
    std::fs::write(&file, "").unwrap();
    let error = super::cache_dir(Some(&file)).unwrap_err();
    assert!(error.to_string().contains("takes a directory"), "{error}");
    assert_eq!(
        super::cache_dir(Some(directory.path())).unwrap(),
        directory.path()
    );
}

/// A release the Count could not write into a declaration is reported by
/// `patchula apply`, in the text and in the JSON, and is never handed to copier.
#[tokio::test]
async fn apply_reports_notices_that_are_not_templates() {
    use patchula_testkit::{Body, Cassette, Interaction, RecordedRequest, RecordedResponse};
    let tree = tempfile::tempdir().unwrap();
    for (path, content) in [
        (
            "patchula.pkl",
            "amends \"PatchulaConfig.pkl\"\ndefaults { waitHours = 0 }\nmanagers = new Listing { \"composer\" }\n",
        ),
        ("composer.json", r#"{"require":{"acme/lib":"^1 | ^2"}}"#),
        (
            "composer.lock",
            r#"{"packages":[{"name":"acme/lib","version":"2.5.0","notification-url":"https://packagist.org/downloads/"}]}"#,
        ),
    ] {
        std::fs::write(tree.path().join(path), content).unwrap();
    }
    let packagist = Interaction {
        request: RecordedRequest {
            method: "GET".into(),
            url: "https://repo.packagist.org/p2/acme/lib.json".into(),
            headers: [("accept".to_owned(), vec!["application/json".to_owned()])].into(),
            body: Body::Text("".into()),
        },
        response: RecordedResponse {
            status: 200,
            headers: Default::default(),
            body: Body::Json(json!({"packages": {"acme/lib": [
                {"version": "3.0.0", "time": "2025-12-01T00:00:00+00:00"},
                {"version": "2.5.0", "time": "2025-11-01T00:00:00+00:00"}
            ]}})),
        },
    };
    // One lookup for each of the two runs below.
    let cassette = Cassette {
        schema_version: 1,
        provenance: "Simulated from the Packagist p2 metadata format".into(),
        interactions: vec![packagist.clone(), packagist],
    };
    let scratch = tempfile::tempdir().unwrap();
    let cassette_path = scratch.path().join("notices.json");
    patchula_testkit::save(&cassette_path, &cassette, &Default::default()).unwrap();
    let transport = patchula_testkit::from_env(&cassette_path, Default::default(), || {
        Ok(Arc::new(patchula_http::Network::new()?))
    })
    .unwrap();
    let cache = scratch.path().join("cache");
    let services = Services {
        token: Some("offline".into()),
        transport: Some(transport),
        dir: Some(tree.path().to_owned()),
        ..Services::default()
    };
    let apply = async |args: &[&str]| {
        let mut words = vec![
            OsStr::new("apply"),
            OsStr::new("--cache"),
            cache.as_os_str(),
        ];
        words.extend(args.iter().map(OsStr::new));
        let app = App::parse_from(&words).unwrap();
        let Some(super::Command::Apply(options)) = &app.command else {
            panic!("apply")
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = super::apply::command(
            &app,
            options,
            &services,
            &mut out,
            &mut err,
            &super::output::Style::PLAIN,
        )
        .await
        .unwrap();
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    };
    let (_, _, err) = apply(&["--check"]).await;
    assert!(err.contains("Newer versions not proposed (1)"), "{err}");
    assert!(
        err.contains("acme/lib ^1 | ^2 → 3.0.0 available, not proposed: the constraint `^1 | ^2` cannot be rewritten"),
        "{err}"
    );
    assert!(!err.contains("Template updates"), "{err}");
    assert!(!err.contains("copier"), "{err}");
    let (_, out, _) = apply(&["--check", "--json"]).await;
    let result: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(result["notices"][0]["available"], "3.0.0");
    assert!(
        result["notices"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("without guessing"),
        "{result}"
    );
}

/// A file that cannot be opened stops the write before any file changes, and
/// a write leaves no file behind.
#[test]
fn a_write_that_cannot_open_every_file_writes_none() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "old").unwrap();
    let plan = crate::plan::Plan {
        files: BTreeMap::from([
            ("a.txt".to_owned(), ("old".to_owned(), "new".to_owned())),
            (
                "gone/b.txt".to_owned(),
                ("old".to_owned(), "new".to_owned()),
            ),
        ]),
        applied: Vec::new(),
        skipped: Vec::new(),
    };
    let error = plan.write(dir.path()).unwrap_err();
    assert!(error.written.is_empty(), "{error}");
    assert!(
        error.to_string().ends_with("nothing was written"),
        "{error}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "old"
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);

    let plan = crate::plan::Plan {
        files: BTreeMap::from([("a.txt".to_owned(), ("old".to_owned(), "new".to_owned()))]),
        applied: Vec::new(),
        skipped: Vec::new(),
    };
    plan.write(dir.path()).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "new"
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

/// Files are written in place: a hard link sees the new text, a read-only
/// file is refused before anything changes, a file in a read-only directory
/// is still written, and two paths to one file are refused.
#[cfg(unix)]
#[test]
fn writes_are_in_place_and_refuse_before_changing_anything() {
    use std::os::unix::fs::PermissionsExt;
    let plan = |files: &[&str]| crate::plan::Plan {
        files: files
            .iter()
            .map(|f| ((*f).to_owned(), ("old".to_owned(), "new".to_owned())))
            .collect(),
        applied: Vec::new(),
        skipped: Vec::new(),
    };
    let read = |path: &std::path::Path| std::fs::read_to_string(path).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("a.txt"), "old").unwrap();
    std::fs::hard_link(root.join("a.txt"), root.join("link.txt")).unwrap();
    plan(&["a.txt"]).write(root).unwrap();
    assert_eq!(read(&root.join("link.txt")), "new");

    std::fs::write(root.join("b.txt"), "old").unwrap();
    std::fs::write(root.join("locked.txt"), "old").unwrap();
    std::fs::set_permissions(
        root.join("locked.txt"),
        std::fs::Permissions::from_mode(0o444),
    )
    .unwrap();
    let error = plan(&["b.txt", "locked.txt"]).write(root).unwrap_err();
    assert!(
        error.written.is_empty() && error.partial.is_none(),
        "{error}"
    );
    assert_eq!(read(&root.join("b.txt")), "old");

    std::os::unix::fs::symlink("b.txt", root.join("alias.txt")).unwrap();
    let error = plan(&["alias.txt", "b.txt"]).write(root).unwrap_err();
    assert!(error.to_string().contains("same file"), "{error}");
    assert_eq!(read(&root.join("b.txt")), "old");

    let sealed = root.join("sealed");
    std::fs::create_dir(&sealed).unwrap();
    std::fs::write(sealed.join("c.txt"), "old").unwrap();
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o555)).unwrap();
    plan(&["sealed/c.txt"]).write(root).unwrap();
    assert_eq!(read(&sealed.join("c.txt")), "new");
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o755)).unwrap();
    let names: Vec<_> = std::fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 6, "{names:?}");
}

/// Two hard links are one file, so writing both is refused; and a plan may
/// touch more files than the process may hold open at once.
#[cfg(unix)]
#[test]
fn hard_links_are_one_file_and_wide_plans_write() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("a.txt"), "old").unwrap();
    std::fs::hard_link(root.join("a.txt"), root.join("b.txt")).unwrap();
    let plan = |files: Vec<String>| crate::plan::Plan {
        files: files
            .into_iter()
            .map(|f| (f, ("old".to_owned(), "new".to_owned())))
            .collect(),
        applied: Vec::new(),
        skipped: Vec::new(),
    };
    let error = plan(vec!["a.txt".into(), "b.txt".into()])
        .write(root)
        .unwrap_err();
    assert!(error.to_string().contains("same file"), "{error}");
    std::fs::create_dir(root.join("dir.txt")).unwrap();
    let error = plan(vec!["a.txt".into(), "dir.txt".into()])
        .write(root)
        .unwrap_err();
    assert!(error.to_string().contains("not a regular file"), "{error}");
    assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "old");
    assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "old");

    let names: Vec<String> = (0..5000).map(|n| format!("wide/f{n}.txt")).collect();
    std::fs::create_dir(root.join("wide")).unwrap();
    for name in &names {
        std::fs::write(root.join(name), "old").unwrap();
    }
    plan(names.clone()).write(root).unwrap();
    assert!(
        names
            .iter()
            .all(|name| std::fs::read_to_string(root.join(name)).unwrap() == "new")
    );
}

#[tokio::test]
async fn check_settings_names_the_field_the_engine_rules_refuse() {
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("bad.pkl");
    std::fs::write(
        &bad,
        "amends \"PatchulaSettings.pkl\"\nhttp { hostConcurrency { [\"API.github.com\"] = 8 } }\n",
    )
    .unwrap();
    let args: Vec<_> = [
        OsStr::new("config"),
        OsStr::new("check-settings"),
        bad.as_os_str(),
    ]
    .into();
    let app = App::parse_from(&args).unwrap();
    let Err((code, error)) = run(&app, &Services::default()).await else {
        panic!("invalid settings passed check-settings")
    };
    assert_eq!(code, 2);
    assert!(error.to_string().contains("hostConcurrency"), "{error}");

    let good = dir.path().join("good.pkl");
    std::fs::write(&good, "amends \"PatchulaSettings.pkl\"\n").unwrap();
    let args: Vec<_> = [
        OsStr::new("config"),
        OsStr::new("check-settings"),
        good.as_os_str(),
    ]
    .into();
    let app = App::parse_from(&args).unwrap();
    assert_eq!(run(&app, &Services::default()).await.unwrap(), 0);
}
