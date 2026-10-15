use rustovate_report::Repository;
use rustovate_update::{
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
fn dashboard_snapshot_and_identity_do_not_depend_on_wording() {
    let repo = repository();
    let dep = &repo.package_files[0].dependencies[0];
    let text = dashboard::render(&repo, &[], &[], Recheck::NextRun);
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
        dashboard::render(&repo, &[], &[], Recheck::Now),
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
fn issue_discovery_checks_marker_author_and_ambiguity_never_title() {
    let mut inventory = Inventory {
        actor: "rustovate[bot]".into(),
        ..Default::default()
    };
    inventory.issues = vec![
        json!({"number":1,"title":"Dependency Dashboard","body":MARKER,"author":{"__typename":"User","login":"human"}}),
    ];
    assert!(dashboard::issue(&inventory).unwrap().is_none());
    inventory.issues.push(json!({"number":2,"title":"Renamed by owner","body":MARKER,"author":{"__typename":"Bot","login":"rustovate"}}));
    assert_eq!(dashboard::issue(&inventory).unwrap().unwrap()["number"], 2);
    inventory.issues.push(inventory.issues[1].clone());
    assert!(dashboard::issue(&inventory).is_err());
}
