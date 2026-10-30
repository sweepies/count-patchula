//! Tooling warnings: updates set to merge on their own in a repository whose
//! CI, required checks or delivery don't back that up. Every warning needs
//! positive evidence; anything the Count can't read is unknown, and unknown
//! never warns.
use crate::{Plan, Snapshot, receipts::Inventory};
use globset::{GlobBuilder, GlobMatcher};
use patchula_config::Config;
use patchula_report::ToolingWarning;
use serde::Serialize;
use serde_json::Value;
use serde_yaml_ng::{Mapping, Value as Yaml};
use std::collections::BTreeMap;

pub const MERGE_WITHOUT_CI: &str = "merge-without-ci";
pub const MERGE_WITHOUT_REQUIRED_CHECKS: &str = "merge-without-required-checks";
pub const SECURITY_MERGE_WITHOUT_DELIVERY: &str = "security-merge-without-delivery";
pub const REVIEWS_ONLY: &str = "reviews-only";
pub const TAGGED_RELEASE: &str = "tagged-release";
/// GitHub's auto-merge, not the Count, merges the PR.
pub const NATIVE: &str = "native";

/// Most unread YAML files read to look for Flux and Argo CD manifests; a tree
/// with more leaves delivery unknown.
pub const MANIFEST_LIMIT: usize = 300;

/// CI systems the Count knows by their file alone.
const OTHER_CI: [&str; 6] = [
    ".gitlab-ci.yml",
    ".circleci/config.yml",
    "Jenkinsfile",
    "azure-pipelines.yml",
    ".travis.yml",
    ".buildkite/",
];

/// A warning shown on one PR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Callout {
    pub rule: &'static str,
    pub detail: Option<&'static str>,
    /// The default branch, named in the wording.
    pub branch: String,
}

/// Files whose contents the warnings read, beyond what the managers select.
pub fn reads(path: &str) -> bool {
    let yaml = path.ends_with(".yml") || path.ends_with(".yaml");
    yaml && (path
        .strip_prefix(".github/workflows/")
        .is_some_and(|name| !name.contains('/'))
        || woodpecker(path))
}
fn woodpecker(path: &str) -> bool {
    let stem = path
        .strip_suffix(".yml")
        .or_else(|| path.strip_suffix(".yaml"))
        .unwrap_or_default();
    stem == ".woodpecker"
        || stem
            .strip_prefix(".woodpecker/")
            .is_some_and(|name| !name.is_empty() && !name.contains('/'))
}

/// What the repository shows. `None` is unknown.
#[derive(Debug, Default)]
pub struct Evidence {
    /// The workflows and CI files that run on the Count's pull requests.
    pub ci: Option<Ci>,
    /// A check or status reported on one of the default branch's last 10 commits.
    pub branch_checks: Option<bool>,
    /// GitHub's auto-merge is allowed, so the Count uses it.
    pub native: bool,
    pub required: Option<Required>,
    pub delivery: Option<Delivery>,
}
/// What runs on a pull request.
#[derive(Clone, Debug, Default)]
pub struct Ci {
    /// Something runs on every pull request.
    pub always: bool,
    /// Workflows whose `paths` filters pick the pull requests they run on.
    pub filtered: Vec<Filter>,
}
impl Ci {
    fn runs(&self, files: &[&str]) -> bool {
        self.always || self.filtered.iter().any(|f| f.runs_for(files))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Required {
    pub checks: bool,
    pub reviews: bool,
}
/// What runs when the default branch changes.
#[derive(Clone, Debug, Default)]
pub struct Delivery {
    /// Something runs on every change.
    pub always: bool,
    /// Push workflows that run only for some files.
    pub filtered: Vec<Filter>,
    /// A workflow runs on tags or releases.
    pub tags: bool,
}
impl Delivery {
    fn ships(&self, files: &[&str]) -> bool {
        self.always || self.filtered.iter().any(|f| f.runs_for(files))
    }
}
#[derive(Clone, Debug, Default)]
pub struct Filter {
    paths: Option<Patterns>,
    ignore: Option<Patterns>,
}
impl Filter {
    fn runs_for(&self, files: &[&str]) -> bool {
        files.iter().any(|file| {
            self.paths.as_ref().is_none_or(|p| p.includes(file))
                && self.ignore.as_ref().is_none_or(|p| !p.includes(file))
        })
    }
}
/// GitHub's filter patterns: in order, a `!` pattern excludes what earlier
/// ones included.
#[derive(Clone, Debug)]
struct Patterns(Vec<(bool, GlobMatcher)>);
impl Patterns {
    fn parse(value: &Yaml) -> Result<Self, ()> {
        let items = match value {
            Yaml::String(s) => vec![s.as_str()],
            Yaml::Sequence(items) => items
                .iter()
                .map(|i| i.as_str().ok_or(()))
                .collect::<Result<_, _>>()?,
            _ => return Err(()),
        };
        items
            .into_iter()
            .map(|item| {
                let (negated, glob) = match item.strip_prefix('!') {
                    Some(rest) => (true, rest),
                    None => (false, item),
                };
                GlobBuilder::new(glob)
                    .literal_separator(true)
                    .build()
                    .map(|g| (negated, g.compile_matcher()))
                    .map_err(|_| ())
            })
            .collect::<Result<_, _>>()
            .map(Self)
    }
    fn includes(&self, value: &str) -> bool {
        let mut included = false;
        for (negated, glob) in &self.0 {
            if glob.is_match(value) {
                included = !negated;
            }
        }
        included
    }
}

/// One GitHub Actions workflow's triggers that matter here.
#[derive(Default)]
struct Workflow {
    name: String,
    /// How a pull request or a push to the Count's branch runs it.
    pull_requests: Vec<Filter>,
    /// How a push to the default branch runs it.
    push: Option<Filter>,
    tags: bool,
    schedule: bool,
    /// Workflows whose completion triggers this one.
    after: Vec<String>,
}

/// YAML 1.1 readers turn an `on` key into `true`; accept both.
fn key<'a>(map: &'a Mapping, name: &str) -> Option<&'a Yaml> {
    map.iter()
        .find(|(k, _)| k.as_str() == Some(name) || (name == "on" && k.as_bool() == Some(true)))
        .map(|(_, v)| v)
}

fn workflow(path: &str, text: &str, branch: &str) -> Result<Workflow, ()> {
    let document: Yaml = serde_yaml_ng::from_str(text).map_err(|_| ())?;
    let map = document.as_mapping().ok_or(())?;
    let mut result = Workflow {
        name: key(map, "name")
            .and_then(Yaml::as_str)
            .unwrap_or(path)
            .to_owned(),
        ..Default::default()
    };
    let events: Vec<(&str, Option<&Mapping>)> = match key(map, "on").ok_or(())? {
        Yaml::String(event) => vec![(event.as_str(), None)],
        Yaml::Sequence(events) => events
            .iter()
            .map(|e| e.as_str().map(|e| (e, None)).ok_or(()))
            .collect::<Result<_, _>>()?,
        Yaml::Mapping(events) => events
            .iter()
            .map(|(k, v)| {
                let config = match v {
                    Yaml::Mapping(m) => Some(m),
                    _ => None,
                };
                k.as_str().map(|k| (k, config)).ok_or(())
            })
            .collect::<Result<_, _>>()?,
        _ => return Err(()),
    };
    for (event, config) in events {
        let filter = |name: &str| {
            config
                .and_then(|c| key(c, name))
                .map(Patterns::parse)
                .transpose()
        };
        match event {
            // A pull request's workflows run for its base branch and its files.
            "pull_request" | "pull_request_target" => {
                let branches = filter("branches")?;
                let ignored = filter("branches-ignore")?;
                let paths = filter("paths")?;
                let paths_ignore = filter("paths-ignore")?;
                if branches.is_none_or(|b| b.includes(branch))
                    && ignored.is_none_or(|i| !i.includes(branch))
                {
                    result.pull_requests.push(Filter {
                        paths,
                        ignore: paths_ignore,
                    });
                }
            }
            "merge_group" => result.pull_requests.push(Filter::default()),
            "schedule" => result.schedule = true,
            "release" => result.tags = true,
            "workflow_run" => match config.and_then(|c| key(c, "workflows")) {
                Some(Yaml::String(name)) => result.after.push(name.clone()),
                Some(Yaml::Sequence(names)) => {
                    for name in names {
                        result.after.push(name.as_str().ok_or(())?.to_owned());
                    }
                }
                Some(_) => return Err(()),
                None => {}
            },
            "push" => {
                let branches = filter("branches")?;
                let branches_ignore = filter("branches-ignore")?;
                let tag_filter = config
                    .is_some_and(|c| key(c, "tags").is_some() || key(c, "tags-ignore").is_some());
                let branch_filter = branches.is_some() || branches_ignore.is_some();
                result.tags |= tag_filter || !branch_filter;
                let paths = Filter {
                    paths: filter("paths")?,
                    ignore: filter("paths-ignore")?,
                };
                // A push to the Count's branch runs it, so it reports on his PRs.
                if branches_ignore.is_some() || (!branch_filter && !tag_filter) {
                    result.pull_requests.push(paths.clone());
                }
                let runs = match (&branches, &branches_ignore) {
                    (Some(b), _) => b.includes(branch),
                    (None, Some(ignore)) => !ignore.includes(branch),
                    (None, None) => !tag_filter,
                };
                if runs {
                    result.push = Some(paths);
                }
            }
            _ => {}
        }
    }
    Ok(result)
}

/// Whether a Woodpecker pipeline runs on pull requests, and on pushes to the
/// default branch.
fn pipeline(text: &str, branch: &str) -> Result<(bool, bool), ()> {
    let document: Yaml = serde_yaml_ng::from_str(text).map_err(|_| ())?;
    let map = document.as_mapping().ok_or(())?;
    let conditions = match key(map, "when") {
        None => return Ok((true, true)),
        Some(Yaml::Sequence(items)) => items.iter().collect(),
        Some(when @ Yaml::Mapping(_)) => vec![when],
        Some(_) => return Err(()),
    };
    let mut result = (false, false);
    for condition in conditions {
        let condition = condition.as_mapping().ok_or(())?;
        let events: Option<Vec<&str>> = match key(condition, "event") {
            None => None,
            Some(Yaml::String(e)) => Some(vec![e.as_str()]),
            Some(Yaml::Sequence(items)) => Some(
                items
                    .iter()
                    .map(|i| i.as_str().ok_or(()))
                    .collect::<Result<_, _>>()?,
            ),
            Some(_) => return Err(()),
        };
        let on = |event: &str| events.as_ref().is_none_or(|e| e.contains(&event));
        let matches = match key(condition, "branch") {
            None => true,
            Some(Yaml::Mapping(m)) => {
                key(m, "include")
                    .map(Patterns::parse)
                    .transpose()?
                    .is_none_or(|p| p.includes(branch))
                    && key(m, "exclude")
                        .map(Patterns::parse)
                        .transpose()?
                        .is_none_or(|p| !p.includes(branch))
            }
            Some(value) => Patterns::parse(value)?.includes(branch),
        };
        result.0 |= on("pull_request");
        result.1 |= on("push") && matches;
    }
    Ok(result)
}

impl Evidence {
    pub fn read(snapshot: &Snapshot, branch: &str, inventory: &Inventory) -> Self {
        let (ci, delivery) = match files(snapshot, branch) {
            Ok((ci, delivery)) => (Some(ci), delivery),
            Err(()) => (None, None),
        };
        let head = &inventory.default_branch;
        let branch_checks = head["target"]["history"]["nodes"]
            .as_array()
            .map(|commits| commits.iter().any(|c| !c["statusCheckRollup"].is_null()));
        Self {
            ci,
            branch_checks,
            native: inventory.merge_settings.native,
            required: required(head),
            delivery,
        }
    }
}

/// Required checks and reviews from active rulesets and classic protection.
fn required(head: &Value) -> Option<Required> {
    let rules = &head["rules"];
    let nodes = rules["nodes"].as_array()?;
    let mut result = Required {
        checks: nodes.iter().any(|r| {
            r["type"] == "REQUIRED_STATUS_CHECKS"
                && r["parameters"]["requiredStatusChecks"]
                    .as_array()
                    .is_some_and(|c| !c.is_empty())
        }),
        reviews: nodes.iter().any(|r| {
            r["type"] == "PULL_REQUEST"
                && r["parameters"]["requiredApprovingReviewCount"]
                    .as_u64()
                    .is_some_and(|n| n > 0)
        }),
    };
    let classic = &head["refUpdateRule"];
    result.checks |= classic["requiredStatusCheckContexts"]
        .as_array()
        .is_some_and(|c| !c.is_empty());
    result.reviews |= classic["requiredApprovingReviewCount"]
        .as_u64()
        .is_some_and(|n| n > 0);
    if !result.checks && rules["pageInfo"]["hasNextPage"].as_bool() != Some(false) {
        return None;
    }
    Some(result)
}

/// A file that may declare Flux or Argo CD resources.
pub fn manifest(path: &str) -> bool {
    path.ends_with(".yml") || path.ends_with(".yaml")
}

/// CI and delivery from the tree: workflows, Woodpecker pipelines, other CI
/// files and GitOps manifests. An unreadable workflow makes both unknown, and
/// so does a YAML file that was never fetched, unless something else already
/// ships every change: it could be a manifest.
fn files(snapshot: &Snapshot, branch: &str) -> Result<(Ci, Option<Delivery>), ()> {
    let mut ci = Ci::default();
    let mut delivery = Delivery::default();
    let mut workflows = Vec::new();
    for path in snapshot.paths.iter().filter(|p| reads(p)) {
        let text = &snapshot.files.get(path).ok_or(())?.content;
        if woodpecker(path) {
            let (pull_requests, push) = pipeline(text, branch)?;
            ci.always |= pull_requests;
            delivery.always |= push;
        } else {
            workflows.push(workflow(path, text, branch)?);
        }
    }
    let by_name: BTreeMap<_, _> = workflows.iter().map(|w| (w.name.as_str(), w)).collect();
    for w in &workflows {
        for filter in &w.pull_requests {
            if filter.paths.is_none() && filter.ignore.is_none() {
                ci.always = true;
            } else {
                ci.filtered.push(filter.clone());
            }
        }
        delivery.tags |= w.tags;
        delivery.always |= w.schedule;
        let triggers = w
            .after
            .iter()
            .filter_map(|name| by_name.get(name.as_str()).copied());
        for filter in std::iter::once(w)
            .chain(triggers)
            .filter_map(|w| w.push.as_ref())
        {
            if filter.paths.is_none() && filter.ignore.is_none() {
                delivery.always = true;
            } else {
                delivery.filtered.push(filter.clone());
            }
        }
    }
    if snapshot.paths.iter().any(|p| {
        OTHER_CI
            .iter()
            .any(|ci| p == ci || (ci.ends_with('/') && p.starts_with(ci)))
    }) {
        ci.always = true;
        delivery.always = true;
    }
    delivery.always |= snapshot.files.values().any(|blob| {
        let text = &blob.content;
        (text.contains("fluxcd.io/")
            && (text.contains("kind: Kustomization") || text.contains("kind: HelmRelease")))
            || (text.contains("argoproj.io/")
                && (text.contains("kind: Application") || text.contains("kind: ApplicationSet")))
    });
    let unread = snapshot
        .paths
        .iter()
        .any(|p| manifest(p) && !snapshot.files.contains_key(p));
    Ok((ci, (delivery.always || !unread).then_some(delivery)))
}

/// Puts each holding warning on the PRs it applies to, and returns one entry
/// per rule for the report and dashboard. Silenced rules stay off PRs.
pub fn apply(
    evidence: &Evidence,
    config: &Config,
    branch: &str,
    plans: &mut [Plan],
) -> Vec<ToolingWarning> {
    let mut warnings = BTreeMap::<&str, ToolingWarning>::new();
    for plan in plans.iter_mut() {
        plan.tooling.clear();
        if !crate::automerge::requested(plan, config) {
            continue;
        }
        let files: Vec<&str> = plan
            .action
            .files
            .iter()
            .map(|f| f.path.as_str())
            .chain(plan.changes.iter().map(|c| c.file.as_str()))
            .collect();
        let mut holding = Vec::new();
        if evidence.branch_checks == Some(false)
            && evidence.ci.as_ref().is_some_and(|ci| !ci.runs(&files))
        {
            holding.push((MERGE_WITHOUT_CI, evidence.native.then_some(NATIVE)));
        }
        if let Some(required) = evidence.required.filter(|r| evidence.native && !r.checks) {
            holding.push((
                MERGE_WITHOUT_REQUIRED_CHECKS,
                required.reviews.then_some(REVIEWS_ONLY),
            ));
        }
        if let Some(delivery) = &evidence.delivery
            && evidence.branch_checks == Some(false)
            && plan.changes.iter().any(|c| !c.advisories.is_empty())
            && !delivery.ships(&files)
        {
            holding.push((
                SECURITY_MERGE_WITHOUT_DELIVERY,
                delivery.tags.then_some(TAGGED_RELEASE),
            ));
        }
        for (rule, detail) in holding {
            let silenced = config.warnings.quiet.get(rule).cloned();
            let entry = warnings.entry(rule).or_insert_with(|| ToolingWarning {
                rule: rule.into(),
                detail: detail.map(str::to_owned),
                branches: Vec::new(),
                silenced: silenced.clone(),
            });
            entry.branches.push(plan.action.branch.clone());
            if silenced.is_none() {
                plan.tooling.push(Callout {
                    rule,
                    detail,
                    branch: branch.into(),
                });
            }
        }
    }
    warnings.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot(files: &[(&str, &str)]) -> Snapshot {
        Snapshot {
            paths: files.iter().map(|(p, _)| p.to_string()).collect(),
            files: files
                .iter()
                .map(|(p, c)| {
                    (
                        p.to_string(),
                        crate::Blob {
                            sha: String::new(),
                            mode: "100644".into(),
                            content: c.to_string(),
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn push_filters_follow_github_semantics() {
        let w = workflow(
            "ci.yml",
            "on:\n  push:\n    branches: [main]\n    paths: ['src/**', '!src/docs/**']\n",
            "main",
        )
        .unwrap();
        let push = w.push.unwrap();
        assert!(push.runs_for(&["src/lib.rs"]));
        assert!(!push.runs_for(&["src/docs/a.md", "README.md"]));
        assert!(w.pull_requests.is_empty() && !w.tags);
        let w = workflow(
            "ci.yml",
            "on:\n  push:\n    branches-ignore: [main]\n",
            "main",
        )
        .unwrap();
        assert!(w.push.is_none() && w.pull_requests.len() == 1);
        let w = workflow("release.yml", "on:\n  push:\n    tags: ['v*']\n", "main").unwrap();
        assert!(w.push.is_none() && w.tags);
        let w = workflow("ci.yml", "on: [push, pull_request]\n", "main").unwrap();
        assert!(w.push.is_some() && w.pull_requests.len() == 2 && w.tags);
        let w = workflow(
            "ci.yml",
            "on:\n  schedule:\n    - cron: '0 0 * * *'\n",
            "main",
        )
        .unwrap();
        assert!(w.schedule);
        assert!(workflow("ci.yml", "jobs: {}\n", "main").is_err());
    }

    #[test]
    fn delivery_counts_workflow_runs_schedules_gitops_and_other_ci() {
        let (ci, delivery) = files(
            &snapshot(&[
                (
                    ".github/workflows/build.yml",
                    "name: Build\non:\n  push:\n    branches: [main]\n    paths: [app/**]\n",
                ),
                (
                    ".github/workflows/deploy.yml",
                    "on:\n  workflow_run:\n    workflows: [Build]\n",
                ),
            ]),
            "main",
        )
        .unwrap();
        let delivery = delivery.unwrap();
        assert!(!ci.always && ci.filtered.is_empty() && !delivery.always);
        assert_eq!(delivery.filtered.len(), 2);
        assert!(delivery.ships(&["app/main.go"]));
        assert!(!delivery.ships(&["go.mod"]));
        for extra in [
            (
                ".github/workflows/nightly.yml",
                "on:\n  schedule:\n    - cron: '0 0 * * *'\n",
            ),
            (".gitlab-ci.yml", "build: {}\n"),
            (
                "ops/app.yaml",
                "apiVersion: argoproj.io/v1alpha1\nkind: Application\n",
            ),
            (".woodpecker.yml", "steps: []\n"),
        ] {
            let (_, delivery) = files(&snapshot(&[extra]), "main").unwrap();
            assert!(delivery.unwrap().always, "{}", extra.0);
        }
        let (ci, delivery) = files(
            &snapshot(&[(".woodpecker/ci.yml", "when:\n  event: pull_request\n")]),
            "main",
        )
        .unwrap();
        assert!(ci.always && !delivery.unwrap().always);
    }

    #[test]
    fn pull_request_workflows_count_only_for_their_branch_and_files() {
        let on = |text: &str| workflow("ci.yml", text, "main").unwrap().pull_requests;
        assert!(on("on:\n  pull_request:\n    branches: [release]\n").is_empty());
        assert!(on("on:\n  pull_request:\n    branches-ignore: [main]\n").is_empty());
        assert_eq!(on("on:\n  pull_request:\n    branches: [main]\n").len(), 1);
        let filtered = on("on:\n  pull_request:\n    paths: ['src/**']\n");
        assert!(filtered[0].runs_for(&["src/lib.rs"]));
        assert!(!filtered[0].runs_for(&["docs/a.md"]));
        let (ci, _) = files(
            &snapshot(&[(
                ".github/workflows/ci.yml",
                "on:\n  pull_request:\n    paths-ignore: ['**.md']\n",
            )]),
            "main",
        )
        .unwrap();
        assert!(!ci.always && ci.runs(&["src/lib.rs"]) && !ci.runs(&["README.md"]));
        let (ci, _) = files(
            &snapshot(&[(
                ".github/workflows/ci.yml",
                "on:\n  pull_request:\n    branches: [release]\n",
            )]),
            "main",
        )
        .unwrap();
        assert!(!ci.runs(&["src/lib.rs"]));
    }

    #[test]
    fn an_unread_yaml_file_leaves_delivery_unknown_unless_something_ships() {
        let mut tree = snapshot(&[(".github/workflows/ci.yml", "on: [pull_request]\n")]);
        tree.paths.insert("clusters/prod.yaml".into());
        assert!(files(&tree, "main").unwrap().1.is_none());
        tree.paths.insert(".gitlab-ci.yml".into());
        assert!(files(&tree, "main").unwrap().1.unwrap().always);
    }

    #[test]
    fn an_unreadable_workflow_makes_ci_and_delivery_unknown() {
        let broken = snapshot(&[(".github/workflows/ci.yml", "on: [push\n")]);
        assert!(files(&broken, "main").is_err());
        let mut unfetched = snapshot(&[]);
        unfetched.paths.insert(".github/workflows/ci.yml".into());
        assert!(files(&unfetched, "main").is_err());
    }

    #[test]
    fn required_checks_come_from_rulesets_or_classic_protection() {
        let head = |nodes: Value, more: bool, classic: Value| json!({"rules": {"nodes": nodes, "pageInfo": {"hasNextPage": more}}, "refUpdateRule": classic});
        let none = Required {
            checks: false,
            reviews: false,
        };
        assert_eq!(required(&json!({"rules": null})), None);
        assert_eq!(
            required(&head(json!([{"type":"DELETION"}]), false, Value::Null)),
            Some(none)
        );
        assert_eq!(required(&head(json!([]), true, Value::Null)), None);
        assert_eq!(
            required(&head(
                json!([{"type":"PULL_REQUEST","parameters":{"requiredApprovingReviewCount":1}}]),
                false,
                Value::Null
            )),
            Some(Required {
                reviews: true,
                ..none
            })
        );
        assert_eq!(
            required(&head(
                json!([]),
                true,
                json!({"requiredStatusCheckContexts":["test"]})
            )),
            Some(Required {
                checks: true,
                ..none
            })
        );
        assert_eq!(
            required(&head(
                json!([{"type":"REQUIRED_STATUS_CHECKS","parameters":{"requiredStatusChecks":[{"context":"ci"}]}}]),
                false,
                Value::Null
            )),
            Some(Required {
                checks: true,
                ..none
            })
        );
    }
    fn plan(branch: &str, advisory: bool) -> Plan {
        Plan {
            action: patchula_report::BranchAction {
                repository: "demo/app".into(),
                branch: branch.into(),
                base_commit: "base".into(),
                status: patchula_report::ActionStatus::Planned,
                reason: None,
                message: "chore(deps): update".into(),
                previous_commit: None,
                commit: None,
                verified: None,
                lockfile_pending: false,
                files: Vec::new(),
            },
            contents: Default::default(),
            identity: branch.into(),
            fingerprint: String::new(),
            changes: vec![crate::Change {
                source: "npm".into(),
                package: "lodash".into(),
                file: "app/package.json".into(),
                current: "4.17.20".into(),
                current_version: Some("4.17.20".into()),
                value: Some("4.17.21".into()),
                version: Some("4.17.21".into()),
                digest: None,
                kind: "patch".into(),
                merge: true,
                labels: Vec::new(),
                release_notes: false,
                body_notes: Vec::new(),
                advisories: advisory
                    .then(|| patchula_report::Advisory {
                        id: "GHSA-jf85-cpcp-j695".into(),
                        aliases: Vec::new(),
                        severity: None,
                        summary: None,
                        url: "https://osv.dev/vulnerability/GHSA-jf85-cpcp-j695".into(),
                        fixed_in: None,
                    })
                    .into_iter()
                    .collect(),
                image: None,
            }],
            retry: false,
            tree: None,
            cached_pr: None,
            input_fingerprint: String::new(),
            lock_jobs: Vec::new(),
            lockfiles_ready: true,
            deleted: Default::default(),
            maintenance: false,
            batch: None,
            tooling: Vec::new(),
        }
    }

    fn config() -> Config {
        patchula_config::load(std::path::Path::new("../../PatchulaConfig.pkl")).unwrap()
    }

    /// No CI anywhere, nothing required, GitHub's auto-merge on: every rule holds.
    fn bare() -> Evidence {
        Evidence {
            ci: Some(Ci::default()),
            branch_checks: Some(false),
            native: true,
            required: Some(Required {
                checks: false,
                reviews: false,
            }),
            delivery: Some(Delivery::default()),
        }
    }

    fn rules(plan: &Plan) -> Vec<(&'static str, Option<&'static str>)> {
        plan.tooling.iter().map(|c| (c.rule, c.detail)).collect()
    }

    #[test]
    fn each_rule_warns_on_positive_evidence_for_updates_that_merge() {
        let mut plans = vec![plan("patchula/fix", true), plan("patchula/plain", false)];
        let warnings = apply(&bare(), &config(), "main", &mut plans);
        assert_eq!(
            rules(&plans[0]),
            [
                (MERGE_WITHOUT_CI, Some(NATIVE)),
                (MERGE_WITHOUT_REQUIRED_CHECKS, None),
                (SECURITY_MERGE_WITHOUT_DELIVERY, None)
            ]
        );
        assert_eq!(
            rules(&plans[1]),
            [
                (MERGE_WITHOUT_CI, Some(NATIVE)),
                (MERGE_WITHOUT_REQUIRED_CHECKS, None)
            ]
        );
        assert_eq!(plans[0].tooling[0].branch, "main");
        assert_eq!(warnings.len(), 3);
        assert_eq!(warnings[0].rule, MERGE_WITHOUT_CI);
        assert_eq!(warnings[0].branches, ["patchula/fix", "patchula/plain"]);
        assert_eq!(warnings[2].branches, ["patchula/fix"]);
    }

    #[test]
    fn nothing_warns_for_updates_that_wait_for_a_person() {
        let mut plans = vec![plan("patchula/fix", true)];
        plans[0].changes[0].merge = false;
        assert!(apply(&bare(), &config(), "main", &mut plans).is_empty());
        assert!(plans[0].tooling.is_empty());
    }

    #[test]
    fn unknown_evidence_never_warns() {
        for unknown in [
            Evidence {
                ci: None,
                delivery: None,
                required: None,
                ..bare()
            },
            Evidence {
                branch_checks: None,
                required: None,
                ..bare()
            },
        ] {
            let mut plans = vec![plan("patchula/fix", true)];
            assert!(apply(&unknown, &config(), "main", &mut plans).is_empty());
            assert!(plans[0].tooling.is_empty());
        }
    }

    #[test]
    fn present_tooling_clears_each_warning() {
        let mut plans = vec![plan("patchula/fix", true)];
        let evidence = Evidence {
            ci: Some(Ci {
                always: true,
                ..Default::default()
            }),
            required: Some(Required {
                checks: true,
                reviews: false,
            }),
            delivery: Some(Delivery {
                filtered: vec![Filter {
                    paths: Some(Patterns::parse(&Yaml::String("app/**".into())).unwrap()),
                    ignore: None,
                }],
                ..Default::default()
            }),
            ..bare()
        };
        assert!(apply(&evidence, &config(), "main", &mut plans).is_empty());
        let reported = Evidence {
            branch_checks: Some(true),
            ..bare()
        };
        assert_eq!(
            rules(&{
                let mut plans = vec![plan("patchula/fix", true)];
                apply(&reported, &config(), "main", &mut plans);
                plans.remove(0)
            }),
            [(MERGE_WITHOUT_REQUIRED_CHECKS, None)]
        );
        let native_off = Evidence {
            native: false,
            ..bare()
        };
        let mut plans = vec![plan("patchula/fix", false)];
        apply(&native_off, &config(), "main", &mut plans);
        assert_eq!(rules(&plans[0]), [(MERGE_WITHOUT_CI, None)]);
    }

    #[test]
    fn narrower_cases_change_the_wording_and_quiet_rules_stay_off_prs() {
        let evidence = Evidence {
            ci: Some(Ci {
                always: true,
                ..Default::default()
            }),
            required: Some(Required {
                checks: false,
                reviews: true,
            }),
            delivery: Some(Delivery {
                tags: true,
                ..Default::default()
            }),
            ..bare()
        };
        let mut config = config();
        config.warnings.quiet.insert(
            MERGE_WITHOUT_REQUIRED_CHECKS.into(),
            "reviews gate every merge".into(),
        );
        let mut plans = vec![plan("patchula/fix", true)];
        let warnings = apply(&evidence, &config, "main", &mut plans);
        assert_eq!(
            rules(&plans[0]),
            [(SECURITY_MERGE_WITHOUT_DELIVERY, Some(TAGGED_RELEASE))]
        );
        assert_eq!(warnings[0].rule, MERGE_WITHOUT_REQUIRED_CHECKS);
        assert_eq!(warnings[0].detail.as_deref(), Some(REVIEWS_ONLY));
        assert_eq!(
            warnings[0].silenced.as_deref(),
            Some("reviews gate every merge")
        );
        assert_eq!(warnings[1].silenced, None);
    }

    #[test]
    fn checks_and_protection_come_from_the_inventory() {
        let inventory = Inventory {
            default_branch: json!({
                "target": {"history": {"nodes": [{"statusCheckRollup": null}]}},
                "rules": {"nodes": [], "pageInfo": {"hasNextPage": false}},
                "refUpdateRule": null
            }),
            ..Default::default()
        };
        let evidence = Evidence::read(&snapshot(&[]), "main", &inventory);
        assert_eq!(evidence.branch_checks, Some(false));
        assert!(!evidence.ci.unwrap().always);
        assert_eq!(
            evidence.required,
            Some(Required {
                checks: false,
                reviews: false
            })
        );
        let unread = Evidence::read(&snapshot(&[]), "main", &Inventory::default());
        assert_eq!((unread.branch_checks, unread.required), (None, None));
    }
}
