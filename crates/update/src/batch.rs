//! The batch PR collects updates whose "Add to batch" box was ticked. Each
//! keeps the targets that were reviewed; the individual PR closes as batched,
//! which never suppresses the update. Unticking, removing or closing the batch
//! returns its updates to individual PRs.
use crate::error::{Error, PlanError};
use crate::markers;
use crate::{Change, Inventory, Plan, Snapshot, pull_requests};
use patchula_config::{Config, Registry};
use patchula_github::{
    Client,
    model::{GitCommit, Tree, TreeEntry},
};
use patchula_manager::Dependency;
use patchula_report::Repository;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Maximum changed files recorded in one item's [`GreenProof`] so a large PR
/// cannot bloat the batch PR's items marker.
const MAX_GREEN_FILES: usize = 64;
/// Maximum serialized byte size of one item's [`GreenProof`].
const MAX_GREEN_PROOF_BYTES: usize = 4_096;

/// Proof that an item's source PR passed every check on `base`, together with
/// the exact file-level tree delta that commit tested.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GreenProof {
    /// The base commit SHA the source PR was built and verified on.
    pub base: String,
    /// Exact changed blobs (`Some(blob_sha)`) and deleted blobs (`None`) in the
    /// verified source PR head commit relative to `base`.
    pub files: BTreeMap<String, Option<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    /// The update key of the PR the item came from.
    pub origin: String,
    pub number: Option<u64>,
    pub title: String,
    pub targets: Vec<Change>,
    /// Proof that this item's source PR was clean and green on a specific base
    /// commit, along with the exact blob changes it tested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub green: Option<GreenProof>,
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
/// open update PR whose "Add to batch" box is ticked on the PR itself or on
/// the dependency dashboard. A newly added update replaces an item for the
/// same dependency, so its newer target wins.
pub fn items(
    prs: &[pull_requests::Tracked],
    dashboard_batch: &BTreeSet<String>,
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
        if t.pr.head.branch == batch_branch {
            continue;
        }
        let body = pull_requests::body(&t.pr);
        let (Some(origin), Some(targets)) = (
            pull_requests::marker_update(body),
            markers::section_json::<Vec<Change>>(body, markers::TARGETS, markers::PAYLOAD),
        ) else {
            continue;
        };
        let ticked = dashboard_batch.contains(&origin)
            || markers::ticked_lines(body)
                .iter()
                .any(|line| line.ends_with(markers::ADD));
        if !ticked || targets.is_empty() {
            continue;
        }
        items.retain(|item| item.origin != origin && !overlaps(item, &targets));
        items.push(Item {
            origin,
            number: Some(t.pr.number),
            title: t.pr.title.clone(),
            targets,
            green: None,
        });
    }
    items.sort();
    items
}

/// The batch branch's label under the branch prefix.
pub(crate) const LABEL: &str = "batch";

/// Whether `plan` is a non-empty batch plan whose exact written tree equals
/// `base_commit` plus the pairwise-disjoint file deltas of verified clean,
/// green source PRs built on that same `base_commit`, with no lockfile pending.
pub(crate) fn is_green(plan: &Plan) -> bool {
    if plan.action().lockfile_pending || plan.owes_lockfiles() {
        return false;
    }
    let Some(items) = plan.batch_items() else {
        return false;
    };
    if items.is_empty() {
        return false;
    }
    let base_commit = plan.action().base_commit.as_str();
    let mut expected: BTreeMap<&str, Option<&str>> = BTreeMap::new();
    for item in items {
        let Some(proof) = &item.green else {
            return false;
        };
        if proof.base != base_commit || proof.files.is_empty() {
            return false;
        }
        for (path, blob) in &proof.files {
            // Overlapping files (e.g. a shared lockfile or manifest) mean the
            // combined result was never tested by any single source PR.
            if expected.insert(path.as_str(), blob.as_deref()).is_some() {
                return false;
            }
        }
    }
    let contents = plan.contents();
    let deleted = plan.deleted();
    if expected.len() != contents.len() {
        return false;
    }
    expected
        .into_iter()
        .all(|(path, expected_sha)| match expected_sha {
            None => deleted.contains(path) && contents.contains_key(path),
            Some(sha) => {
                !deleted.contains(path)
                    && contents
                        .get(path)
                        .is_some_and(|content| crate::blob_sha(content) == sha)
            }
        })
}

fn leaf_entries(entries: &[TreeEntry]) -> BTreeMap<&str, (&str, &str, &str)> {
    entries
        .iter()
        .filter(|e| e.kind != "tree")
        .map(|e| {
            (
                e.path.as_str(),
                (e.sha.as_str(), e.mode.as_str(), e.kind.as_str()),
            )
        })
        .collect()
}

/// Exact blob-level delta from `base_entries` to `source_entries`, or `None`
/// if empty, non-blob, mode-changing, or too large to vouch for.
fn tree_delta(
    base_entries: &[TreeEntry],
    source_entries: &[TreeEntry],
) -> Option<BTreeMap<String, Option<String>>> {
    let base = leaf_entries(base_entries);
    let source = leaf_entries(source_entries);
    let mut files = BTreeMap::new();
    for (&path, &(sha, mode, kind)) in &source {
        match base.get(path) {
            None => {
                if kind != "blob" || mode != "100644" {
                    return None;
                }
                files.insert(path.to_owned(), Some(sha.to_owned()));
            }
            Some(&(base_sha, base_mode, base_kind)) => {
                if (sha, mode, kind) != (base_sha, base_mode, base_kind) {
                    if kind != "blob" || base_kind != "blob" || mode != base_mode {
                        return None;
                    }
                    files.insert(path.to_owned(), Some(sha.to_owned()));
                }
            }
        }
    }
    for (&path, &(_, _, base_kind)) in &base {
        if !source.contains_key(path) {
            if base_kind != "blob" {
                return None;
            }
            files.insert(path.to_owned(), None);
        }
    }
    if files.is_empty() || files.len() > MAX_GREEN_FILES {
        return None;
    }
    Some(files)
}

/// If `pr`'s head commit in `inventory` is a verified commit of the Count's on
/// `base_commit` against `default_branch` with no rebasing banner or pending
/// lockfile, return its tree OID so its checks can vouch for its exact changes.
fn eligible_for_green<'a>(
    inventory: &'a Inventory,
    pr: &pull_requests::Tracked,
    base_commit: &str,
    default_branch: &str,
) -> Option<&'a str> {
    if pr.pr.base.branch != default_branch || crate::pr::banner::shown(pull_requests::body(&pr.pr))
    {
        return None;
    }
    let r = inventory
        .refs
        .items
        .iter()
        .flatten()
        .find(|r| r.name == pr.pr.head.branch)?;
    let Some(crate::receipts::RefTarget::Commit(c)) = &r.target else {
        return None;
    };
    let actor = inventory.actor.as_str();
    let ok = c.oid == pr.pr.head.sha
        && c.author
            .as_ref()
            .and_then(|a| a.user.as_ref())
            .is_some_and(|u| u.login == actor)
        && c.committer
            .as_ref()
            .and_then(|a| a.user.as_ref())
            .is_some_and(|u| u.login == actor || u.login == crate::cleanup::WEB_FLOW)
        && c.signature.as_ref().is_some_and(|s| s.is_valid)
        && crate::trailers::version(&c.message)
        && crate::trailers::base(&c.message) == Some(base_commit)
        && !c.message.contains("Lockfile pending:");
    ok.then_some(c.tree.oid.as_str())
}

/// Add the batch plan and skip the update plans it carries. Items whose targets the
/// base already satisfies are complete and leave the batch.
#[allow(clippy::too_many_arguments)]
pub async fn apply(
    client: &Client,
    plans: &mut Vec<Plan>,
    repository: &Repository,
    config: &Config,
    dependencies: &[Dependency],
    snapshot: &Snapshot,
    inventory: &Inventory,
    dashboard_batch: &BTreeSet<String>,
    allow_pending: bool,
    since: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<BTreeSet<String>, Error> {
    let name = crate::branch_name(&config.prs.branch, LABEL)
        .map_err(|e| Error::Refused(e.to_string().into()))?;
    let prs = inventory.pull_requests(&repository.repository);
    let mut items = items(
        &prs,
        dashboard_batch,
        &inventory.actor,
        &repository.repository,
        &name,
    );
    if let (Some(base_commit), Some(default_branch)) = (
        repository.commit.as_deref(),
        repository.default_branch.as_deref(),
    ) {
        let mut base_entries: Option<Option<Vec<TreeEntry>>> = None;
        for item in &mut items {
            if item.green.is_some() {
                continue;
            }
            let Some(source) = prs.iter().find(|t| {
                t.pr.state == "open"
                    && t.pr.head.branch != name
                    && Some(t.pr.number) == item.number
                    && pull_requests::marker_update(pull_requests::body(&t.pr)).as_deref()
                        == Some(item.origin.as_str())
            }) else {
                continue;
            };
            let Some(source_tree_oid) =
                eligible_for_green(inventory, source, base_commit, default_branch)
            else {
                continue;
            };
            let passed = crate::automerge::clean_and_green(
                client,
                &inventory.actor,
                &repository.repository,
                &source.pr,
            )
            .await
            .map_err(|_| Error::Raced("failed to inspect batched PR checks; rerun"))?;
            if !passed {
                continue;
            }
            if base_entries.is_none() {
                let base_git = client
                    .get_immutable::<GitCommit>(&format!(
                        "repos/{}/git/commits/{base_commit}",
                        repository.repository
                    ))
                    .await
                    .map_err(|_| Error::Raced("failed to inspect base commit tree; rerun"))?;
                let base_tree = client
                    .get_immutable::<Tree>(&format!(
                        "repos/{}/git/trees/{}?recursive=1",
                        repository.repository, base_git.tree.sha
                    ))
                    .await
                    .map_err(|_| Error::Raced("failed to inspect base commit tree; rerun"))?;
                base_entries = Some((!base_tree.truncated).then_some(base_tree.tree));
            }
            let Some(base_tree_entries) = base_entries.as_ref().and_then(|o| o.as_deref()) else {
                continue;
            };
            let source_tree = client
                .get_immutable::<Tree>(&format!(
                    "repos/{}/git/trees/{source_tree_oid}?recursive=1",
                    repository.repository
                ))
                .await
                .map_err(|_| Error::Raced("failed to inspect batched PR tree; rerun"))?;
            if source_tree.truncated {
                continue;
            }
            let Some(files) = tree_delta(base_tree_entries, &source_tree.tree) else {
                continue;
            };
            let proof = GreenProof {
                base: base_commit.to_owned(),
                files,
            };
            if serde_json::to_string(&proof).is_ok_and(|s| s.len() <= MAX_GREEN_PROOF_BYTES) {
                item.green = Some(proof);
            }
        }
    }
    apply_items(
        plans,
        repository,
        config,
        dependencies,
        snapshot,
        items,
        allow_pending,
        since,
        now,
    )
    .map_err(|e| Error::Refused(e.to_string().into()))
}

#[allow(clippy::too_many_arguments)]
fn apply_items(
    plans: &mut Vec<Plan>,
    repository: &Repository,
    config: &Config,
    dependencies: &[Dependency],
    snapshot: &Snapshot,
    items: Vec<Item>,
    allow_pending: bool,
    since: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<BTreeSet<String>, PlanError> {
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

/// The batch section of the batch PR body: a collapsed list with a removal
/// box per item, then the items marker, ending in a blank line.
pub(crate) fn render(items: &[Item]) -> String {
    let mut text = format!(
        "<details>\n<summary><strong>Remove an update from this batch</strong> ({} item{})</summary>\n\n<sub>Tick an item to send it back to its own pull request; closing this PR sends every item back.</sub>\n\n",
        items.len(),
        if items.len() == 1 { "" } else { "s" }
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
    text.push_str("\n</details>\n\n");
    text.push_str(&markers::hidden(markers::ITEMS, &items));
    text.push_str("\n\n");
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn change(package: &str, value: &str) -> Change {
        Change {
            value: Some(value.into()),
            version: Some(value.into()),
            ..crate::plan::testing::change(package)
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
        let empty = BTreeSet::new();
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
        let items = items(&prs, &empty, "bot", "o/r", "patchula/batch");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].number, Some(1));
        // Ticking on the dashboard adds an unticked PR too.
        let dashboard = BTreeSet::from([k2.clone()]);
        let both = super::items(&prs, &dashboard, "bot", "o/r", "patchula/batch");
        assert_eq!(both.len(), 2);
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
        let items = super::items(&prs, &empty, "bot", "o/r", "patchula/batch");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].targets[0].value.as_deref(), Some("1.3.0"));
        // Ticking an item's removal box drops it.
        let removed = batch.replacen("- [ ]", "- [x]", 1);
        let items = super::items(
            &[pr(9, "patchula/batch", removed)],
            &empty,
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
        let empty = BTreeSet::new();
        let items = items(&prs, &empty, "bot", "o/r", "patchula/batch");
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
            &empty,
            "bot",
            "o/r",
            "patchula/batch",
        );
        assert_eq!(left.len(), 2);
    }

    #[test]
    fn payload_markers_outside_his_section_are_not_his() {
        let key = "1".repeat(64);
        let empty = BTreeSet::new();
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
            &empty,
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
                &empty,
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
                green: None,
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
                &empty,
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
        let batch = crate::branch_name(&config.prs.branch, LABEL).unwrap();
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
                patchula_lockfiles::BaseFile {
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
        let items = items(&prs, &BTreeSet::new(), "bot", "o/r", &batch);
        let error = apply_items(
            &mut plans,
            &repository,
            &config,
            &[dep],
            &snapshot,
            items,
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

    #[test]
    fn is_green_requires_disjoint_files_and_exact_blob_match() {
        let base = "b".repeat(40);
        let a_content = "{\"a\":\"2.0.0\"}\n";
        let b_content = "{\"b\":\"2.0.0\"}\n";
        let item = |origin: &str, files: BTreeMap<String, Option<String>>| Item {
            origin: origin.into(),
            number: Some(1),
            title: origin.into(),
            targets: vec![change(origin, "2.0.0")],
            green: Some(GreenProof {
                base: base.clone(),
                files,
            }),
        };
        let mut action = crate::plan::testing::action("patchula/batch");
        action.base_commit = base.clone();

        // A plan whose lockfiles have not yet been marked ready fails closed.
        let unready = crate::plan::testing::Draft::new(action.clone())
            .batch(vec![item(
                "a",
                BTreeMap::from([("a.json".into(), Some(crate::blob_sha(a_content)))]),
            )])
            .content("a.json", a_content)
            .build();
        assert!(!is_green(&unready));

        // Single item with exact blob SHA match is green.
        let one = crate::plan::testing::Draft::new(action.clone())
            .batch(vec![item(
                "a",
                BTreeMap::from([("a.json".into(), Some(crate::blob_sha(a_content)))]),
            )])
            .content("a.json", a_content)
            .lockfiles_ready(true)
            .build();
        assert!(is_green(&one));

        // Two items touching disjoint files with exact blob SHAs are green.
        let two = crate::plan::testing::Draft::new(action.clone())
            .batch(vec![
                item(
                    "a",
                    BTreeMap::from([("a.json".into(), Some(crate::blob_sha(a_content)))]),
                ),
                item(
                    "b",
                    BTreeMap::from([("b.json".into(), Some(crate::blob_sha(b_content)))]),
                ),
            ])
            .content("a.json", a_content)
            .content("b.json", b_content)
            .lockfiles_ready(true)
            .build();
        assert!(is_green(&two));

        // Two items overlapping on a shared lockfile are NOT green, even if the
        // blob SHA happened to match one of them.
        let shared_lock = "lock-v2\n";
        let overlapping = crate::plan::testing::Draft::new(action.clone())
            .batch(vec![
                item(
                    "a",
                    BTreeMap::from([
                        ("a.json".into(), Some(crate::blob_sha(a_content))),
                        ("Cargo.lock".into(), Some(crate::blob_sha(shared_lock))),
                    ]),
                ),
                item(
                    "b",
                    BTreeMap::from([
                        ("b.json".into(), Some(crate::blob_sha(b_content))),
                        ("Cargo.lock".into(), Some(crate::blob_sha(shared_lock))),
                    ]),
                ),
            ])
            .content("a.json", a_content)
            .content("b.json", b_content)
            .content("Cargo.lock", shared_lock)
            .lockfiles_ready(true)
            .build();
        assert!(!is_green(&overlapping));

        // A single byte of drift between the source head's blob and the batch's
        // written content (e.g. body/branch desync or rebuilt content) fails closed.
        let drifted = crate::plan::testing::Draft::new(action.clone())
            .batch(vec![item(
                "a",
                BTreeMap::from([("a.json".into(), Some(crate::blob_sha(a_content)))]),
            )])
            .content("a.json", "{\"a\":\"2.0.1\"}\n")
            .lockfiles_ready(true)
            .build();
        assert!(!is_green(&drifted));

        // Extra files written by the batch that no green proof vouches for fail closed.
        let extra = crate::plan::testing::Draft::new(action.clone())
            .batch(vec![item(
                "a",
                BTreeMap::from([("a.json".into(), Some(crate::blob_sha(a_content)))]),
            )])
            .content("a.json", a_content)
            .content("Cargo.lock", shared_lock)
            .lockfiles_ready(true)
            .build();
        assert!(!is_green(&extra));

        // Pending lockfile fails closed.
        let mut pending_action = action.clone();
        pending_action.lockfile_pending = true;
        let pending = crate::plan::testing::Draft::new(pending_action)
            .batch(vec![item(
                "a",
                BTreeMap::from([("a.json".into(), Some(crate::blob_sha(a_content)))]),
            )])
            .content("a.json", a_content)
            .lockfiles_ready(true)
            .build();
        assert!(!is_green(&pending));

        // Base mismatch fails closed.
        let mut moved_action = action;
        moved_action.base_commit = "c".repeat(40);
        let moved = crate::plan::testing::Draft::new(moved_action)
            .batch(vec![item(
                "a",
                BTreeMap::from([("a.json".into(), Some(crate::blob_sha(a_content)))]),
            )])
            .content("a.json", a_content)
            .lockfiles_ready(true)
            .build();
        assert!(!is_green(&moved));
    }

    #[test]
    fn tree_delta_rejects_empty_mode_changes_and_non_blobs() {
        let entry = |path: &str, sha: &str, mode: &str, kind: &str| TreeEntry {
            path: path.into(),
            sha: sha.into(),
            mode: mode.into(),
            kind: kind.into(),
            size: None,
        };
        let base = vec![
            entry("a.toml", "sha1", "100644", "blob"),
            entry("old.lock", "sha2", "100644", "blob"),
        ];
        // Identical trees produce None (empty delta).
        assert_eq!(tree_delta(&base, &base), None);
        // Modified + added + deleted blobs produce the exact delta.
        let changed = vec![
            entry("a.toml", "sha1-new", "100644", "blob"),
            entry("new.toml", "sha3", "100644", "blob"),
        ];
        assert_eq!(
            tree_delta(&base, &changed),
            Some(BTreeMap::from([
                ("a.toml".into(), Some("sha1-new".into())),
                ("new.toml".into(), Some("sha3".into())),
                ("old.lock".into(), None),
            ]))
        );
        // Mode change on an existing file fails closed.
        let mode_changed = vec![
            entry("a.toml", "sha1", "100755", "blob"),
            entry("old.lock", "sha2", "100644", "blob"),
        ];
        assert_eq!(tree_delta(&base, &mode_changed), None);
        // Non-100644 new file fails closed.
        let exec_new = vec![
            entry("a.toml", "sha1", "100644", "blob"),
            entry("old.lock", "sha2", "100644", "blob"),
            entry("run.sh", "sha4", "100755", "blob"),
        ];
        assert_eq!(tree_delta(&base, &exec_new), None);
        // Submodule / non-blob addition fails closed.
        let submodule = vec![
            entry("a.toml", "sha1", "100644", "blob"),
            entry("old.lock", "sha2", "100644", "blob"),
            entry("sub", "sha5", "160000", "commit"),
        ];
        assert_eq!(tree_delta(&base, &submodule), None);
    }
}
