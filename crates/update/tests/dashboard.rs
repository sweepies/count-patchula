use patchula_report::Repository;
use patchula_update::{
    Inventory, Recheck,
    dashboard::{self, MARKER},
};
use serde_json::json;

fn repository() -> Repository {
    serde_json::from_value(json!({"repository":"demo/app","status":"complete","reason":null,
        "warnings":["fixture: cached after rate limiting"],"packageFiles":[{"path":"package.json","dependencies":[{
            "manager":"npm","name":"fixture","package":"fixture","current":{"value":"1.0.0","version":"1.0.0","digest":null},"updates":[],
            "metadata":{"source":"npm","kinds":[],"locations":[],"stale":{"catalog":123},"heldBack":[{
                "version":"2.0.0","updateType":"major","publishedAt":"2026-10-15T00:00:00Z","eligibleAt":"2026-10-16T00:00:00Z"
            }]}
        }]}]})).unwrap()
}
#[test]
fn held_back_dashboard_reports_the_reused_catalog_timestamp() {
    let mut repo = repository();
    let metadata = repo.package_files[0].dependencies[0]
        .metadata
        .as_mut()
        .unwrap();
    metadata.lookup = Some(
        serde_json::from_value(json!({
            "evaluatedAt":"2026-10-15T12:00:00Z", "disabledUpdates":[], "rejected":[],
            "catalog":{"fetchedAt":"2026-10-15T09:00:00Z", "reused":true}
        }))
        .unwrap(),
    );
    let text = dashboard::render(&repo, &[], &[], Recheck::NextRun, None);
    assert!(
        text.contains("eligible at 2026-10-16T00:00:00Z; catalog fetched at 2026-10-15T09:00:00Z")
    );
}
#[test]
fn a_failed_lookup_is_listed_once_under_its_dependency() {
    let mut repo = repository();
    repo.warnings.clear();
    repo.lookup_failures = serde_json::from_value(json!([{
        "manager":"npm","path":"package.json","name":"fixture","package":"fixture",
        "datasource":"npm","kind":"network","message":"registry HTTP 503"
    }]))
    .unwrap();
    let text = dashboard::render(&repo, &[], &[], Recheck::NextRun, None);
    assert_eq!(
        text.matches("Lookup failed (npm, network): registry HTTP 503")
            .count(),
        1,
        "{text}"
    );
    assert!(!text.contains("## Report warnings"), "{text}");
}
#[test]
fn dashboard_snapshot_and_identity_do_not_depend_on_wording() {
    let repo = repository();
    let dep = &repo.package_files[0].dependencies[0];
    let text = dashboard::render(&repo, &[], &[], Recheck::NextRun, None);
    let repo_target = text
        .lines()
        .find(|l| l.contains("\"recheck\""))
        .unwrap()
        .split("\"target\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let snapshot = text
        .replace(repo_target, "REPOSITORY")
        .replace(
            &dashboard::dependency_key("package.json", dep),
            "DEPENDENCY",
        )
        .replace(
            &dashboard::held_key("package.json", dep, "2.0.0", "major"),
            "HELD_UPDATE",
        );
    assert_eq!(snapshot, include_str!("snapshots/dashboard.md"));
    assert_eq!(
        dashboard::render(&repo, &[], &[], Recheck::Now, None),
        text.replace("on the next run", "now")
    );
    let mut other = dep.clone();
    other.current.value = Some("1.1.0".into());
    assert_ne!(
        dashboard::dependency_key("package.json", dep),
        dashboard::dependency_key("package.json", &other)
    );
}
#[test]
fn dashboard_orders_pr_numbers_numerically_and_dependency_summaries_last() {
    let repo = repository();
    let prs: Vec<_> = [24, 3, 11]
        .into_iter()
        .map(|number| json!({"number":number,"state":"open","title":format!("PR {number}")}))
        .collect();
    let text = dashboard::render(&repo, &[], &prs, Recheck::NextRun, None);
    let positions: Vec<_> = [
        "## Open PRs",
        "- #3:",
        "- #11:",
        "- #24:",
        "## Pending updates",
        "## Lookup warnings",
        "## Report warnings",
        "<details>",
    ]
    .into_iter()
    .map(|label| text.find(label).unwrap())
    .collect();
    assert!(positions.windows(2).all(|p| p[0] < p[1]), "{text}");
    let mut reversed = prs;
    reversed.reverse();
    assert_eq!(
        text,
        dashboard::render(&repo, &[], &reversed, Recheck::NextRun, None)
    );
}

#[test]
fn issue_discovery_checks_marker_author_and_ambiguity_never_title() {
    let mut inventory = Inventory {
        actor: "patchula[bot]".into(),
        ..Default::default()
    };
    inventory.issues = vec![
        json!({"number":1,"title":"Count Patchula's dependency dashboard","body":MARKER,"author":{"__typename":"User","login":"human"}}),
    ];
    assert!(dashboard::issue(&inventory).unwrap().is_none());
    inventory.issues.push(json!({"number":2,"title":"Renamed by owner","body":MARKER,"author":{"__typename":"Bot","login":"patchula"}}));
    assert_eq!(dashboard::issue(&inventory).unwrap().unwrap()["number"], 2);
    inventory.issues.push(inventory.issues[1].clone());
    assert!(dashboard::issue(&inventory).is_err());
}

#[test]
fn checkbox_parser_rejects_malformed_duplicate_fenced_foreign_and_wrong_repository_markers() {
    let repo = repository();
    let text = dashboard::render(&repo, &[], &[], Recheck::NextRun, None);
    let line = text
        .lines()
        .find(|l| l.contains("\"recheck\""))
        .unwrap()
        .replace("- [ ]", "- [x]");
    for (name, content, author, expected) in [
        ("valid", format!("{MARKER}\n{line}"), "patchula", true),
        (
            "duplicate",
            format!("{MARKER}\n{line}\n{line}"),
            "patchula",
            false,
        ),
        (
            "fenced",
            format!("{MARKER}\n```md\n{line}\n```"),
            "patchula",
            false,
        ),
        ("foreign", format!("{MARKER}\n{line}"), "other", false),
        (
            "malformed",
            format!(
                "{MARKER}\n{}",
                line.replace("\"action\":\"recheck\"", "\"action\":\"unknown\"")
            ),
            "patchula",
            false,
        ),
        (
            "short-target",
            format!(
                "{MARKER}\n- [x] Text <!-- patchula-action:{{\"action\":\"recheck\",\"target\":\"abc\"}} -->"
            ),
            "patchula",
            false,
        ),
    ] {
        let inventory = Inventory {
            actor: "patchula[bot]".into(),
            issues: vec![json!({"body":content,"author":{"__typename":"Bot","login":author}})],
            ..Default::default()
        };
        assert_eq!(
            dashboard::Controls::read(&inventory, "demo/app", true)
                .unwrap()
                .recheck,
            expected,
            "{name}"
        );
        assert!(
            !dashboard::Controls::read(&inventory, "other/repository", true)
                .unwrap()
                .recheck,
            "{name}"
        );
        assert!(
            !dashboard::Controls::read(&inventory, "demo/app", false)
                .unwrap()
                .recheck,
            "{name}"
        );
    }
}

#[test]
fn pr_checkbox_requires_bot_author_same_repository_and_a_valid_ownership_marker() {
    for case in [
        "valid",
        "foreign",
        "fork",
        "bad-marker",
        "duplicate",
        "fenced",
    ] {
        let ownership = format!(
            "<!-- patchula:{{\"version\":1,\"identity\":\"{}\",\"update\":\"{}\"}} -->",
            "a".repeat(64),
            "b".repeat(64)
        );
        let checkbox = "- [X] Changed wording <!-- patchula-recheck -->";
        let body = match case {
            "bad-marker" => checkbox.into(),
            "duplicate" => format!("{checkbox}\n{checkbox}\n{ownership}"),
            "fenced" => format!("```md\n{checkbox}\n```\n{ownership}"),
            _ => format!("{checkbox}\n{ownership}"),
        };
        let pr = json!({"body":body,"headRefName":"patchula/fixture","author":{"__typename":"Bot","login":if case == "foreign" {"other"} else {"patchula"}},
            "headRepository":{"nameWithOwner":if case == "fork" {"other/app"} else {"demo/app"}},"baseRepository":{"nameWithOwner":"demo/app"},"labels":{"nodes":[],"pageInfo":{"hasNextPage":false}}});
        let inventory = Inventory {
            actor: "patchula[bot]".into(),
            refs: vec![json!({"associatedPullRequests":{"nodes":[pr]}})],
            ..Default::default()
        };
        let controls = dashboard::Controls::read(&inventory, "demo/app", true).unwrap();
        assert_eq!(
            controls.branches.contains("patchula/fixture"),
            case == "valid",
            "{case}"
        );
    }
}

struct InventoryPages(std::sync::Mutex<usize>);
impl patchula_http::Transport for InventoryPages {
    fn send(&self, request: reqwest::Request) -> patchula_http::Exchange<'_> {
        let input: serde_json::Value =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(input["query"], patchula_update::INVENTORY_QUERY);
        *self.0.lock().unwrap() += 1;
        let page = |cursor: &serde_json::Value, pages: usize, prefix: &str| {
            let offset = cursor
                .as_str()
                .map(|c| c.parse::<usize>().unwrap())
                .unwrap_or(0);
            json!({"nodes":[{"name":format!("{prefix}{offset}")}],"pageInfo":{"hasNextPage":offset + 1 < pages,"endCursor":(offset+1).to_string()}})
        };
        let value = json!({"data":{"viewer":{"login":"patchula[bot]"},"nodes":[],"repository":{
            "defaultBranchRef":{"name":"main","target":{"oid":"base"}},
            "refs":page(&input["variables"]["cursor"],2,"ref"),
            "issues":page(&input["variables"]["issueCursor"],3,"issue")
        }}});
        Box::pin(async move {
            Ok(patchula_http::Response {
                status: reqwest::StatusCode::OK,
                headers: Default::default(),
                body: serde_json::to_vec(&value).unwrap(),
            })
        })
    }
}
#[tokio::test]
async fn shared_inventory_follows_each_connection_without_duplicating_finished_pages() {
    let fake = std::sync::Arc::new(InventoryPages(Default::default()));
    let cache = patchula_cache::Cache::open(":memory:", Default::default())
        .await
        .unwrap();
    let github = patchula_github::Client::with_transport(
        "offline",
        cache.clone(),
        Default::default(),
        fake.clone(),
    )
    .unwrap();
    let config = patchula_config::load(std::path::Path::new("../../PatchulaConfig.pkl")).unwrap();
    let mut repo = repository();
    repo.commit = Some("base".into());
    repo.default_branch = Some("main".into());
    let inventory = Inventory::read(&github, &cache, &repo, &config)
        .await
        .unwrap();
    assert_eq!(
        inventory
            .refs
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ref0", "ref1"]
    );
    assert_eq!(
        inventory
            .issues
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["issue0", "issue1", "issue2"]
    );
    assert_eq!(*fake.0.lock().unwrap(), 3);
}
#[test]
fn the_dashboard_hides_the_advertised_server_under_its_marker() {
    let repo = repository();
    let text = dashboard::render(
        &repo,
        &[],
        &[],
        Recheck::Now,
        Some("https://patchula.acme.dev"),
    );
    assert_eq!(
        patchula_update::advertise::find(&text),
        Some("https://patchula.acme.dev")
    );
    assert_eq!(text.lines().filter(|l| *l == dashboard::MARKER).count(), 1);
    assert_eq!(
        patchula_update::advertise::find(&dashboard::render(&repo, &[], &[], Recheck::Now, None)),
        None
    );
}

#[test]
fn known_vulnerabilities_lead_the_dashboard_with_their_fix() {
    let mut repo = repository();
    let dep = &mut repo.package_files[0].dependencies[0];
    dep.updates = vec![serde_json::from_value(json!({
        "newValue":"1.0.5","newVersion":"1.0.5","newDigest":null,"updateType":"patch","group":null,
        "fixes":["GHSA-fixed"]
    }))
    .unwrap()];
    dep.metadata.as_mut().unwrap().advisories = serde_json::from_value(json!([
        {"id":"GHSA-fixed","aliases":["CVE-2026-0001"],"severity":"high","summary":"Prototype pollution",
         "url":"https://osv.dev/vulnerability/GHSA-fixed","fixedIn":"1.0.5"},
        {"id":"GHSA-open","url":"https://osv.dev/vulnerability/GHSA-open"}
    ]))
    .unwrap();
    let text = dashboard::render(&repo, &[], &[], Recheck::NextRun, None);
    let section = text.find("## Known vulnerabilities").unwrap();
    assert!(section < text.find("## Pending updates").unwrap());
    assert!(text.contains("- fixture 1.0.0 (package.json): [GHSA-fixed](https://osv.dev/vulnerability/GHSA-fixed) (CVE-2026-0001), high; fixed by the update to 1.0.5. Prototype pollution\n"), "{text}");
    assert!(text.contains("- fixture 1.0.0 (package.json): [GHSA-open](https://osv.dev/vulnerability/GHSA-open), unrated; no fixed release yet\n"), "{text}");
}

#[test]
fn template_notices_are_listed_without_a_branch_or_a_checkbox() {
    let mut repo = repository();
    repo.notices = serde_json::from_value(json!([{
        "manager":"copier","path":".copier-answers.yml","line":2,"name":"gh:acme/template",
        "package":"acme/template","current":"v1.4.0","available":"v1.6.0",
        "compareUrl":"https://github.com/acme/template/compare/v1.4.0...v1.6.0"
    }]))
    .unwrap();
    let text = dashboard::render(&repo, &[], &[], Recheck::NextRun, None);
    let section = text.find("## Template updates").unwrap();
    assert!(section > text.find("## Pending updates").unwrap());
    let row = "- template gh:acme/template: v1.4.0 → v1.6.0 available in .copier-answers.yml; run copier update to apply it ([compare](https://github.com/acme/template/compare/v1.4.0...v1.6.0))\n";
    assert!(text[section..].contains(row), "{text}");
    assert!(
        !text
            .lines()
            .any(|l| l.contains("acme/template") && l.contains("patchula-action"))
    );
}
