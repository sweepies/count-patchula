use patchula_report::Repository;
use patchula_update::{Inventory, dashboard, identity, markers::DASHBOARD as MARKER};
use patchula_words::Recheck;
use serde_json::json;

fn repository() -> Repository {
    serde_json::from_value(json!({"repository":"demo/app","status":"complete","reason":null,
        "warnings":["fixture: cached after rate limiting","the cache was warmed by an earlier run"],"packageFiles":[{"path":"package.json","dependencies":[{
            "manager":"npm","name":"fixture","package":"fixture","current":{"value":"1.0.0","version":"1.0.0","digest":null},"updates":[],
            "metadata":{"source":"npm","kinds":[],"locations":[],"stale":{"catalog":123},"heldBack":[{
                "version":"2.0.0","updateType":"major","publishedAt":"2026-10-15T00:00:00Z","eligibleAt":"2026-10-16T00:00:00Z"
            }]}
        }]}]})).unwrap()
}
fn tracked(
    number: u64,
    state: &str,
    branch: &str,
    title: &str,
) -> patchula_update::pull_requests::Tracked {
    patchula_update::pull_requests::Tracked::from(
        serde_json::from_value::<patchula_github::model::PullRequest>(
            json!({"node_id":format!("N{number}"),"number":number,
            "html_url":format!("https://github.com/demo/app/pull/{number}"),"state":state,
            "title":title,"labels":[],"user":{"login":"bot"},
            "head":{"ref":branch,"sha":"h","repo":{"full_name":"demo/app"}},
            "base":{"ref":"main","repo":{"full_name":"demo/app"}}}),
        )
        .unwrap(),
    )
}
#[test]
fn a_held_update_says_when_it_becomes_eligible_and_not_when_its_catalog_was_fetched() {
    // Whether a catalog is reused, and when it was fetched, change from run to
    // run; the dashboard must not, or an unchanged repository would rewrite it.
    let render = |fetched: &str, reused: bool| {
        let mut repo = repository();
        let metadata = repo.package_files[0].dependencies[0]
            .metadata
            .as_mut()
            .unwrap();
        metadata.lookup = Some(
            serde_json::from_value(json!({
                "evaluatedAt":"2026-10-15T12:00:00Z", "disabledUpdates":[], "rejected":[],
                "catalog":{"fetchedAt":fetched, "reused":reused}
            }))
            .unwrap(),
        );
        dashboard::render(&repo, &[], &[], &[], Recheck::NextRun, None)
    };
    let first = render("2026-10-15T09:00:00Z", false);
    assert!(
        first.contains("; eligible 16 Oct 00:00 UTC) <!--"),
        "{first}"
    );
    assert!(
        !first.contains("catalog fetched") && !first.contains("2026-10-15T09"),
        "{first}"
    );
    for (fetched, reused) in [
        ("2026-10-15T09:00:00Z", true),
        ("2026-10-16T23:59:59.999Z", true),
        ("2026-10-15 09:00", false),
    ] {
        assert_eq!(render(fetched, reused), first, "{fetched} {reused}");
    }
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
    let text = dashboard::render(&repo, &[], &[], &[], Recheck::NextRun, None);
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
    let text = dashboard::render(&repo, &[], &[], &[], Recheck::NextRun, None);
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
        .replace(&identity::retry_key("npm", "fixture", "npm"), "RETRY")
        .replace(
            &identity::held_key("package.json", dep, "2.0.0", "major"),
            "HELD_UPDATE",
        );
    assert_eq!(snapshot, include_str!("snapshots/dashboard.md"));
    assert_eq!(
        dashboard::render(&repo, &[], &[], &[], Recheck::Now, None),
        text.replace("on the next run", "now")
    );
    let mut other = dep.clone();
    other.current.value = Some("1.1.0".into());
    assert_ne!(
        identity::dependency_key("package.json", dep),
        identity::dependency_key("package.json", &other)
    );
}
#[test]
fn dashboard_orders_pr_numbers_numerically_and_dependency_summaries_last() {
    let repo = repository();
    let prs: Vec<_> = [24, 3, 11]
        .into_iter()
        .map(|number| tracked(number, "open", "patchula/other", &format!("PR {number}")))
        .collect();
    let text = dashboard::render(&repo, &[], &prs, &[], Recheck::NextRun, None);
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
        dashboard::render(&repo, &[], &reversed, &[], Recheck::NextRun, None)
    );
}

#[test]
fn issue_discovery_checks_marker_author_and_ambiguity_never_title() {
    let issue = |number: i64, title: &str, login: &str, typename: &str| {
        serde_json::from_value(
            json!({"id":format!("i{number}"),"number":number,"title":title,
            "body":MARKER,"state":"OPEN","author":{"__typename":typename,"login":login}}),
        )
        .unwrap()
    };
    let mut inventory = Inventory {
        actor: "patchula[bot]".into(),
        ..Default::default()
    };
    inventory.issues = patchula_update::Page::of(vec![issue(
        1,
        "Count Patchula's dependency dashboard",
        "human",
        "User",
    )]);
    assert!(dashboard::issue(&inventory).unwrap().is_none());
    inventory.issues = patchula_update::Page::of(vec![
        issue(1, "Count Patchula's dependency dashboard", "human", "User"),
        issue(2, "Renamed by owner", "patchula", "Bot"),
    ]);
    assert_eq!(dashboard::issue(&inventory).unwrap().unwrap().number, 2);
    inventory.issues = patchula_update::Page::of(vec![
        issue(2, "Renamed by owner", "patchula", "Bot"),
        issue(3, "Renamed by owner", "patchula", "Bot"),
    ]);
    assert!(dashboard::issue(&inventory).is_err());
}

#[test]
fn checkbox_parser_rejects_malformed_duplicate_fenced_foreign_and_wrong_repository_markers() {
    let repo = repository();
    let text = dashboard::render(&repo, &[], &[], &[], Recheck::NextRun, None);
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
            issues: patchula_update::Page::of(vec![
                serde_json::from_value(
                    json!({"id":"i1","number":1,"title":"dashboard","body":content,
                       "state":"OPEN","author":{"__typename":"Bot","login":author}}),
                )
                .unwrap(),
            ]),
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
        let pr = json!({"number":7,"id":"pr-7","url":"https://github.com/demo/app/pull/7",
            "title":"Update","body":body,"headRefName":"patchula/fixture","headRefOid":"oid",
            "baseRefName":"main","author":{"__typename":"Bot","login":if case == "foreign" {"other"} else {"patchula"}},
            "headRepository":{"nameWithOwner":if case == "fork" {"other/app"} else {"demo/app"}},"baseRepository":{"nameWithOwner":"demo/app"},"labels":{"nodes":[],"pageInfo":{"hasNextPage":false}}});
        let inventory = Inventory {
            actor: "patchula[bot]".into(),
            refs: patchula_update::Page::of(vec![
                serde_json::from_value(json!({"name":"patchula/fixture","target":null,
                    "associatedPullRequests":{"nodes":[pr],"pageInfo":{"hasNextPage":false}}}))
                .unwrap(),
            ]),
            issues: patchula_update::Page::of(vec![]),
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

/// Records `(document, issueFilter)` per request and answers canned pages:
/// `refs` has two pages, `issues` three, over the stored `issue`/`ref`
/// nodes. The filter is recorded, not applied — the differential suite
/// proves the filtered set; this test proves the document sequence.
struct InventoryPages(std::sync::Mutex<Vec<(String, serde_json::Value)>>);
impl patchula_http::Transport for InventoryPages {
    fn send(&self, request: reqwest::Request) -> patchula_http::Exchange<'_> {
        let input: serde_json::Value =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        let page = |cursor: &serde_json::Value, pages: usize, node: serde_json::Value| {
            let offset = cursor
                .as_str()
                .map(|c| c.parse::<usize>().unwrap())
                .unwrap_or(0);
            json!({"nodes":[node],"pageInfo":{"hasNextPage":offset + 1 < pages,"endCursor":(offset+1).to_string()}})
        };
        let n = |v: &serde_json::Value| {
            v.as_str()
                .and_then(|c| c.parse::<usize>().ok())
                .unwrap_or(0)
        };
        let head = json!({"name":"main","target":{"oid":"base"}});
        let refs = |input: &serde_json::Value| {
            page(
                &input["variables"]["cursor"],
                2,
                json!({"name":format!("ref{}",n(&input["variables"]["cursor"])),"target":null,"associatedPullRequests":{"nodes":[],"pageInfo":{"hasNextPage":false}}}),
            )
        };
        let issues = |input: &serde_json::Value| {
            page(
                &input["variables"]["issueCursor"],
                3,
                json!({"id":format!("i{}",n(&input["variables"]["issueCursor"])),"number":n(&input["variables"]["issueCursor"]),"title":"t","body":"b","state":"OPEN"}),
            )
        };
        let query = input["query"].as_str().unwrap();
        let (name, value) = if query == patchula_update::INVENTORY_QUERY {
            // The first page asks for everything once.
            (
                "first",
                json!({"data":{"viewer":{"login":"patchula[bot]"},"nodes":[],"repository":{
                    "id":"repo-id","autoMergeAllowed":false,"squashMergeAllowed":true,
                    "mergeCommitAllowed":false,"rebaseMergeAllowed":false,"deleteBranchOnMerge":false,
                    "defaultBranchRef":{"name":"main","target":{"__typename":"Commit","oid":"base"}},
                    "refs":refs(&input),
                    "issues":issues(&input)
                }}}),
            )
        } else if query == patchula_update::INVENTORY_NEXT_QUERY {
            // Continuations carry every connection that still has a page,
            // the actor and the default branch's head — nothing else.
            (
                "next",
                json!({"data":{"viewer":{"login":"patchula[bot]"},"repository":{
                    "defaultBranchRef":head,
                    "refs":refs(&input),
                    "issues":issues(&input)
                }}}),
            )
        } else if query == patchula_update::INVENTORY_REFS_QUERY {
            (
                "refs",
                json!({"data":{"viewer":{"login":"patchula[bot]"},"repository":{
                    "defaultBranchRef":head,
                    "refs":refs(&input)
                }}}),
            )
        } else if query == patchula_update::INVENTORY_ISSUES_QUERY {
            (
                "issues",
                json!({"data":{"viewer":{"login":"patchula[bot]"},"repository":{
                    "defaultBranchRef":head,
                    "issues":issues(&input)
                }}}),
            )
        } else if query == patchula_update::INVENTORY_LAST_QUERY {
            // The closing re-read of the first page's constants, sent when
            // the walk paged: the same constants, no connections.
            (
                "last",
                json!({"data":{"viewer":{"login":"patchula[bot]"},"nodes":[],"repository":{
                    "id":"repo-id","autoMergeAllowed":false,"squashMergeAllowed":true,
                    "mergeCommitAllowed":false,"rebaseMergeAllowed":false,"deleteBranchOnMerge":false,
                    "defaultBranchRef":{"name":"main","target":{"__typename":"Commit","oid":"base"}}
                }}}),
            )
        } else {
            panic!("unexpected inventory document: {query}");
        };
        self.0
            .lock()
            .unwrap()
            .push((name.into(), input["variables"]["issueFilter"].clone()));
        Box::pin(async move {
            Ok(patchula_http::Response {
                status: reqwest::StatusCode::OK,
                headers: Default::default(),
                body: serde_json::to_vec(&value).unwrap(),
                stale: None,
            })
        })
    }
}
#[tokio::test]
async fn split_inventory_pages_each_connection_once_and_never_repeats_a_finished_one() {
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
    let read = || Inventory::read(&github, &cache, &repo, &config);
    let names = |inventory: &Inventory| {
        inventory
            .refs
            .items
            .iter()
            .flatten()
            .map(|r| r.name.clone())
            .collect::<Vec<String>>()
    };
    let titles = |inventory: &Inventory| {
        inventory
            .issues
            .items
            .iter()
            .flatten()
            .map(|i| i.title.clone())
            .collect::<Vec<String>>()
    };
    let filtered = || json!({"createdBy":"patchula[bot]"});
    let sent = |fake: &InventoryPages| std::mem::take(&mut *fake.0.lock().unwrap());
    // Cold, without a stored actor: the unfiltered first page leaves issues
    // unfinished, so that connection restarts on the answer's actor — the
    // one wasted page is the cold tax the hint buys back.
    let inventory = read().await.unwrap();
    assert_eq!(names(&inventory), ["ref0", "ref1"]);
    assert_eq!(titles(&inventory), ["t", "t", "t"]);
    assert_eq!(
        sent(&fake),
        [
            ("first".to_string(), serde_json::Value::Null),
            ("next".into(), filtered()),
            ("issues".into(), filtered()),
            ("issues".into(), filtered()),
            ("last".into(), serde_json::Value::Null),
        ]
    );
    // Warm: the stored actor filters the first page, and one continuation
    // carries both connections until `refs` finishes — three requests, not
    // refs plus issues, then the closing re-read of the constants.
    let inventory = read().await.unwrap();
    assert_eq!(names(&inventory), ["ref0", "ref1"]);
    assert_eq!(titles(&inventory), ["t", "t", "t"]);
    assert_eq!(
        sent(&fake),
        [
            ("first".to_string(), filtered()),
            ("next".into(), filtered()),
            ("issues".into(), filtered()),
            ("last".into(), serde_json::Value::Null),
        ]
    );
}
#[test]
fn the_dashboard_hides_the_advertised_server_under_its_marker() {
    let repo = repository();
    let text = dashboard::render(
        &repo,
        &[],
        &[],
        &[],
        Recheck::Now,
        Some("https://patchula.acme.dev"),
    );
    assert_eq!(
        patchula_update::advertise::find(&text),
        Some("https://patchula.acme.dev")
    );
    assert_eq!(text.lines().filter(|l| *l == MARKER).count(), 1);
    assert_eq!(
        patchula_update::advertise::find(&dashboard::render(
            &repo,
            &[],
            &[],
            &[],
            Recheck::Now,
            None
        )),
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
    let text = dashboard::render(&repo, &[], &[], &[], Recheck::NextRun, None);
    let section = text.find("## Known vulnerabilities").unwrap();
    assert!(section < text.find("## Pending updates").unwrap());
    let row = "- **high**: `fixture` 1.0.0 in `package.json`: 2 advisories ([GHSA-fixed](https://osv.dev/vulnerability/GHSA-fixed), [GHSA-open](https://osv.dev/vulnerability/GHSA-open)). 1 fixed by the update to `1.0.5`; 1 no fixed release yet.\n";
    assert!(text.contains(row), "{text}");
    assert!(
        text.contains("  - [GHSA-fixed](https://osv.dev/vulnerability/GHSA-fixed) (CVE-2026-0001), high: Prototype pollution\n"),
        "{text}"
    );
    assert!(!text.contains("unrated"), "{text}");
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
    let text = dashboard::render(&repo, &[], &[], &[], Recheck::NextRun, None);
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

#[test]
fn images_the_count_cant_vouch_for_follow_known_vulnerabilities() {
    let mut repo = repository();
    repo.warnings.clear();
    repo.package_files[0].dependencies[0]
        .metadata
        .as_mut()
        .unwrap()
        .advisories = serde_json::from_value(json!([
        {"id":"GHSA-open","url":"https://osv.dev/vulnerability/GHSA-open"}
    ]))
    .unwrap();
    let image =
        |package: &str, tag: &str, state: serde_json::Value| -> patchula_report::Dependency {
            serde_json::from_value(json!({
                "manager":"compose","name":package,"package":package,
                "current":{"value":tag,"version":tag,"digest":"sha256:aa"},"updates":[],
                "metadata":{"source":"docker","kinds":[],"locations":[],"image":state}
            }))
            .unwrap()
        };
    repo.package_files
        .push(serde_json::from_value(json!({"path":"compose.yaml","dependencies":[]})).unwrap());
    let deps = &mut repo.package_files[1].dependencies;
    deps.push(image(
        "quay.io/argoproj/argocd",
        "v3.1.8",
        json!({"state":"no-sbom"}),
    ));
    deps.push(image(
        "ghcr.io/acme/api",
        "2.4",
        json!({"state":"sbom-without-os","detail":"ko"}),
    ));
    deps.push(image(
        "mysql",
        "8.4",
        json!({"state":"os-not-covered","os":"ol 9"}),
    ));
    deps.push(image(
        "ghcr.io/acme/internal-base",
        "1",
        json!({"state":"no-sbom","silenced":"built from our own Wolfi base, scanned in CI"}),
    ));
    let text = dashboard::render(&repo, &[], &[], &[], Recheck::NextRun, None);
    let section = text.find("## Images the Count can't vouch for\n").unwrap();
    assert!(
        text.find("## Known vulnerabilities").unwrap() < section,
        "{text}"
    );
    assert!(text.find("## Pending updates").unwrap() > section, "{text}");
    for line in [
        "- `quay.io/argoproj/argocd:v3.1.8@sha256:aa` in `compose.yaml`: no SBOM published\n",
        "- `ghcr.io/acme/api:2.4@sha256:aa` in `compose.yaml`: its SBOM (made by ko) lists no OS packages\n",
        "- `mysql:8.4@sha256:aa` in `compose.yaml`: ol 9, which OSV.dev doesn't cover\n",
        "<details><summary>Silenced (1)</summary>\n\n- `ghcr.io/acme/internal-base:1@sha256:aa` in `compose.yaml`: built from our own Wolfi base, scanned in CI\n</details>\n",
    ] {
        assert!(text[section..].contains(line), "{line}\n{text}");
    }
    assert_eq!(
        text.matches("## Images the Count can't vouch for").count(),
        1
    );
}

#[test]
fn tooling_warnings_sit_between_known_vulnerabilities_and_images() {
    let mut repo = repository();
    repo.warnings.clear();
    repo.default_branch = Some("main".into());
    let metadata = repo.package_files[0].dependencies[0]
        .metadata
        .as_mut()
        .unwrap();
    metadata.image = Some(serde_json::from_value(json!({"state":"no-sbom"})).unwrap());
    metadata.advisories = serde_json::from_value(json!([
        {"id":"GHSA-open","url":"https://osv.dev/vulnerability/GHSA-open"}
    ]))
    .unwrap();
    let warning = |rule: &str, detail: Option<&str>, silenced: Option<&str>| {
        patchula_report::ToolingWarning {
            rule: rule.parse().unwrap(),
            detail: detail.map(|d| d.parse().unwrap()),
            branches: vec!["patchula/fixture".into(), "patchula/other".into()],
            silenced: silenced.map(str::to_owned),
        }
    };
    repo.tooling = vec![
        warning("merge-without-required-checks", Some("reviews-only"), None),
        warning("security-merge-without-delivery", None, None),
        warning(
            "merge-without-ci",
            None,
            Some("CI runs in Buildkite, reporting to another org"),
        ),
    ];
    let prs = [
        tracked(412, "open", "patchula/fixture", "Update fixture"),
        tracked(7, "closed", "patchula/other", "Old"),
    ];
    let text = dashboard::render(&repo, &[], &prs, &[], Recheck::NextRun, None);
    let section = text
        .find("## Tooling warnings\n")
        .unwrap_or_else(|| panic!("{text}"));
    let known = text.find("## Known vulnerabilities\n").unwrap();
    let images = text.find("## Images the Count can't vouch for").unwrap();
    assert!(known < section && section < images, "{text}");
    assert!(text.find("## Pending updates").unwrap() > images, "{text}");
    for line in [
        "These settings let updates merge on their own, and checks of this repository's CI, branch protection and delivery found a gap behind them.\n\n",
        "- **Nothing requires checks before GitHub merges.** `main` requires a review but no status checks. Open now: #412.\n",
        "- **Security fixes merge, but nothing ships them.** Fixes merge on their own, and no workflow runs when `main` changes. Open now: #412.\n",
        "<details><summary>Silenced (1)</summary>\n\n- `merge-without-ci`: \"CI runs in Buildkite, reporting to another org\" Open now: #412.\n</details>\n",
    ] {
        assert!(text[section..].contains(line), "{line}\n{text}");
    }
    assert_eq!(text.matches("## Tooling warnings").count(), 1);
    repo.tooling.clear();
    repo.tooling
        .push(warning("merge-without-ci", None, Some("Buildkite")));
    let text = dashboard::render(&repo, &[], &prs, &[], Recheck::NextRun, None);
    assert!(
        text.contains("\n## Tooling warnings\n\n<details><summary>Silenced (1)</summary>"),
        "{text}"
    );
    repo.tooling.clear();
    let text = dashboard::render(&repo, &[], &prs, &[], Recheck::NextRun, None);
    assert!(!text.contains("Tooling warnings"), "{text}");
}

/// With `prs.concurrent` on by default, the updates that wait for a slot are
/// on the dashboard, each with the limit and a box to open it now.
#[test]
fn updates_held_by_the_pr_limits_are_listed_with_their_reason_and_a_box() {
    let plan = |package: &str, hold: patchula_update::Hold| {
        let mut plan = patchula_update::testing::Draft::new(patchula_report::BranchAction {
            repository: "demo/app".into(),
            branch: format!("patchula/{package}"),
            base_commit: "a".repeat(40),
            status: patchula_report::BranchStatus::Planned,
            reason: None,
            message: format!("chore(deps): update {package} to 2.0.0"),
            previous_commit: None,
            commit: None,
            verified: None,
            lockfile_pending: false,
            files: Vec::new(),
        })
        .identity(package)
        .fingerprint(package)
        .input_fingerprint(package)
        .lockfiles_ready(true)
        .build();
        plan.hold(hold);
        plan
    };
    let plans = [
        plan("left-pad", patchula_update::Hold::Concurrent),
        plan("is-odd", patchula_update::Hold::PerHour),
    ];
    let text = dashboard::render(&repository(), &plans, &[], &[], Recheck::NextRun, None);
    let pending = &text[text.find("## Pending updates").unwrap()..];
    for line in [
        "Create it now anyway: chore(deps): update left-pad to 2.0.0 (prs.concurrent limit reached)",
        "Create it now anyway: chore(deps): update is-odd to 2.0.0 (prs.perHour limit reached)",
    ] {
        let row = pending
            .lines()
            .find(|l| l.contains(line))
            .unwrap_or_else(|| panic!("{line}: {text}"));
        assert!(row.starts_with("- [ ] "), "{row}");
    }
}

/// A newer version the Count looked up but cannot write into the declaration
/// is listed with why, not as a template that copier could apply.
#[test]
fn newer_versions_that_cannot_be_proposed_are_listed_with_their_reason() {
    let mut repo = repository();
    repo.notices = serde_json::from_value(json!([{
        "manager":"composer","path":"composer.json","line":4,"name":"acme/lib",
        "package":"acme/lib","current":"^1 | ^2","available":"3.0.0",
        "reason":"the constraint `^1 | ^2` cannot be rewritten to allow it without guessing"
    }]))
    .unwrap();
    let text = dashboard::render(&repo, &[], &[], &[], Recheck::NextRun, None);
    assert!(!text.contains("copier"), "{text}");
    let section = &text[text.find("## Newer versions not proposed").unwrap()..];
    assert!(
        section.contains("- acme/lib ^1 | ^2 → 3.0.0 available, not proposed: the constraint &#96;^1 | ^2&#96; cannot be rewritten to allow it without guessing in composer.json\n"),
        "{section}"
    );
}
