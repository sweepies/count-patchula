//! The batch PR collects updates whose "Add to batch" box was ticked. Each
//! keeps the targets that were reviewed; the individual PR closes as batched,
//! which never suppresses the update. Unticking, removing or closing the batch
//! returns its updates to individual PRs.
use crate::{Change, Plan, Snapshot, pull_requests, update_key};
use patchula_config::Config;
use patchula_manager::Dependency;
use patchula_report::Repository;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::BTreeSet;

/// Ticked on an update PR: move it into the batch.
pub const ADD: &str = "<!-- patchula-batch-add -->";
/// The reviewed targets of an update PR, recorded when its body was rendered.
pub(crate) const TARGETS: &str = "<!-- patchula-targets:";
/// The batch PR's items.
pub(crate) const ITEMS: &str = "<!-- patchula-batch:";
/// Ticked on a batch item: return it to an individual PR.
pub(crate) const REMOVE: &str = "<!-- patchula-batch-remove:";
/// The reason recorded on a plan whose update the batch carries.
pub const BATCHED_REASON: &str = "batched: carried by the batch PR";
/// A PR closed because its update moved into the batch.
pub(crate) const BATCHED: &str = "<!-- patchula-closed:batched -->";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    /// The update key of the PR the item came from.
    pub origin: String,
    pub number: Option<u64>,
    pub title: String,
    pub targets: Vec<Change>,
}

/// JSON in an HTML comment. `<` and `>` are escaped (they only occur inside
/// JSON strings), so a value can neither end the comment nor forge a marker.
pub(crate) fn hidden_marker<T: Serialize>(prefix: &str, value: &T) -> String {
    let json = serde_json::to_string(value)
        .expect("serializable marker")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e");
    format!("{prefix}{json} -->")
}
fn hidden<T: DeserializeOwned>(body: &str, prefix: &str) -> Option<T> {
    let (_, rest) = body.split_once(prefix)?;
    if rest.contains(prefix) {
        return None;
    }
    let (json, _) = rest.split_once(" -->")?;
    serde_json::from_str(json).ok()
}
fn body(pr: &Value) -> &str {
    pr["body"].as_str().unwrap_or("")
}
fn branch(pr: &Value) -> &str {
    pr["head"]["ref"].as_str().unwrap_or("")
}
fn overlaps(item: &Item, targets: &[Change]) -> bool {
    item.targets.iter().any(|a| {
        targets
            .iter()
            .any(|b| a.source == b.source && a.package == b.package && a.file == b.file)
    })
}

/// The batch branch name for a configured prefix.
pub fn branch_name(config: &Config) -> Result<String, String> {
    crate::branch_name(&config.prs.branch, "batch", "batch")
}

/// Current items: the open batch PR's items without removed ones, plus every
/// open update PR whose "Add to batch" box is ticked. A newly added update
/// replaces an item for the same dependency, so its newer target wins.
pub fn items(prs: &[Value], actor: &str, repo: &str, batch_branch: &str) -> Vec<Item> {
    let open: Vec<_> = prs
        .iter()
        .filter(|p| p["state"] == "open" && pull_requests::owned_pr(p, actor, repo))
        .collect();
    let batch = open.iter().find(|p| branch(p) == batch_branch);
    let mut items: Vec<Item> = batch
        .and_then(|p| hidden(body(p), ITEMS))
        .unwrap_or_default();
    if let Some(pr) = batch {
        let removed: BTreeSet<_> = pull_requests::ticked_lines(body(pr))
            .into_iter()
            .filter_map(|line| {
                line.rsplit_once(REMOVE)
                    .and_then(|(_, rest)| rest.strip_suffix(" -->"))
                    .map(str::to_owned)
            })
            .collect();
        items.retain(|item| !removed.contains(&item.origin));
    }
    for pr in &open {
        if branch(pr) == batch_branch
            || !pull_requests::ticked_lines(body(pr))
                .iter()
                .any(|line| line.ends_with(ADD))
        {
            continue;
        }
        let (Some(origin), Some(targets)) = (
            pull_requests::marker_update(body(pr)),
            hidden::<Vec<Change>>(body(pr), TARGETS),
        ) else {
            continue;
        };
        if targets.is_empty() {
            continue;
        }
        items.retain(|item| item.origin != origin && !overlaps(item, &targets));
        items.push(Item {
            origin,
            number: pr["number"].as_u64(),
            title: pr["title"].as_str().unwrap_or("").into(),
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
    let name = branch_name(config)?;
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
    if plans.iter().any(|p| p.action.branch == plan.action.branch) {
        return Err(format!(
            "{} is reserved for the batch PR; rename the update group",
            plan.action.branch
        ));
    }
    let pending: BTreeSet<_> = plan
        .changes
        .iter()
        .map(|c| (&c.source, &c.package, &c.file))
        .collect();
    let items: Vec<_> = items
        .into_iter()
        .filter(|i| {
            i.targets
                .iter()
                .any(|t| pending.contains(&(&t.source, &t.package, &t.file)))
        })
        .collect();
    let origins: BTreeSet<_> = items.iter().map(|i| i.origin.clone()).collect();
    // Batched plans stay desired so cleanup keeps their branches until their
    // PRs close as batched; a removed item resumes on the same branch.
    for p in plans
        .iter_mut()
        .filter(|p| origins.contains(&update_key(p)))
    {
        p.action.status = patchula_report::ActionStatus::Skipped;
        p.action.reason = Some(BATCHED_REASON.into());
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
            "- [ ] {title}{from} {REMOVE}{} -->\n",
            item.origin
        ));
    }
    text.push('\n');
    text.push_str(&hidden_marker(ITEMS, &items));
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn change(package: &str, value: &str) -> Change {
        Change {
            source: "npm".into(),
            package: package.into(),
            file: "package.json".into(),
            current: "1.0.0".into(),
            current_version: None,
            value: Some(value.into()),
            version: Some(value.into()),
            digest: None,
            kind: "minor".into(),
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
            "{}\n- [{}] Add to the batch PR. This one closes and the update moves there. {ADD}\n<!-- patchula:{{\"version\":1,\"identity\":\"{}\",\"update\":\"{key}\"}} -->\n",
            hidden_marker(TARGETS, &targets),
            if ticked { "x" } else { " " },
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

    #[test]
    fn markers_cannot_be_closed_early_or_forged_by_values() {
        let marker = hidden_marker(TARGETS, &vec![change("evil--><!-- patchula:x", "1.0.0")]);
        assert_eq!(marker.matches("-->").count(), 1);
        assert_eq!(marker.matches("<!--").count(), 1);
        let parsed: Vec<Change> = hidden(&marker, TARGETS).unwrap();
        assert_eq!(parsed[0].package, "evil--><!-- patchula:x");
    }
}
