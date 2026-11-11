use crate::Plan;
use crate::markers;
use crate::pr::body::{
    Bodies, Closure, body_hash, closure, notes_key, same_owner, title, unticked, with_foreign,
};
use crate::pr::view;
use chrono::{DateTime, Utc};
use patchula_config::{Config, PackageId};
use patchula_github::{Client, model};
use patchula_report::{BranchStatus, PullRequestAction, PullRequestStatus};
use patchula_words::Recheck;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

pub const BODY_LIMIT: usize = 32_000;

#[allow(dead_code)]
mod auto_merge {
    use graphql_client::GraphQLQuery;

    /// `GitObjectID` scalar the generated variables resolve as
    /// `super::GitObjectID`.
    type GitObjectID = String;

    /// `enable-auto-merge.graphql`, checked against the vendored GitHub
    /// schema. One operation per document: a `query` body carries the whole
    /// file, so two operations here would ask GitHub to run both.
    #[derive(GraphQLQuery)]
    #[graphql(
        schema_path = "../github/graphql/schema.docs.graphql",
        query_path = "src/enable-auto-merge.graphql",
        response_derives = "Debug"
    )]
    pub struct EnableAutoMerge;
    /// The sibling mutation in its own document.
    #[derive(GraphQLQuery)]
    #[graphql(
        schema_path = "../github/graphql/schema.docs.graphql",
        query_path = "src/disable-auto-merge.graphql",
        response_derives = "Debug"
    )]
    pub struct DisableAutoMerge;
}
pub(crate) use auto_merge::disable_auto_merge::ResponseData as DisableAutoMergeData;
pub(crate) use auto_merge::enable_auto_merge::ResponseData as EnableAutoMergeData;

/// The enable-auto-merge document, exported for tests that compare request
/// bodies.
pub const ENABLE_AUTO_MERGE: &str = auto_merge::enable_auto_merge::QUERY;
const DISABLE_AUTO_MERGE: &str = auto_merge::disable_auto_merge::QUERY;

/// How the re-check timing reads in PR bodies and the dashboard.
pub(crate) fn recheck_timing(recheck: Recheck) -> &'static str {
    match recheck {
        Recheck::Now => "now",
        Recheck::NextRun => "on the next run",
    }
}
/// How `recheck` enters the body and receipt hashes. Open PR bodies carry
/// hashes over these spellings, so changing them rewrites every open PR body.
pub(crate) fn recheck_hashed(recheck: Recheck) -> &'static str {
    match recheck {
        Recheck::Now => "Now",
        Recheck::NextRun => "NextRun",
    }
}
/// Whether the Count should merge this plan itself once its checks pass:
/// never while a lockfile is pending; maintenance plans follow the default
/// policy, and every other plan merges only when all its changes opt in.
pub(crate) fn merge_requested(plan: &Plan, policy: &patchula_config::Policy) -> bool {
    !plan.action().lockfile_pending
        && if plan.is_maintenance() {
            policy.merge
        } else {
            !plan.changes().is_empty() && plan.changes().iter().all(|c| c.merge)
        }
}
/// The Count's note on a PR's merge state, carried on the PR object the
/// lifecycle, receipts and dashboard share.
#[derive(Clone, Debug)]
pub enum MergeNote {
    /// "auto-merge waiting for a later run with a verified receipt and passing checks"
    Waiting,
    /// "native auto-merge enabled"
    Native,
    /// Why the Count's merge did not happen, in its own words.
    Reason(String),
}
impl MergeNote {
    fn text(&self) -> &str {
        match self {
            Self::Waiting => {
                "auto-merge waiting for a later run with a verified receipt and passing checks"
            }
            Self::Native => "native auto-merge enabled",
            Self::Reason(reason) => reason,
        }
    }
}
/// A pull request as the lifecycle carries it: the decoded REST object
/// plus the Count's own merge note, for which GitHub's object has no
/// place. What JSON smuggled in as a `patchula_merge` key is a field now.
#[derive(Clone, Debug)]
pub struct Tracked {
    /// The pull request itself.
    pub pr: model::PullRequest,
    /// The Count's note on its merge state.
    pub(crate) merge_note: Option<MergeNote>,
}
impl Tracked {
    /// The note a PR carries, if any.
    pub(crate) fn merge_note(&self) -> Option<&str> {
        self.merge_note.as_ref().map(MergeNote::text)
    }
    /// Set the note; `None` leaves it unset.
    pub fn set_merge_note(&mut self, note: Option<MergeNote>) {
        self.merge_note = note;
    }
    /// Mark the PR closed with `text` as its body — the local edit applied
    /// after a closure PATCH, before GitHub's answer says anything.
    pub(crate) fn mark_closed(&mut self, body: String) {
        self.pr.state = "closed".into();
        self.pr.body = Some(body);
    }
    /// Mark the PR merged at `at` — the local edit applied after the
    /// Count's own merge answers.
    pub(crate) fn mark_merged(&mut self, at: String) {
        self.pr.state = "closed".into();
        self.pr.merged_at = Some(at);
    }
    /// Record whether auto-merge is enabled on the PR.
    pub(crate) fn set_auto_merge(&mut self, enabled: bool) {
        self.pr.auto_merge = enabled.then_some(model::AutoMerge {});
    }
    /// Record the PR as open — the local edit after it is created or
    /// updated.
    pub(crate) fn mark_open(&mut self) {
        self.pr.state = "open".into();
    }
    /// Record the PR's labels after a label write.
    pub(crate) fn set_labels(&mut self, labels: BTreeSet<String>) {
        self.pr.labels = labels
            .into_iter()
            .map(|name| model::Label { name })
            .collect();
    }
}
impl From<model::PullRequest> for Tracked {
    fn from(pr: model::PullRequest) -> Self {
        Self {
            pr,
            merge_note: None,
        }
    }
}
/// The fields an inventory `PullRequest` projects onto the lifecycle's
/// `Tracked` — what the old `rest_pr` used to synthesize as a
/// REST object. `None` when the label page is incomplete: an unverified
/// pull must not settle.
pub(crate) fn tracked_pr(pr: &crate::receipts::InventoryPr) -> Option<Tracked> {
    let labels = pr.labels.as_ref()?;
    if !labels.complete {
        return None;
    }
    Some(Tracked {
        pr: model::PullRequest {
            node_id: pr.id.clone(),
            number: pr.number as u64,
            html_url: pr.url.clone(),
            state: "open".into(),
            title: pr.title.clone(),
            body: Some(pr.body.clone()),
            user: pr.author.as_ref().map(|actor| model::Actor {
                login: crate::receipts::actor_login(actor),
            }),
            draft: None,
            labels: labels
                .items
                .iter()
                .flatten()
                .map(|l| model::Label {
                    name: l.name.clone(),
                })
                .collect(),
            merged_at: None,
            auto_merge: pr
                .auto_merge_request
                .is_some()
                .then_some(model::AutoMerge {}),
            mergeable: None,
            mergeable_state: None,
            created_at: None,
            updated_at: None,
            head: model::PullRequestHead {
                branch: pr.head_ref_name.clone(),
                sha: pr.head_ref_oid.clone(),
                repo: pr
                    .head_repository
                    .as_ref()
                    .map(|r| model::PullRequestRepository {
                        full_name: r.name_with_owner.clone(),
                    }),
            },
            base: model::PullRequestBase {
                branch: pr.base_ref_name.clone(),
                repo: pr
                    .base_repository
                    .as_ref()
                    .map(|r| model::PullRequestRepository {
                        full_name: r.name_with_owner.clone(),
                    }),
            },
        },
        merge_note: None,
    })
}
/// The body text of a pull request; `None` reads as empty, as indexing a
/// missing or null `body` did.
pub(crate) fn body(pr: &model::PullRequest) -> &str {
    pr.body.as_deref().unwrap_or("")
}
pub(crate) fn branch(pr: &model::PullRequest) -> &str {
    &pr.head.branch
}
/// Whether two GitHub repository names name the same repository: repository
/// names are case-insensitive.
pub fn same_repository(name: &str, other: &str) -> bool {
    name.eq_ignore_ascii_case(other)
}
pub(crate) fn same_repo(pr: &model::PullRequest, repo: &str) -> bool {
    pr.head
        .repo
        .as_ref()
        .is_some_and(|r| same_repository(&r.full_name, repo))
        && pr
            .base
            .repo
            .as_ref()
            .is_some_and(|r| same_repository(&r.full_name, repo))
}
pub fn owned_pr(pr: &model::PullRequest, actor: &str, repo: &str) -> bool {
    pr.user.as_ref().is_some_and(|u| u.login == actor)
        && same_repo(pr, repo)
        && marker_update(body(pr)).is_some()
}
/// The action a plan whose PR already says everything records: the PR is
/// left exactly as it is.
pub fn unchanged(repo: &str, branch: &str, pr: &model::PullRequest) -> PullRequestAction {
    action(repo, branch, Some(pr), PullRequestStatus::Unchanged, None)
}
fn action(
    repo: &str,
    name: &str,
    pr: Option<&model::PullRequest>,
    status: PullRequestStatus,
    reason: Option<String>,
) -> PullRequestAction {
    PullRequestAction {
        repository: repo.into(),
        branch: name.into(),
        number: pr.map(|p| p.number),
        url: pr.map(|p| p.html_url.clone()),
        status,
        reason,
    }
}
/// The update key recorded by a patchula PR's ownership marker.
pub(crate) fn marker_update(text: &str) -> Option<String> {
    markers::ownership_marker(text).map(|m| m.update)
}
/// Whether an edit ticked a patchula control: re-check, add to batch or
/// remove from batch.
pub fn controls_edited(before: &str, after: &str) -> bool {
    let controls = |text| -> BTreeSet<String> {
        markers::ticked_lines(text)
            .into_iter()
            .filter(|l| {
                l.ends_with(markers::RECHECK)
                    || l.ends_with(markers::ADD)
                    || l.contains(markers::REMOVE)
            })
            .map(str::to_owned)
            .collect()
    };
    let (before, after) = (controls(before), controls(after));
    after.difference(&before).next().is_some()
}
/// Batched closures and batch PRs never suppress an update.
pub(crate) fn suppresses(pr: &model::PullRequest) -> bool {
    let body = body(pr);
    !markers::closed(body, markers::OBSOLETE)
        && !markers::closed(body, markers::BATCHED)
        && markers::section_json::<Vec<crate::batch::Item>>(body, markers::ITEMS, markers::PAYLOAD)
            .is_none()
}
pub fn checked(pr: &model::PullRequest) -> bool {
    body(pr).matches(markers::RECHECK).count() == 1
        && markers::ticked_lines(body(pr))
            .iter()
            .any(|line| line.ends_with(markers::RECHECK))
}
pub struct Lifecycle {
    repo: String,
    actor: String,
    base: String,
    pub(crate) prs: Vec<Tracked>,
    /// Every element of the pulls listing decoded. `false` when one was
    /// unreadable: the set is then incomplete and every decision that
    /// depends on the whole set is blocked — per-plan decisions protect
    /// instead of creating, closing or holding, and `close_batched`
    /// refuses its set-wide target list. Per-element reads still apply:
    /// an unreadable PR is simply never the one acted on.
    complete: bool,
    pub actions: Vec<PullRequestAction>,
    notes: BTreeMap<(PackageId, String, String), patchula_notes::Notes>,
    pub(crate) recheck: Recheck,
    merge_settings: Option<crate::automerge::Settings>,
    maintenance_policy: patchula_config::Policy,
    maintenance_group: String,
    branch_permits: BTreeMap<String, BTreeSet<String>>,
    /// Update keys now carried by the batch PR; their PRs close as batched.
    batched: BTreeSet<String>,
    /// The server URL to advertise to the CLI, if any.
    pub(crate) server: Option<String>,
}
impl Lifecycle {
    pub fn set_advertised_server(&mut self, url: Option<String>) {
        self.server = url;
    }
    pub fn set_merge_settings(&mut self, settings: crate::automerge::Settings) {
        self.merge_settings = Some(settings);
    }
    pub fn record_merges(&mut self, plans: &[Plan], actions: Vec<PullRequestAction>) {
        for plan in plans {
            if let Some(pr) = plan.restored_pr()
                && let Some(old) = self.prs.iter_mut().find(|p| p.pr.number == pr.pr.number)
            {
                old.clone_from(pr);
            }
        }
        self.actions
            .retain(|a| !actions.iter().any(|m| m.branch == a.branch));
        self.actions.extend(actions);
    }
    pub fn dashboard_prs(&self) -> Vec<Tracked> {
        self.prs
            .iter()
            .filter(|p| owned_pr(&p.pr, &self.actor, &self.repo))
            .cloned()
            .collect()
    }
    pub(crate) fn closed_for(&self, plan: &Plan) -> Option<&Tracked> {
        let key = plan.update_key();
        self.prs.iter().find(|p| {
            owned_pr(&p.pr, &self.actor, &self.repo)
                && p.pr.state == "closed"
                && p.pr.merged_at.is_none()
                && suppresses(&p.pr)
                && marker_update(body(&p.pr)).is_some_and(|u| u == key)
        })
    }
    /// Authoritative inventory includes closed history and all pages. No cached
    /// decisions can suppress a newly closed PR or permit an over-budget create.
    /// `actor` is the run's one viewer login, read with the inventory.
    #[allow(clippy::too_many_arguments)]
    pub async fn prepare_with_controls(
        client: &Client,
        repository: &patchula_report::Repository,
        plans: &mut [Plan],
        config: &Config,
        now: DateTime<Utc>,
        recheck: Recheck,
        controls: &crate::dashboard::Controls,
        actor: &str,
    ) -> Result<Self, String> {
        let repo = repository.repository.as_str();
        let base = repository
            .commit
            .as_deref()
            .ok_or("missing repository commit")?;
        let default_branch = repository
            .default_branch
            .as_deref()
            .ok_or("missing default branch")?;
        // The listing decodes element-wise: a PR that fails stays out of
        // `items` and marks the set incomplete — see `complete`.
        let page = model::Page::<model::PullRequest>::decode(
            client
                .rest_pages(
                    &format!(
                        "repos/{repo}/pulls?state=all&sort=created&direction=desc&per_page=100"
                    ),
                    |_| false,
                )
                .await
                .map_err(|e| e.to_string())?,
        );
        let mut state = Self {
            repo: repo.into(),
            actor: actor.into(),
            base: default_branch.into(),
            prs: page.items.into_iter().map(Tracked::from).collect(),
            complete: page.complete,
            actions: Vec::new(),
            notes: BTreeMap::new(),
            recheck,
            merge_settings: None,
            maintenance_policy: config.defaults.clone(),
            maintenance_group: config.lockfiles.group.clone(),
            branch_permits: controls.branch_permits.clone(),
            batched: controls.batched.clone(),
            server: None,
        };
        let owner = view::Owner { actor, repo };
        let desired: BTreeSet<String> = plans.iter().map(|p| p.action().branch.clone()).collect();
        // Which open owned PRs are obsolete is a decision and lives with
        // the rest of them in the view; a degraded repository selects none,
        // as it skipped them all before. The closures stay requests.
        let obsolete: BTreeSet<u64> = if repository.degraded() {
            BTreeSet::new()
        } else {
            view::obsolete(
                &view::Pulls::from_listing(&state.prs, plans, controls, owner, now, state.complete),
                &desired,
                controls,
                owner,
            )
            .into_iter()
            .map(|tracked| tracked.pr.number)
            .collect()
        };
        // The repository metadata does not change between pulls, so it is
        // fetched once for all obsolete PRs, on the first that needs it.
        let mut meta = None;
        for pr in &mut state.prs {
            if !obsolete.contains(&pr.pr.number) {
                continue;
            }
            let name = branch(&pr.pr).to_owned();
            let sha = pr.pr.head.sha.clone();
            if !crate::owned(client, repo, &sha, actor).await? {
                state.actions.push(action(
                    repo,
                    &name,
                    Some(&pr.pr),
                    PullRequestStatus::Protected,
                    Some("obsolete PR branch has unowned history; left untouched".into()),
                ));
                continue;
            }
            // Refresh the pull immediately before editing to catch user changes.
            let number = pr.pr.number;
            let fresh = client
                .get_fresh::<model::PullRequest>(&format!("repos/{repo}/pulls/{number}"))
                .await
                .map_err(|e| e.to_string())?;
            if !owned_pr(&fresh, actor, repo) || fresh.head.sha != sha {
                return Err("obsolete PR changed during inspection; rerun".into());
            }
            if fresh.state != "open" {
                state.actions.push(action(
                    repo,
                    &name,
                    Some(&fresh),
                    PullRequestStatus::Skipped,
                    Some("closed-by-owner: PR closed during this run; left closed".into()),
                ));
                *pr = Tracked::from(fresh);
                continue;
            }
            // The base lease on deletion remains authoritative; don't close on
            // a stale extraction either. The repository's metadata does not
            // change between pulls, so it is fetched once for them all.
            if meta.is_none() {
                meta = Some(
                    client
                        .get_fresh::<model::Repository>(&format!("repos/{repo}"))
                        .await
                        .map_err(|e| e.to_string())?,
                );
            }
            let meta = meta.as_ref().unwrap();
            if meta.default_branch != default_branch {
                return Err("default branch changed before PR closure; rerun".into());
            }
            // The default branch's head is a lease on each closure: a base
            // that moved while earlier PRs closed must stop the next one.
            let head = client
                .get_fresh::<model::BranchHead>(&format!("repos/{repo}/branches/{default_branch}"))
                .await
                .map_err(|e| e.to_string())?;
            if head.commit.sha != base {
                return Err("base moved before obsolete PR closure; rerun".into());
            }
            let text = closure(body(&fresh), Closure::Obsolete);
            client
                .patch::<serde::de::IgnoredAny>(
                    &format!("repos/{repo}/pulls/{number}"),
                    json!({"state":"closed","body":text}),
                )
                .await
                .map_err(|e| e.to_string())?;
            pr.mark_closed(text);
            state.actions.push(action(
                repo,
                &name,
                Some(&pr.pr),
                PullRequestStatus::Closed,
                Some("superseded or already satisfied update".into()),
            ));
        }
        let pulls =
            view::Pulls::from_listing(&state.prs, plans, controls, owner, now, state.complete);
        view::admit(&pulls, plans, controls, config, owner);
        Ok(state)
    }

    pub async fn publish_with_sources(
        &mut self,
        client: &Client,
        sources: &patchula_notes::Client,
        plans: &mut [Plan],
    ) {
        let mut ranges = BTreeSet::new();
        let bodies = self.bodies();
        for plan in plans.iter().filter(|p| {
            p.restored_pr().is_none()
                && matches!(
                    p.action().status,
                    BranchStatus::Created
                        | BranchStatus::Updated
                        | BranchStatus::Rebased
                        | BranchStatus::Unchanged
                )
        }) {
            let existing = self.prs.iter().find(|p| {
                p.pr.state == "open"
                    && branch(&p.pr) == plan.action().branch
                    && same_repo(&p.pr, &self.repo)
            });
            let (_, marker) = bodies.intent(plan);
            if bodies.reusable(plan, existing.map(|t| &t.pr), &marker) {
                continue;
            }
            let mut plan_ranges = BTreeSet::new();
            for key in plan.changes().iter().filter_map(notes_key) {
                plan_ranges.insert(key);
                if plan_ranges.len() == 8 {
                    break;
                }
            }
            ranges.extend(plan_ranges);
        }
        let ranges: Vec<_> = ranges.into_iter().collect();
        sources.prepare_release_notes(&ranges).await;
        for plan in plans {
            if let Some(pr) = plan.restored_pr() {
                self.actions
                    .push(unchanged(&self.repo, &plan.action().branch, &pr.pr));
                continue;
            }
            if !matches!(
                plan.action().status,
                BranchStatus::Created
                    | BranchStatus::Updated
                    | BranchStatus::Rebased
                    | BranchStatus::Unchanged
            ) {
                continue;
            }
            if let Err(error) = self.publish_one(client, sources, plan).await {
                self.actions.push(action(
                    &self.repo,
                    &plan.action().branch,
                    None,
                    PullRequestStatus::Error,
                    Some(error),
                ));
            }
        }
    }
    /// Close each open update PR the batch now carries, once the batch PR is
    /// published. The batched marker keeps the closure from suppressing the
    /// update, so removing it from the batch brings back an individual PR.
    pub async fn close_batched(&mut self, client: &Client, plans: &[Plan]) -> Result<(), String> {
        if !self.complete {
            // The target list is read off the whole listing; an unreadable
            // PR could be a batch member or the batch PR itself.
            return Err("repository pull request list is incomplete".into());
        }
        let Some(plan) = plans.iter().find(|p| p.batch_items().is_some()) else {
            return Ok(());
        };
        if !matches!(
            plan.action().status,
            BranchStatus::Created
                | BranchStatus::Updated
                | BranchStatus::Rebased
                | BranchStatus::Unchanged
        ) {
            return Ok(());
        }
        let Some(number) = self
            .prs
            .iter()
            .find(|p| {
                p.pr.state == "open"
                    && branch(&p.pr) == plan.action().branch
                    && same_repo(&p.pr, &self.repo)
            })
            .map(|p| p.pr.number)
        else {
            return Ok(());
        };
        let targets: Vec<_> = self
            .prs
            .iter()
            .filter(|p| {
                p.pr.state == "open"
                    && branch(&p.pr) != plan.action().branch
                    && owned_pr(&p.pr, &self.actor, &self.repo)
                    && marker_update(body(&p.pr)).is_some_and(|u| self.batched.contains(&u))
            })
            .map(|p| p.pr.number)
            .collect();
        for target in targets {
            let path = format!("repos/{}/pulls/{target}", self.repo);
            let fresh = client
                .get_fresh::<model::PullRequest>(&path)
                .await
                .map_err(|e| e.to_string())?;
            if fresh.state != "open"
                || !owned_pr(&fresh, &self.actor, &self.repo)
                || !marker_update(body(&fresh)).is_some_and(|u| self.batched.contains(&u))
            {
                continue;
            }
            // Closed first: a closed PR is never visited again, so a failure
            // after this cannot repeat the comment.
            let text = closure(body(&fresh), Closure::Batched);
            let closed = client
                .patch::<model::PullRequest>(&path, json!({"state":"closed","body":text}))
                .await
                .map_err(|e| e.to_string())?;
            let mut reason = format!("moved to the batch PR #{number}");
            if let Err(error) = client
                .post::<serde::de::IgnoredAny>(
                    &format!("repos/{}/issues/{target}/comments", self.repo),
                    json!({"body":format!("Moved to the batch PR #{number}. Tick this update's box there to bring it back.")}),
                )
                .await
            {
                reason.push_str(&format!("; the note saying so was not posted: {error}"));
            }
            self.actions.push(action(
                &self.repo,
                branch(&closed),
                Some(&closed),
                PullRequestStatus::Closed,
                Some(reason),
            ));
            if let Some(old) = self.prs.iter_mut().find(|p| p.pr.number == target) {
                *old = Tracked::from(closed);
            }
        }
        Ok(())
    }
    /// A failed branch attempt is acknowledged too; its diagnostic remains in
    /// the report and the operator can explicitly request another attempt.
    pub async fn acknowledge_rechecks(&mut self, client: &Client) -> Result<(), String> {
        for pr in &mut self.prs {
            if pr.pr.state != "open"
                || !owned_pr(&pr.pr, &self.actor, &self.repo)
                || !checked(&pr.pr)
            {
                continue;
            }
            let number = pr.pr.number;
            let path = format!("repos/{}/pulls/{number}", self.repo);
            let fresh = client
                .get_fresh::<model::PullRequest>(&path)
                .await
                .map_err(|e| e.to_string())?;
            if fresh.state != "open" || !owned_pr(&fresh, &self.actor, &self.repo) {
                continue;
            }
            if checked(&fresh) {
                *pr = Tracked::from(
                    client
                        .patch::<model::PullRequest>(&path, json!({"body":unticked(body(&fresh))}))
                        .await
                        .map_err(|e| e.to_string())?,
                );
            }
        }
        Ok(())
    }
    async fn publish_one(
        &mut self,
        client: &Client,
        sources: &patchula_notes::Client,
        plan: &Plan,
    ) -> Result<(), String> {
        if self.bodies().reserved(plan) > BODY_LIMIT {
            return Err(
                "manual release-age evidence exceeds the PR body budget; split this update group"
                    .into(),
            );
        }
        let existing = self
            .prs
            .iter()
            .find(|p| {
                p.pr.state == "open"
                    && branch(&p.pr) == plan.action().branch
                    && same_repo(&p.pr, &self.repo)
            })
            .cloned();
        let text = self
            .render(sources, plan, existing.as_ref().map(|t| &t.pr))
            .await;
        let title = title(plan);
        let path = format!("repos/{}/pulls", self.repo);
        let (mut pr, status) = if let Some(pr) = existing {
            // A user can close a PR while branches are being written. Never
            // reopen it; the next inventory applies closed-target suppression.
            let number = pr.pr.number;
            let mut attempt = 0;
            let fresh = loop {
                let fresh = client
                    .get_fresh::<model::PullRequest>(&format!("{path}/{number}"))
                    .await
                    .map_err(|e| e.to_string())?;
                if !owned_pr(&fresh, &self.actor, &self.repo)
                    || branch(&fresh) != plan.action().branch
                    || !same_owner(body(&fresh), body(&pr.pr))
                {
                    return Err(
                        "PR ownership marker or repository changed during this run; left untouched"
                            .into(),
                    );
                }
                if fresh.state != "open" {
                    self.actions.push(action(
                        &self.repo,
                        &plan.action().branch,
                        Some(&fresh),
                        PullRequestStatus::Skipped,
                        Some("closed-by-owner: PR closed during this run; left closed".into()),
                    ));
                    if let Some(old) = self.prs.iter_mut().find(|p| p.pr.number == number) {
                        *old = Tracked::from(fresh);
                    }
                    return Ok(());
                }
                if fresh.head.sha == plan.action().commit.as_deref().unwrap_or("") {
                    break fresh;
                }
                // Only the exact head verified before our publication is an
                // expected stale value. Never retry an unfamiliar commit.
                if fresh.head.sha != plan.action().previous_commit.as_deref().unwrap_or("") {
                    return Err(
                        "PR head moved after branch publication to an unexpected commit; rerun"
                            .into(),
                    );
                }
                if attempt == 6 {
                    return Err("PR head has not reflected our published commit after bounded retries; rerun".into());
                }
                tokio::time::sleep(std::time::Duration::from_millis(100 << attempt)).await;
                attempt += 1;
            };
            // Keep what others wrote as of this read, not the inventory's.
            let text = with_foreign(text, body(&fresh));
            if fresh.title == title
                && body_hash(body(&fresh)) == body_hash(&text)
                && fresh.base.branch == self.base
            {
                (Tracked::from(fresh), PullRequestStatus::Unchanged)
            } else {
                let updated = client
                    .patch::<model::PullRequest>(
                        &format!("{path}/{number}"),
                        json!({"title":title,"body":text,"base":self.base}),
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                (Tracked::from(updated), PullRequestStatus::Updated)
            }
        } else {
            let created = client.post::<model::PullRequest>(&path,json!({"title":title,"body":text,"head":plan.action().branch,"base":self.base,"maintainer_can_modify":false})).await.map_err(|e|e.to_string())?;
            (Tracked::from(created), PullRequestStatus::Created)
        };
        let number = pr.pr.number;
        let labels: BTreeSet<_> = plan
            .changes()
            .iter()
            .flat_map(|c| c.labels.iter().cloned())
            .chain(
                plan.is_maintenance()
                    .then_some(&self.maintenance_policy.labels)
                    .into_iter()
                    .flatten()
                    .cloned(),
            )
            .collect();
        let existing: BTreeSet<_> = pr.pr.labels.iter().map(|l| l.name.clone()).collect();
        if !labels.is_subset(&existing) {
            let labels: BTreeSet<_> = labels.union(&existing).cloned().collect();
            client
                .put::<serde::de::IgnoredAny>(
                    &format!("repos/{}/issues/{number}/labels", self.repo),
                    json!({"labels":labels}),
                )
                .await
                .map_err(|e| e.to_string())?;
            pr.set_labels(labels);
        }
        let merge = merge_requested(plan, &self.maintenance_policy);
        let enabled = pr.pr.auto_merge.is_some();
        let mut native = enabled;
        if merge && !enabled {
            if self.merge_settings.is_none() {
                let meta = client
                    .get_fresh::<model::Repository>(&format!("repos/{}", self.repo))
                    .await
                    .map_err(|e| e.to_string())?;
                self.merge_settings = Some(crate::automerge::Settings::from_rest(&meta));
            }
            let settings = self.merge_settings.as_ref().expect("repository settings");
            if settings.native {
                let method = settings
                    .method
                    .as_ref()
                    .ok_or("repository has no enabled merge method")?;
                // A PR that is already mergeable cannot take auto-merge; the
                // Count's own merge handles it on a later run.
                native = crate::automerge::enable_native(
                    client,
                    &pr.pr.node_id,
                    method,
                    &json!(plan.action().commit),
                )
                .await?
                    == crate::automerge::Native::Enabled;
            }
        } else if !merge && enabled {
            client
                .graphql::<DisableAutoMergeData>(
                    DISABLE_AUTO_MERGE,
                    json!({"input":{"pullRequestId":pr.pr.node_id}}),
                )
                .await
                .map_err(|e| e.to_string())?;
        }
        pr.set_auto_merge(merge && native);
        if merge && !native {
            pr.set_merge_note(Some(MergeNote::Waiting));
        } else if merge && pr.pr.auto_merge.is_some() {
            pr.set_merge_note(Some(MergeNote::Native));
        }
        self.actions.push(action(
            &self.repo,
            &plan.action().branch,
            Some(&pr.pr),
            status,
            None,
        ));
        pr.mark_open();
        if let Some(old) = self.prs.iter_mut().find(|p| p.pr.number == number) {
            *old = pr;
        } else {
            self.prs.push(pr);
        }
        Ok(())
    }

    /// The fields body writing reads, as one value. `render` spells the
    /// literal out instead of calling this: a `&self` borrow would cover
    /// `self.notes`, which the fetch loop writes between pushes.
    fn bodies(&self) -> Bodies<'_> {
        Bodies {
            recheck: self.recheck,
            server: self.server.as_deref(),
            branch_permits: &self.branch_permits,
            maintenance_group: &self.maintenance_group,
        }
    }
    /// The body a plan's PR carries: the one it has, when nothing it
    /// records changed, or a fresh draft the run's fetches fill.
    async fn render(
        &mut self,
        sources: &patchula_notes::Client,
        plan: &Plan,
        existing: Option<&model::PullRequest>,
    ) -> String {
        let bodies = Bodies {
            recheck: self.recheck,
            server: self.server.as_deref(),
            branch_permits: &self.branch_permits,
            maintenance_group: &self.maintenance_group,
        };
        let (_, marker) = bodies.intent(plan);
        if let Some(pr) = existing.filter(|_| bodies.reusable(plan, existing, &marker)) {
            // Rebases, label reconciliation and receipt repair do not change
            // body inputs. Reuse their notes without metadata or network reads.
            return body(pr).into();
        }
        let mut draft = bodies.draft(plan);
        while let Some(key) = draft.next() {
            if !self.notes.contains_key(&key) {
                let note = sources
                    .release_notes(&key.0, &key.1, &key.2)
                    .await
                    .unwrap_or_default();
                self.notes.insert(key.clone(), note);
            }
            draft.push(&self.notes[&key]);
        }
        draft.finish()
    }
}

#[cfg(test)]
mod merge_tests {
    use super::*;
    use patchula_report::BranchStatus;

    fn action(branch: &str) -> patchula_report::BranchAction {
        patchula_report::BranchAction {
            repository: "o/r".into(),
            branch: branch.into(),
            base_commit: "b".repeat(40),
            status: BranchStatus::Planned,
            reason: None,
            message: "m".into(),
            previous_commit: None,
            commit: None,
            verified: None,
            lockfile_pending: false,
            files: Vec::new(),
        }
    }
    fn change(merge: bool) -> crate::Change {
        crate::Change {
            source: patchula_words::Source::Npm,
            package: "a".into(),
            registry: None,
            file: "package.json".into(),
            current: "1.0.0".into(),
            current_version: None,
            value: Some("1.1.0".into()),
            version: Some("1.1.0".into()),
            digest: None,
            kind: patchula_words::Update::Minor,
            merge,
            labels: Vec::new(),
            release_notes: false,
            body_notes: Vec::new(),
            advisories: Vec::new(),
            image: None,
        }
    }

    /// Maintenance follows the repository's merge policy alone; other plans
    /// merge only when every change opts in, and never with a lockfile pending.
    #[test]
    fn merge_requests_follow_policy_opt_ins_and_lockfile_readiness() {
        let mut config = patchula_config::defaults().unwrap();
        let plan = crate::plan::testing::Draft::new(action("patchula/mise"))
            .maintenance()
            .build();
        config.defaults.merge = true;
        assert!(merge_requested(&plan, &config.defaults));
        config.defaults.merge = false;
        assert!(!merge_requested(&plan, &config.defaults));
        // An ordinary plan merges only when every change asks for it.
        let plan = crate::plan::testing::Draft::from(plan).update().build();
        config.defaults.merge = true;
        assert!(!merge_requested(&plan, &config.defaults));
        let plan = crate::plan::testing::Draft::from(plan)
            .changes(vec![change(true)])
            .build();
        assert!(merge_requested(&plan, &config.defaults));
        let plan = crate::plan::testing::Draft::from(plan)
            .change(change(false))
            .build();
        assert!(!merge_requested(&plan, &config.defaults));
        // A pending lockfile never merges, policy or opt-ins regardless.
        let mut pending_action = action("patchula/b");
        pending_action.lockfile_pending = true;
        let pending = crate::plan::testing::Draft::new(pending_action)
            .maintenance()
            .build();
        assert!(!merge_requested(&pending, &config.defaults));
    }
}

#[cfg(test)]
mod tracked_tests {
    use super::*;

    fn node(labels: serde_json::Value) -> crate::receipts::InventoryPr {
        serde_json::from_value(serde_json::json!({
            "number": 1, "id": "PR_node1",
            "url": "https://github.com/o/r/pull/1",
            "title": "chore(deps): bump serde", "body": "Bumps.",
            "headRefName": "patchula/serde-2.x", "headRefOid": "s",
            "baseRefName": "main",
            "author": {"__typename": "Bot", "login": "patchula"},
            "headRepository": {"nameWithOwner": "o/r"},
            "baseRepository": {"nameWithOwner": "o/r"},
            "labels": labels,
            "autoMergeRequest": null
        }))
        .expect("a complete inventory pull request")
    }

    /// A PR whose label page is missing or unreadable is not a lifecycle
    /// pull: it must not settle, restore, batch or render as though the
    /// labels were absent.
    #[test]
    fn an_inventory_pr_with_incomplete_labels_is_untracked() {
        assert!(tracked_pr(&node(serde_json::Value::Null)).is_none());
        assert!(
            tracked_pr(&node(serde_json::json!({
                "nodes": [{"name": "dependencies"}, null],
                "pageInfo": {"hasNextPage": false, "endCursor": null}
            })))
            .is_none()
        );
        assert!(
            tracked_pr(&node(serde_json::json!({
                "nodes": [{"name": "dependencies"}],
                "pageInfo": {"hasNextPage": true, "endCursor": "c"}
            })))
            .is_none()
        );
        let tracked = tracked_pr(&node(serde_json::json!({
            "nodes": [{"name": "dependencies"}],
            "pageInfo": {"hasNextPage": false, "endCursor": null}
        })))
        .unwrap();
        assert_eq!(tracked.pr.labels[0].name, "dependencies");
        assert_eq!(tracked.pr.user.unwrap().login, "patchula[bot]");
    }
}
