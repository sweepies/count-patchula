//! The batch PR collects updates whose "Add to batch" box was ticked. Each
//! keeps the targets that were reviewed; the individual PR closes as batched,
//! which never suppresses the update. Unticking, removing or closing the batch
//! returns its updates to individual PRs.
use crate::markers;
use crate::{Change, Plan, Snapshot, pull_requests, update_key};
use patchula_config::{Config, Registry};
use patchula_manager::Dependency;
use patchula_report::Repository;
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
pub fn items(prs: &[Value], actor: &str, repo: &str, batch_branch: &str) -> Vec<Item> {
    let open: Vec<_> = prs
        .iter()
        .map(|pr| (pr, pull_requests::Pr::read(pr)))
        .filter(|(pr, fields)| {
            fields.state.as_deref() == Some("open") && pull_requests::owned_pr(pr, actor, repo)
        })
        .collect();
    let batch = open
        .iter()
        .find(|(_, fields)| fields.head.branch.as_deref() == Some(batch_branch));
    let mut items: Vec<Item> = batch
        .and_then(|(pr, _)| {
            markers::section_json::<Vec<Item>>(
                pull_requests::body(pr),
                markers::ITEMS,
                markers::PAYLOAD,
            )
        })
        .unwrap_or_default();
    if let Some((pr, _)) = batch {
        let removed: BTreeSet<_> = markers::ticked_lines(pull_requests::body(pr))
            .into_iter()
            .filter_map(|line| {
                markers::line_end(line, markers::REMOVE, usize::MAX).map(str::to_owned)
            })
            .collect();
        items.retain(|item| !removed.contains(&item.origin));
    }
    for (pr, fields) in &open {
        if fields.head.branch.as_deref() == Some(batch_branch)
            || !markers::ticked_lines(pull_requests::body(pr))
                .iter()
                .any(|line| line.ends_with(markers::ADD))
        {
            continue;
        }
        let (Some(origin), Some(targets)) = (
            pull_requests::marker_update(pull_requests::body(pr)),
            markers::section_json::<Vec<Change>>(
                pull_requests::body(pr),
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
            number: fields.number,
            title: fields.title.clone().unwrap_or_default(),
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
    prs: &[Value],
    actor: &str,
    allow_pending: bool,
    since: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<BTreeSet<String>, String> {
    let name = crate::branch_name(&config.prs.branch, "batch")?;
    let items = items(prs, actor, &repository.repository, &name);
    let targets: Vec<_> = items.iter().flat_map(|i| i.targets.clone()).collect();
    let Some(mut plan) = crate::plan::batch_plan(
        repository,
        config,
        dependencies,
        snapshot,
        &targets,
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
        return Err(format!(
            "{} is reserved for the batch PR; rename the update group",
            plan.action().branch
        ));
    }
    let items: Vec<_> = items
        .into_iter()
        .filter(|i| {
            i.targets.iter().any(|t| {
                plan.changes.iter().any(|c| {
                    c.touches(
                        &t.file,
                        t.source,
                        &t.package,
                        t.registry.as_ref().map(Registry::recorded),
                    )
                })
            })
        })
        .collect();
    let origins: BTreeSet<_> = items.iter().map(|i| i.origin.clone()).collect();
    // Batched plans stay desired so cleanup keeps their branches until their
    // PRs close as batched; a removed item resumes on the same branch.
    for p in plans
        .iter_mut()
        .filter(|p| origins.contains(&update_key(p)))
    {
        p.hold(crate::plan::Hold::Batched);
    }
    plan.batch = Some(items);
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
    fn pr(number: u64, branch: &str, body: String) -> Value {
        json!({"number":number,"title":format!("update {number}"),"state":"open","body":body,
            "user":{"login":"bot"},"head":{"ref":branch,"repo":{"full_name":"o/r"}},
            "base":{"repo":{"full_name":"o/r"}}})
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
        let mut plan = Plan::new(patchula_report::BranchAction {
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
        });
        plan.identity = branch.into();
        plan.changes = vec![target];
        plan.lockfiles_ready = true;
        plan
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
        let keys: BTreeSet<_> = plans.iter().map(update_key).collect();
        assert_eq!(keys.len(), 3);
        let prs: Vec<_> = plans
            .iter()
            .zip(1..)
            .map(|(p, n)| {
                pr(
                    n,
                    &p.action().branch,
                    update_body(&update_key(p), &p.changes, true),
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
}
