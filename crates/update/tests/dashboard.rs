use patchula_report::Repository;
use patchula_update::{
    Inventory, Recheck,
    dashboard::{self, MARKER},
};
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
        dashboard::render(&repo, &[], &[], Recheck::NextRun, None)
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
        .replace(&dashboard::retry_key("npm", "fixture", "npm"), "RETRY")
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
    let text = dashboard::render(&repo, &[], &[], Recheck::NextRun, None);
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
            rule: rule.into(),
            detail: detail.map(str::to_owned),
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
        json!({"number":412,"state":"open","title":"Update fixture","head":{"ref":"patchula/fixture"}}),
        json!({"number":7,"state":"closed","merged_at":null,"title":"Old","head":{"ref":"patchula/other"}}),
    ];
    let text = dashboard::render(&repo, &[], &prs, Recheck::NextRun, None);
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
    let text = dashboard::render(&repo, &[], &prs, Recheck::NextRun, None);
    assert!(
        text.contains("\n## Tooling warnings\n\n<details><summary>Silenced (1)</summary>"),
        "{text}"
    );
    repo.tooling.clear();
    let text = dashboard::render(&repo, &[], &prs, Recheck::NextRun, None);
    assert!(!text.contains("Tooling warnings"), "{text}");
}

/// With `prs.concurrent` on by default, the updates that wait for a slot are
/// on the dashboard, each with the limit and a box to open it now.
#[test]
fn updates_held_by_the_pr_limits_are_listed_with_their_reason_and_a_box() {
    let plan = |package: &str, reason: &str| patchula_update::Plan {
        action: patchula_report::BranchAction {
            repository: "demo/app".into(),
            branch: format!("patchula/{package}"),
            base_commit: "a".repeat(40),
            status: patchula_report::ActionStatus::Skipped,
            reason: Some(reason.into()),
            message: format!("chore(deps): update {package} to 2.0.0"),
            previous_commit: None,
            commit: None,
            verified: None,
            lockfile_pending: false,
            files: Vec::new(),
        },
        contents: Default::default(),
        identity: package.into(),
        fingerprint: package.into(),
        changes: Vec::new(),
        retry: false,
        tree: None,
        cached_pr: None,
        input_fingerprint: package.into(),
        lock_jobs: Vec::new(),
        lockfiles_ready: true,
        deleted: Default::default(),
        maintenance: false,
        batch: None,
        tooling: Vec::new(),
    };
    let plans = [
        plan("left-pad", "prs.concurrent limit reached"),
        plan("is-odd", "prs.perHour limit reached"),
    ];
    let text = dashboard::render(&repository(), &plans, &[], Recheck::NextRun, None);
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
    let text = dashboard::render(&repo, &[], &[], Recheck::NextRun, None);
    assert!(!text.contains("copier"), "{text}");
    let section = &text[text.find("## Newer versions not proposed").unwrap()..];
    assert!(
        section.contains("- acme/lib ^1 | ^2 → 3.0.0 available, not proposed: the constraint &#96;^1 | ^2&#96; cannot be rewritten to allow it without guessing in composer.json\n"),
        "{section}"
    );
}
