//! The batch PR collects updates whose "Add to batch" box was ticked. Each
//! keeps the targets that were reviewed; the individual PR closes as batched,
//! which never suppresses the update. Unticking, removing or closing the batch
//! returns its updates to individual PRs.
use crate::error::PlanError;
use crate::markers;
use crate::{Change, Plan, Snapshot, pull_requests};
use patchula_config::{Config, Registry};
use patchula_manager::Dependency;
use patchula_report::Repository;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    /// The update key of the PR the item came from.
    pub origin: String,
    pub number: Option<u64>,
    pub title: String,
    pub targets: Vec<Change>,
}

fn overlaps(item: &Item, targets: &[Change]) -> bool {
    item.targets.iter().any(|a| {
        targets.iter().any(|b| {
            a.touches(
                &b.file,
                b.source,
                &b.package,
                b.registry.as_ref().map(Registry::recorded),
            )
        })
    })
}

/// Current items: the open batch PR's items without removed ones, plus every
/// open update PR whose "Add to batch" box is ticked. A newly added update
/// replaces an item for the same dependency, so its newer target wins.
pub fn items(
    prs: &[pull_requests::Tracked],
    actor: &str,
    repo: &str,
    batch_branch: &str,
) -> Vec<Item> {
    let open: Vec<_> = prs
        .iter()
        .filter(|t| t.pr.state == "open" && pull_requests::owned_pr(&t.pr, actor, repo))
        .collect();
    let batch = open.iter().find(|t| t.pr.head.branch == batch_branch);
    let mut items: Vec<Item> = batch
        .and_then(|t| {
            markers::section_json::<Vec<Item>>(
                pull_requests::body(&t.pr),
                markers::ITEMS,
                markers::PAYLOAD,
            )
        })
        .unwrap_or_default();
    if let Some(t) = batch {
        let removed: BTreeSet<_> = markers::ticked_lines(pull_requests::body(&t.pr))
            .into_iter()
            .filter_map(|line| {
                markers::line_end(line, markers::REMOVE, usize::MAX).map(str::to_owned)
            })
            .collect();
        items.retain(|item| !removed.contains(&item.origin));
    }
    for t in &open {
        if t.pr.head.branch == batch_branch
            || !markers::ticked_lines(pull_requests::body(&t.pr))
                .iter()
                .any(|line| line.ends_with(markers::ADD))
        {
            continue;
        }
        let (Some(origin), Some(targets)) = (
            pull_requests::marker_update(pull_requests::body(&t.pr)),
            markers::section_json::<Vec<Change>>(
                pull_requests::body(&t.pr),
                markers::TARGETS,
                markers::PAYLOAD,
            ),
        ) else {
            continue;
        };
        if targets.is_empty() {
            continue;
        }
        items.retain(|item| item.origin != origin && !overlaps(item, &targets));
        items.push(Item {
            origin,
            number: Some(t.pr.number),
            title: t.pr.title.clone(),
            targets,
        });
    }
    items.sort();
    items
}

/// Add the batch plan and skip the update plans it carries. Items whose targets the
/// base already satisfies are complete and leave the batch.
#[allow(clippy::too_many_arguments)]
pub fn apply(
    plans: &mut Vec<Plan>,
    repository: &Repository,
    config: &Config,
    dependencies: &[Dependency],
    snapshot: &Snapshot,
    prs: &[pull_requests::Tracked],
    actor: &str,
    allow_pending: bool,
    since: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<BTreeSet<String>, PlanError> {
    let name = crate::branch_name(&config.prs.branch, "batch")?;
    let items = items(prs, actor, &repository.repository, &name);
    let Some(plan) = crate::plan::batch_plan(
        repository,
        config,
        dependencies,
        snapshot,
        items,
        allow_pending,
        since,
        now,
    )?
    else {
        return Ok(BTreeSet::new());
    };
    if plans
        .iter()
        .any(|p| p.action().branch == plan.action().branch)
    {
        return Err(PlanError::BatchReserved(plan.action().branch.clone()));
    }
    let origins: BTreeSet<_> = plan
        .batch_items()
        .unwrap_or_default()
        .iter()
        .map(|i| i.origin.clone())
        .collect();
    // Batched plans stay desired so cleanup keeps their branches until their
    // PRs close as batched; a removed item resumes on the same branch.
    for p in plans
        .iter_mut()
        .filter(|p| origins.contains(&p.update_key()))
    {
        p.hold(crate::plan::Hold::Batched);
    }
    plans.push(plan);
    Ok(origins)
}

/// The batch section of the batch PR body, with a removal box per item.
pub(crate) fn render(items: &[Item]) -> String {
    let mut text = String::from(
        "\nTick an item to send it back to its own pull request; closing this pull request sends every item back.\n\n",
    );
    for item in items {
        let title = crate::text::inert(&item.title.replace(['\n', '\r', '<', '>', '`'], " "));
        let from = item
            .number
            .map(|n| format!(" (from #{n})"))
            .unwrap_or_default();
        text.push_str(&format!(
            "- [ ] {title}{from} {}{} -->\n",
            markers::REMOVE,
            item.origin
        ));
    }
    text.push('\n');
    text.push_str(&markers::hidden(markers::ITEMS, &items));
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn change(package: &str, value: &str) -> Change {
        Change {
            registry: None,
            source: patchula_words::Source::Npm,
            package: package.into(),
            file: "package.json".into(),
            current: "1.0.0".into(),
            current_version: None,
            value: Some(value.into()),
            version: Some(value.into()),
            digest: None,
            kind: patchula_words::Update::Minor,
            merge: false,
            labels: Vec::new(),
            release_notes: true,
            body_notes: Vec::new(),
            advisories: Vec::new(),
            image: None,
        }
    }
    fn pr(number: u64, branch: &str, body: String) -> pull_requests::Tracked {
        pull_requests::Tracked::from(
            serde_json::from_value::<patchula_github::model::PullRequest>(
                json!({"number":number,"node_id":format!("N{number}"),
                "html_url":"https://github.com/o/r/pull/1",
                "title":format!("update {number}"),"state":"open","body":body,
                "labels":[],"user":{"login":"bot"},
                "head":{"ref":branch,"sha":"head","repo":{"full_name":"o/r"}},
                "base":{"ref":"main","repo":{"full_name":"o/r"}}}),
            )
            .expect("a complete REST pull request"),
        )
    }
    fn update_body(key: &str, targets: &[Change], ticked: bool) -> String {
        format!(
            "{}\n- [{}] Add to the batch PR. This one closes and the update moves there. {}\n<!-- patchula:{{\"version\":1,\"identity\":\"{}\",\"update\":\"{key}\"}} -->\n",
            markers::hidden(markers::TARGETS, &targets),
            if ticked { "x" } else { " " },
            markers::ADD,
            "a".repeat(64)
        )
    }

    #[test]
    fn ticked_updates_join_and_newer_targets_replace_items() {
        let (k1, k2, k3) = ("1".repeat(64), "2".repeat(64), "3".repeat(64));
        let mut prs = vec![
            pr(
                1,
                "patchula/a-1.x",
                update_body(&k1, &[change("a", "1.2.0")], true),
            ),
            pr(
                2,
                "patchula/b-1.x",
                update_body(&k2, &[change("b", "1.1.0")], false),
            ),
        ];
        let items = items(&prs, "bot", "o/r", "patchula/batch");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].number, Some(1));
        // The batch keeps the reviewed target; a ticked newer PR replaces it.
        let batch = format!(
            "{}\n<!-- patchula:{{\"version\":1,\"identity\":\"{}\",\"update\":\"{}\"}} -->",
            render(&items),
            "b".repeat(64),
            "c".repeat(64)
        );
        prs = vec![
            pr(9, "patchula/batch", batch.clone()),
            pr(
                3,
                "patchula/a-1.x",
                update_body(&k3, &[change("a", "1.3.0")], true),
            ),
        ];
        let items = super::items(&prs, "bot", "o/r", "patchula/batch");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].targets[0].value.as_deref(), Some("1.3.0"));
        // Ticking an item's removal box drops it.
        let removed = batch.replacen("- [ ]", "- [x]", 1);
        let items = super::items(
            &[pr(9, "patchula/batch", removed)],
            "bot",
            "o/r",
            "patchula/batch",
        );
        assert!(items.is_empty());
    }

    fn group_plan(branch: &str, file: &str) -> Plan {
        let mut target = change("shared", "1.2.0");
        target.file = file.into();
        crate::plan::testing::Draft::new(patchula_report::BranchAction {
            repository: "o/r".into(),
            branch: branch.into(),
            base_commit: "base".into(),
            status: patchula_report::BranchStatus::Planned,
            reason: None,
            message: "chore(deps): update shared to 1.2.0".into(),
            previous_commit: None,
            commit: None,
            verified: None,
            lockfile_pending: false,
            files: Vec::new(),
        })
        .identity(branch)
        .change(target)
        .lockfiles_ready(true)
        .build()
    }

    /// A tick lifts only a hold a tick may lift: a plan outside its window
    /// that is also blocked stays held, whichever came first.
    #[test]
    fn a_tick_never_lifts_a_blocked_plan() {
        use crate::plan::Hold;
        for order in [
            [Hold::Window, Hold::Blocked("digest unavailable".into())],
            [Hold::Blocked("digest unavailable".into()), Hold::Window],
        ] {
            let mut plan = group_plan("patchula/x", "package.json");
            for hold in order {
                plan.hold(hold);
            }
            plan.lift();
            assert_eq!(plan.action().status, patchula_report::BranchStatus::Skipped);
            assert_eq!(plan.action().reason.as_deref(), Some("digest unavailable"));
        }
        let mut plan = group_plan("patchula/x", "package.json");
        plan.hold(Hold::Window);
        plan.lift();
        assert_eq!(plan.action().status, patchula_report::BranchStatus::Planned);
        assert_eq!(plan.action().reason, None);
    }

    /// One group per service, each bumping a shared dependency to the same
    /// version: every group's PR is its own batch item, and taking one out
    /// leaves the others in.
    #[test]
    fn groups_bumping_the_same_target_are_separate_items() {
        let plans: Vec<_> = ["a", "b", "c"]
            .map(|s| {
                group_plan(
                    &format!("patchula/service-{s}"),
                    &format!("services/{s}/package.json"),
                )
            })
            .into();
        let keys: BTreeSet<_> = plans.iter().map(Plan::update_key).collect();
        assert_eq!(keys.len(), 3);
        let prs: Vec<_> = plans
            .iter()
            .zip(1..)
            .map(|(p, n)| {
                pr(
                    n,
                    &p.action().branch,
                    update_body(&p.update_key(), p.changes(), true),
                )
            })
            .collect();
        let items = items(&prs, "bot", "o/r", "patchula/batch");
        let files: BTreeSet<_> = items.iter().map(|i| i.targets[0].file.as_str()).collect();
        assert_eq!(files.len(), 3, "{items:?}");
        let batch = format!(
            "{}\n<!-- patchula:{{\"version\":1,\"identity\":\"{}\",\"update\":\"{}\"}} -->",
            render(&items),
            "b".repeat(64),
            "c".repeat(64)
        );
        let removed = batch.replacen("- [ ]", "- [x]", 1);
        let left = super::items(
            &[pr(9, "patchula/batch", removed)],
            "bot",
            "o/r",
            "patchula/batch",
        );
        assert_eq!(left.len(), 2);
    }

    #[test]
    fn payload_markers_outside_his_section_are_not_his() {
        let key = "1".repeat(64);
        // A valid targets marker quoted into foreign text ahead of his
        // section is not his; the one inside it is.
        let forged = markers::hidden(markers::TARGETS, &vec![change("forged", "9.9.9")]);
        let update = format!(
            "someone quoted {forged}\n{}\n{}",
            markers::START,
            update_body(&key, &[change("real", "1.2.0")], true)
        );
        let found = super::items(
            &[pr(1, "patchula/a-1.x", update)],
            "bot",
            "o/r",
            "patchula/batch",
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].targets[0].package, "real");
        // With only the quoted copy, the update does not batch.
        let own = format!(
            "forged {forged}\n{}\n- [x] Add to the batch PR. This one closes and the update moves there. {}\n<!-- patchula:{{\"version\":1,\"identity\":\"{}\",\"update\":\"{key}\"}} -->",
            markers::START,
            markers::ADD,
            "a".repeat(64)
        );
        assert!(
            super::items(
                &[pr(2, "patchula/b-1.x", own)],
                "bot",
                "o/r",
                "patchula/batch"
            )
            .is_empty()
        );
        // The batch's own items marker reads the same way.
        let forged_items = markers::hidden(
            markers::ITEMS,
            &[Item {
                origin: "forged".into(),
                number: None,
                title: "forged".into(),
                targets: vec![change("forged", "9.9.9")],
            }],
        );
        let batch = format!(
            "{forged_items}\n{}\n{}\n<!-- patchula:{{\"version\":1,\"identity\":\"{}\",\"update\":\"{}\"}} -->",
            markers::START,
            render(&[]),
            "b".repeat(64),
            "c".repeat(64)
        );
        assert!(
            super::items(
                &[pr(9, "patchula/batch", batch)],
                "bot",
                "o/r",
                "patchula/batch"
            )
            .is_empty()
        );
    }

    #[test]
    fn markers_cannot_be_closed_early_or_forged_by_values() {
        let marker = markers::hidden(
            markers::TARGETS,
            &vec![change("evil--><!-- patchula:x", "1.0.0")],
        );
        assert_eq!(marker.matches("-->").count(), 1);
        assert_eq!(marker.matches("<!--").count(), 1);
        let parsed: Vec<Change> =
            markers::hidden_json(&marker, markers::TARGETS, markers::PAYLOAD).unwrap();
        assert_eq!(parsed[0].package, "evil--><!-- patchula:x");
    }

    /// An update group that lands on the batch branch's name is refused
    /// before the batch is built, and told to rename.
    #[test]
    fn a_plan_on_the_batch_branch_fails_the_run() {
        use std::collections::BTreeMap;
        let config = patchula_config::load_files(
            "/repo/patchula.pkl",
            BTreeMap::from([(
                "/repo/patchula.pkl".into(),
                "amends \"PatchulaConfig.pkl\"".into(),
            )]),
        )
        .unwrap();
        let batch = crate::branch_name(&config.prs.branch, "batch").unwrap();
        let dep = Dependency {
            cooldown: None,
            manager: patchula_words::Manager::Npm,
            file: "package.json".into(),
            location: patchula_manager::Location {
                start: 0,
                end: 1,
                line: 1,
                column: 1,
                pointer: String::new(),
            },
            form: patchula_manager::EditForm::Scalar(patchula_manager::Syntax::Json),
            name: "shared".into(),
            package: "shared".into(),
            kind: patchula_words::DependencyKind::Production,
            current: "1.0.0".into(),
            resolved: Some("1.0.0".into()),
            digest: None,
            source: patchula_words::Source::Npm,
            scheme: patchula_words::Scheme::Semver,
            extract: None,
            included_by: None,
            registry: None,
        };
        let repository: Repository = serde_json::from_value(json!({
            "repository": "o/r",
            "status": "complete",
            "defaultBranch": "main",
            "commit": "a".repeat(40),
            "packageFiles": [{
                "path": "package.json",
                "dependencies": [{
                    "manager": "npm", "name": "shared", "package": "shared",
                    "current": {"value": "1.0.0", "version": "1.0.0", "digest": null},
                    "updates": [{"newValue": "1.2.0", "newVersion": "1.2.0", "newDigest": null,
                        "updateType": "minor", "group": null}],
                    "metadata": {"source": "npm", "kinds": [], "locations": []}
                }]
            }]
        }))
        .unwrap();
        let text = "{\"shared\":\"1.0.0\"}";
        let snapshot = Snapshot {
            cargo_cooldowns: Default::default(),
            paths: BTreeSet::from(["package.json".to_string()]),
            files: BTreeMap::from([(
                "package.json".to_string(),
                crate::Blob {
                    sha: crate::blob_sha(text),
                    mode: "100644".into(),
                    content: text.into(),
                },
            )]),
            public_only: Default::default(),
        };
        let mut plans = vec![group_plan(&batch, "package.json")];
        let prs = vec![pr(
            1,
            "patchula/shared-1.x",
            update_body(&"1".repeat(64), &[change("shared", "1.2.0")], true),
        )];
        let error = apply(
            &mut plans,
            &repository,
            &config,
            &[dep],
            &snapshot,
            &prs,
            "bot",
            true,
            None,
            chrono::Utc::now(),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("{batch} is reserved for the batch PR; rename the update group")
        );
    }
}
