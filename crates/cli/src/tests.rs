//! CLI dispatch and persistent cache regression for installation-token receipts.
use super::{App, Services, run};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use patchula_cache::Cache;
use std::ffi::OsStr;

use patchula_http::{Exchange, Response, Transport};

use patchula_update::blob_sha;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

struct Fake {
    files: BTreeMap<String, String>,
    writes: Mutex<Vec<(String, Value)>>,
    prs: Mutex<Vec<Value>>,
    issues: Mutex<Vec<Value>>,
    requests: Mutex<Vec<(String, String, Value)>>,
}
impl Fake {
    fn graphql_author(pr: &Value) -> Value {
        let login = pr["user"]["login"].as_str().unwrap();
        if let Some(bot) = login.strip_suffix("[bot]") {
            json!({"__typename":"Bot", "login":bot})
        } else {
            json!({"__typename":"User", "login":login})
        }
    }
    fn new() -> Self {
        Self {
        files:BTreeMap::from([
            ("patchula.pkl".into(),"amends \"PatchulaConfig.pkl\"\nprs { dashboard = true }\ndefaults { waitHours = 0 }\nmanagers = new Listing { \"npm\" }\n".into()),
            ("package.json".into(),"{\n  \"dependencies\": {\"fixture\": \"1.0.0\", \"owner-closed-a\": \"1.0.0\", \"owner-closed-b\": \"1.0.0\"}\n}\n".into()),
        ]),writes:Default::default(),prs:Default::default(),issues:Default::default(),requests:Default::default(),
    }
    }
    fn tree(&self) -> Value {
        let tree: Vec<_> = self
            .files
            .iter()
            .map(|(p, c)| json!({"path":p,"sha":blob_sha(c),"mode":"100644","type":"blob"}))
            .collect();
        json!({"truncated":false,"tree":tree})
    }
}
impl Transport for Fake {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        let path = request.url().path();
        let body: Value = request
            .body()
            .and_then(|b| b.as_bytes())
            .and_then(|b| serde_json::from_slice(b).ok())
            .unwrap_or(Value::Null);
        self.requests.lock().unwrap().push((
            request.method().to_string(),
            path.into(),
            body.clone(),
        ));
        let (status, value) = if request.url().host_str() == Some("api.osv.dev") {
            let queries = body["queries"].as_array().map_or(0, Vec::len);
            (200, json!({"results": vec![json!({}); queries]}))
        } else if request.url().host_str() == Some("registry.npmjs.org") {
            (
                200,
                json!({"dist-tags":{"latest":"2.0.0"},"versions":{"1.0.0":{},"2.0.0":{}}}),
            )
        } else if path == "/graphql" {
            assert!(request.headers()["authorization"].is_sensitive());
            if body["query"] == patchula_engine::REPOSITORY_HEAD_QUERY {
                (
                    200,
                    json!({"data":{"repository":{"defaultBranchRef":{"name":"main","target":{"oid":"base","tree":{"oid":"base-tree"}}}}}}),
                )
            } else if body["query"] == "query{viewer{login}}" {
                (200, json!({"data":{"viewer":{"login":"patchula[bot]"}}}))
            } else if body["query"] == patchula_update::INVENTORY_QUERY {
                let prs = self.prs.lock().unwrap();
                let nodes: Vec<_> = prs.iter().filter(|p|p["state"] == "open").map(|p|json!({
                    "name":p["head"]["ref"],"target":{"oid":p["head"]["sha"],"tree":{"oid":"updated-tree"},"signature":{"isValid":true}},
                    "associatedPullRequests":{"pageInfo":{"hasNextPage":false},"nodes":[{
                        "number":p["number"],"id":p["node_id"],"url":p["html_url"],"title":p["title"],"body":p["body"],
                        "headRefName":p["head"]["ref"],"headRefOid":p["head"]["sha"],"baseRefName":p["base"]["ref"],"author":Self::graphql_author(p),
                        "headRepository":{"nameWithOwner":"demo/sandbox"},"baseRepository":{"nameWithOwner":"demo/sandbox"},
                        "labels":{"nodes":p["labels"],"pageInfo":{"hasNextPage":false}},"autoMergeRequest":p["auto_merge"]
                    }]}
                })).collect();
                let closed: Vec<_> = prs.iter().filter(|p|body["variables"]["closed"].as_array().unwrap().contains(&p["node_id"])).map(|p|json!({
                    "id":p["node_id"],"state":if p["state"] == "closed" {"CLOSED"} else {"OPEN"},"merged":false,"body":p["body"],
                    "author":Self::graphql_author(p),"headRepository":{"nameWithOwner":"demo/sandbox"},"baseRepository":{"nameWithOwner":"demo/sandbox"}
                })).collect();
                (
                    200,
                    json!({"data":{"viewer":{"login":"patchula[bot]"},"nodes":closed,"repository":{"defaultBranchRef":{"name":"main","target":{"oid":"base"}},"refs":{"nodes":nodes,"pageInfo":{"hasNextPage":false,"endCursor":null}},"issues":{"nodes":*self.issues.lock().unwrap(),"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}),
                )
            } else {
                assert_eq!(body["query"], patchula_update::REF_UPDATE);
                self.writes.lock().unwrap().push((path.into(), body));
                (
                    200,
                    json!({"data":{"updateRefs":{"clientMutationId":null}}}),
                )
            }
        } else if request.method() == reqwest::Method::POST {
            self.writes
                .lock()
                .unwrap()
                .push((path.into(), body.clone()));
            if path.ends_with("/pulls") {
                let pr = json!({"number":10,"node_id":"pr-id","html_url":"https://github.com/demo/sandbox/pull/10","title":body["title"],"body":body["body"],"state":"open","user":{"login":"patchula[bot]"},"head":{"ref":body["head"],"sha":"new-commit","repo":{"full_name":"demo/sandbox"}},"base":{"ref":"main","repo":{"full_name":"demo/sandbox"}},"labels":[{"name":"dependencies"}],"auto_merge":null,"created_at":chrono::Utc::now().to_rfc3339(),"merged_at":null});
                let mut pr = pr;
                pr["head"]["label"] = json!("demo:branch");
                pr["head"]["repo"]["updated_at"] = json!("2026-10-15T00:00:00Z");
                pr["base"]["sha"] = json!("base");
                self.prs.lock().unwrap().push(pr.clone());
                (201, pr)
            } else if path.ends_with("/issues") {
                let issue = json!({"number":100,"id":"dashboard-id","title":body["title"],"body":body["body"],"state":"OPEN","author":{"__typename":"Bot","login":"patchula"}});
                self.issues.lock().unwrap().push(issue.clone());
                (201, issue)
            } else if path.ends_with("/git/blobs") {
                (
                    201,
                    json!({"sha":blob_sha(body["content"].as_str().unwrap())}),
                )
            } else if path.ends_with("/git/trees") {
                (201, json!({"sha":"updated-tree"}))
            } else if path.ends_with("/git/commits") {
                (
                    201,
                    json!({"sha":"new-commit","verification":{"verified":true}}),
                )
            } else {
                panic!("unexpected write {path}");
            }
        } else if path == "/repos/demo/sandbox/pulls" {
            (200, json!(*self.prs.lock().unwrap()))
        } else if path == "/repos/demo/sandbox/pulls/10" {
            (
                200,
                self.prs
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|p| p["number"] == 10)
                    .unwrap()
                    .clone(),
            )
        } else if path == "/repos/demo/sandbox/branches" {
            (200, json!([]))
        } else if path == "/repos/demo/sandbox" {
            (200, json!({"default_branch":"main","node_id":"repo-id"}))
        } else if path.ends_with("/git/trees/base-tree") {
            (200, self.tree())
        } else if let Some(sha) = path.strip_prefix("/repos/demo/sandbox/git/blobs/") {
            let content = if sha == "unselected-lock" {
                "{}"
            } else {
                self.files.values().find(|c| blob_sha(c) == sha).unwrap()
            };
            (
                200,
                json!({"encoding":"base64","content":STANDARD.encode(content)}),
            )
        } else if path.ends_with("/git/commits/base") {
            (200, json!({"tree":{"sha":"base-tree"}}))
        } else if path.ends_with("/git/ref/heads/main") {
            (200, json!({"object":{"sha":"base"}}))
        } else if path.contains("/git/ref/heads/") {
            (404, json!({}))
        } else if path.ends_with("/branches/main") {
            (
                200,
                json!({"commit":{"sha":"base","commit":{"tree":{"sha":"base-tree"}}}}),
            )
        } else {
            panic!("unexpected read {path}");
        };
        Box::pin(async move {
            Ok(Response {
                status: reqwest::StatusCode::from_u16(status).unwrap(),
                headers: Default::default(),
                body: serde_json::to_vec(&value).unwrap(),
            })
        })
    }
}

fn existing_pr(package: &str, number: u64, closed: bool) -> Value {
    use sha2::{Digest, Sha256};
    // Same target tuple encoded by update_key; these historical PRs existed
    // before this process/cache. The identity is deliberately unrelated.
    let targets = json!([["npm", package, "2.0.0", "2.0.0", null]]);
    let update = Sha256::digest(serde_json::to_vec(&targets).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let marker = json!({"version":1,"identity":"0".repeat(64),"update":update});
    json!({
        "number":number,"node_id":format!("closed-{number}"),
        "state":if closed {"closed"} else {"open"},"merged_at":null,
        "body":format!("<!-- patchula:{marker} -->"),
        "title":"Existing PR", "user":{"login":"patchula[bot]"},
        "head":{"ref":format!("patchula/{package}-2.x"),"sha":"old-commit","repo":{"full_name":"demo/sandbox"}},
        "base":{"ref":"main","repo":{"full_name":"demo/sandbox"}},
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
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .with_filter(Targets::new().with_target("patchula", tracing::Level::DEBUG)),
    );
    let _guard = tracing::subscriber::set_default(subscriber);
    let fake = Arc::new(Fake::new());
    let mut foreign = existing_pr("foreign", 3, false);
    foreign["head"]["ref"] = json!("someone/feature");
    foreign["user"]["login"] = json!("someone");
    foreign["body"] = json!("unrelated private PR text");
    *fake.prs.lock().unwrap() = vec![
        existing_pr("owner-closed-a", 1, true),
        existing_pr("owner-closed-b", 2, true),
        foreign,
    ];
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
        fake.requests.lock().unwrap().clear();
        fake.writes.lock().unwrap().clear();
        // Exactly the main CLI dispatch: only network I/O and token lookup are
        // substituted. batch opens/closes a fresh disk cache and engine each run.
        let services = Services {
            token: Some("offline-installation".into()),
            transport: Some(fake.clone()),
            ..Services::default()
        };
        assert_eq!(run(&app, &services).await.unwrap(), 0);
        let cache = Cache::open_dir(&cache_path, Default::default())
            .await
            .unwrap();
        let bytes = cache
            .extraction_get(&patchula_cache::ExtractionKey {
                repo: "demo/sandbox".into(),
                default_branch_commit: "patchula/".into(),
                config_hash: "verified-write-receipts-v3".into(),
            })
            .await
            .unwrap()
            .expect("CLI must persist receipts before returning");
        let receipts: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(receipts["writes"].as_object().unwrap().len(), 1);
        assert_eq!(receipts["closed"].as_object().unwrap().len(), 2);
        drop(cache);
        if second {
            let requests = fake.requests.lock().unwrap();
            let inventory = requests
                .iter()
                .position(|(_, _, b)| b["query"] == patchula_update::INVENTORY_QUERY)
                .unwrap();
            assert_eq!(requests.len(), 7, "{requests:?}");
            assert!(inventory > 0, "inventory follows repository discovery");
            assert!(
                requests.iter().all(|(method, path, body)| method == "GET"
                    || body["query"] == patchula_update::INVENTORY_QUERY
                    || path == "/v1/querybatch"),
                "only the inventory and advisory queries may use POST: {requests:?}"
            );
            assert!(fake.writes.lock().unwrap().is_empty());
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
}

#[test]
fn github_remotes_name_their_repository() {
    use super::github_repository;
    for url in [
        "https://github.com/sweepies/count-patchula",
        "https://github.com/sweepies/count-patchula.git",
        "https://github.com/sweepies/count-patchula/",
        "https://x-access-token:secret@github.com/sweepies/count-patchula.git",
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

/// Stands in for `gh`: records each command and answers with `result`.
struct Gh {
    result: Result<&'static str, &'static str>,
    calls: Mutex<Vec<(String, Vec<String>, std::time::Duration)>>,
}
impl Gh {
    fn answering(result: Result<&'static str, &'static str>) -> Self {
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
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<u8>, String>> + Send + '_>>
    {
        self.calls.lock().unwrap().push((
            command.program.display().to_string(),
            command.args,
            command.timeout,
        ));
        let result = self
            .result
            .map(|out| out.as_bytes().to_vec())
            .map_err(String::from);
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
        Err("cannot start gh: No such file or directory"),
        Err("exit status: 1: no oauth token found for github.com"),
        Err("tool exceeded 10s time limit"),
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

    // The largest update for each declaration is applied, and then nothing waits.
    let (code, _, err) = apply(&[]).await;
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("applied 2 updates to 2 files"), "{err}");
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
        (&["--manager", "npn"], "--manager npn is not a manager"),
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
#[cfg(not(target_os = "macos"))]
fn user_cache_dir_follows_xdg_then_home() {
    use std::{collections::HashMap, ffi::OsString, path::PathBuf};
    let lookup = |vars: &[(&str, &str)]| {
        let vars: HashMap<String, OsString> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        super::user_cache_dir(|name| vars.get(name).cloned())
    };
    assert_eq!(
        lookup(&[("XDG_CACHE_HOME", "/xdg"), ("HOME", "/home/count")]),
        Some(PathBuf::from("/xdg"))
    );
    assert_eq!(
        lookup(&[("XDG_CACHE_HOME", "relative"), ("HOME", "/home/count")]),
        Some(PathBuf::from("/home/count/.cache"))
    );
    assert_eq!(lookup(&[("HOME", "")]), None);
    assert_eq!(lookup(&[]), None);
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
