use crate::{Plan, plan::hash};
use chrono::{DateTime, Duration, Utc};
use patchula_config::Config;
use patchula_github::Client;
use patchula_report::{ActionStatus, PullRequestAction};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const BODY_LIMIT: usize = 32_000;
const MARKER: &str = "<!-- patchula:";
const RECHECK: &str = "<!-- patchula-recheck -->";
const OBSOLETE: &str = "<!-- patchula-closed:obsolete -->";
pub(crate) const CLOSED_BY_OWNER: &str =
    "closed-by-owner: this update was manually closed; waiting for a newer target";
pub const ENABLE_AUTO_MERGE: &str = "mutation($input:EnablePullRequestAutoMergeInput!){enablePullRequestAutoMerge(input:$input){pullRequest{id}}}";
const DISABLE_AUTO_MERGE: &str = "mutation($input:DisablePullRequestAutoMergeInput!){disablePullRequestAutoMerge(input:$input){pullRequest{id}}}";

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub enum Recheck {
    Now,
    #[default]
    NextRun,
}
#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Marker {
    version: u32,
    identity: String,
    update: String,
}
fn marker(body: &str) -> Option<Marker> {
    let (_, rest) = body.split_once(MARKER)?;
    if rest.contains(MARKER) {
        return None;
    }
    let (json, _) = rest.split_once(" -->")?;
    if json.len() > 1024 {
        return None;
    }
    let m: Marker = serde_json::from_str(json).ok()?;
    let valid_hash = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
    (m.version == 1 && valid_hash(&m.identity) && valid_hash(&m.update)).then_some(m)
}
/// Target identity intentionally excludes current/base/file/message so a closed
/// update stays closed after a rebase, policy wording change or new occurrence.
pub fn update_key(plan: &Plan) -> String {
    if plan.maintenance {
        return hash(format!("maintenance:{}", plan.input_fingerprint).as_bytes());
    }
    let targets: BTreeSet<_> = plan
        .changes
        .iter()
        .map(|c| (&c.source, &c.package, &c.value, &c.version, &c.digest))
        .collect();
    hash(
        serde_json::to_string(&targets)
            .expect("serializable targets")
            .as_bytes(),
    )
}
fn body(pr: &Value) -> &str {
    pr["body"].as_str().unwrap_or("")
}
/// Opens the part of the description the Count writes. His ownership marker
/// closes it. Whatever else the description holds, before or after, belongs to
/// someone else: a person, or a tool that edits descriptions.
const START: &str = "<!-- patchula-begin -->";

/// A body split by the Count's markers: the text before his section, the
/// section itself, and the text after it. A body with no start marker (one
/// written before it existed) is his from its first byte.
fn regions(body: &str) -> Option<(&str, &str, &str)> {
    marker(body)?;
    let (before, rest) = body.split_once(MARKER)?;
    let (json, _) = rest.split_once(" -->")?;
    let end = before.len() + MARKER.len() + json.len() + " -->".len();
    let start = if before.matches(START).count() == 1 {
        before.find(START).unwrap_or(0)
    } else {
        0
    };
    Some((&body[..start], &body[start..end], &body[end..]))
}
/// The Count's rendering between whatever the PR's body has right now before
/// and after his section. Only the section of `text` is used, so a text that
/// was itself taken from an existing body, or from an older read, never
/// doubles or loses what others wrote, and a body he leaves alone comes out
/// byte for byte the same. A foreign edit inside his section is overwritten.
fn with_foreign(text: String, current: &str) -> String {
    let (Some((_, section, _)), Some((before, _, after))) = (regions(&text), regions(current))
    else {
        return text;
    };
    if before.len() + section.len() + after.len() > FOREIGN_LIMIT {
        tracing::warn!(
            foreign = before.len() + after.len(),
            "text others wrote in a PR body no longer fits beside the Count's own; dropped"
        );
        return format!("{section}\n");
    }
    format!("{before}{section}{after}")
}
/// GitHub's pull request body limit (65,536 characters), with room to spare.
/// The Count's own section stays within `BODY_LIMIT`, so more than 30,000
/// bytes of other people's text fit beside it.
const FOREIGN_LIMIT: usize = 65_000;
pub(crate) fn body_hash(text: &str) -> String {
    // GitHub may return CRLF for a body published with LF. Compare the same
    // content hash as write receipts, without treating transport newlines as edits.
    hash(text.replace("\r\n", "\n").as_bytes())
}
fn branch(pr: &Value) -> &str {
    pr["head"]["ref"].as_str().unwrap_or("")
}
fn same_repo(pr: &Value, repo: &str) -> bool {
    pr["head"]["repo"]["full_name"]
        .as_str()
        .is_some_and(|s| s.eq_ignore_ascii_case(repo))
        && pr["base"]["repo"]["full_name"]
            .as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case(repo))
}
pub fn owned_pr(pr: &Value, actor: &str, repo: &str) -> bool {
    pr["user"]["login"] == actor && same_repo(pr, repo) && marker(body(pr)).is_some()
}
fn action(
    repo: &str,
    name: &str,
    pr: Option<&Value>,
    status: ActionStatus,
    reason: Option<String>,
) -> PullRequestAction {
    PullRequestAction {
        repository: repo.into(),
        branch: name.into(),
        number: pr.and_then(|p| p["number"].as_u64()),
        url: pr.and_then(|p| p["html_url"].as_str()).map(str::to_owned),
        status,
        reason,
    }
}
fn limit(plan: &mut Plan, reason: &str) {
    plan.action.status = ActionStatus::Skipped;
    plan.action.reason = Some(reason.into());
}
/// Ticked checkbox lines outside fenced code, trimmed.
pub(crate) fn ticked_lines(text: &str) -> Vec<&str> {
    let mut fence = None;
    let mut out = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with("```") || line.starts_with("~~~") {
            let kind = &line[..3];
            if fence == Some(kind) {
                fence = None;
            } else if fence.is_none() {
                fence = Some(kind);
            }
            continue;
        }
        if fence.is_none() && (line.starts_with("- [x] ") || line.starts_with("- [X] ")) {
            out.push(line);
        }
    }
    out
}
/// The update key recorded by a patchula PR's ownership marker.
pub(crate) fn marker_update(text: &str) -> Option<String> {
    marker(text).map(|m| m.update)
}
/// Whether an edit ticked a patchula control: re-check, add to batch or
/// remove from batch.
pub fn controls_edited(before: &str, after: &str) -> bool {
    let controls = |text| -> BTreeSet<String> {
        ticked_lines(text)
            .into_iter()
            .filter(|l| {
                l.ends_with(RECHECK)
                    || l.ends_with(crate::batch::ADD)
                    || l.contains(crate::batch::REMOVE)
            })
            .map(str::to_owned)
            .collect()
    };
    let (before, after) = (controls(before), controls(after));
    after.difference(&before).next().is_some()
}
/// Batched closures and batch PRs never suppress an update.
fn suppresses(pr: &Value) -> bool {
    !body(pr).contains(OBSOLETE)
        && !body(pr).contains(crate::batch::BATCHED)
        && !body(pr).contains(crate::batch::ITEMS)
}
pub fn checked(pr: &Value) -> bool {
    if body(pr).matches(RECHECK).count() != 1 {
        return false;
    }
    let mut fence = None;
    for line in body(pr).lines().map(str::trim) {
        if line.starts_with("```") || line.starts_with("~~~") {
            let kind = &line[..3];
            if fence == Some(kind) {
                fence = None;
            } else if fence.is_none() {
                fence = Some(kind);
            }
            continue;
        }
        if fence.is_none()
            && line.ends_with(RECHECK)
            && (line.starts_with("- [x] ") || line.starts_with("- [X] "))
        {
            return true;
        }
    }
    false
}
fn untick(text: &str) -> String {
    text.split_inclusive('\n')
        .map(|line| {
            if line.trim_end().ends_with(RECHECK) {
                line.replacen("- [x]", "- [ ]", 1)
                    .replacen("- [X]", "- [ ]", 1)
            } else {
                line.into()
            }
        })
        .collect()
}

pub struct Lifecycle {
    repo: String,
    actor: String,
    base: String,
    pub(crate) prs: Vec<Value>,
    pub actions: Vec<PullRequestAction>,
    notes: BTreeMap<(String, String, String, String), patchula_datasource::notes::Notes>,
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
            if let Some(pr) = &plan.cached_pr
                && let Some(old) = self.prs.iter_mut().find(|p| p["number"] == pr["number"])
            {
                old.clone_from(pr);
            }
        }
        self.actions
            .retain(|a| !actions.iter().any(|m| m.branch == a.branch));
        self.actions.extend(actions);
    }
    pub fn dashboard_prs(&self) -> Vec<Value> {
        self.prs
            .iter()
            .filter(|p| owned_pr(p, &self.actor, &self.repo))
            .cloned()
            .collect()
    }
    pub(crate) fn closed_for(&self, plan: &Plan) -> Option<&Value> {
        let key = update_key(plan);
        self.prs.iter().find(|p| {
            owned_pr(p, &self.actor, &self.repo)
                && p["state"] == "closed"
                && p["merged_at"].is_null()
                && suppresses(p)
                && marker(body(p)).is_some_and(|m| m.update == key)
        })
    }
    /// Authoritative inventory includes closed history and all pages. No cached
    /// decisions can suppress a newly closed PR or permit an over-budget create.
    pub async fn prepare(
        client: &Client,
        repository: &patchula_report::Repository,
        plans: &mut [Plan],
        config: &Config,
        now: DateTime<Utc>,
        recheck: Recheck,
    ) -> Result<Self, String> {
        Self::prepare_with_controls(
            client,
            repository,
            plans,
            config,
            now,
            recheck,
            &Default::default(),
        )
        .await
    }
    pub async fn prepare_with_controls(
        client: &Client,
        repository: &patchula_report::Repository,
        plans: &mut [Plan],
        config: &Config,
        now: DateTime<Utc>,
        recheck: Recheck,
        controls: &crate::dashboard::Controls,
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
        let prs: Vec<Value> = client
            .rest_pages(
                &format!("repos/{repo}/pulls?state=all&sort=created&direction=desc&per_page=100"),
                |_| false,
            )
            .await
            .map_err(|e| e.to_string())?;
        let viewer = client
            .graphql("query{viewer{login}}", json!({}))
            .await
            .map_err(|e| e.to_string())?;
        let actor = viewer["viewer"]["login"]
            .as_str()
            .ok_or("missing viewer login")?;
        let mut state = Self {
            repo: repo.into(),
            actor: actor.into(),
            base: default_branch.into(),
            prs,
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
        let desired: BTreeSet<_> = plans.iter().map(|p| p.action.branch.as_str()).collect();
        for pr in &mut state.prs {
            if repository.degraded()
                || pr["state"] != "open"
                || !owned_pr(pr, actor, repo)
                || desired.contains(branch(pr))
                || marker(body(pr)).is_some_and(|m| state.batched.contains(&m.update))
            {
                continue;
            }
            let name = branch(pr).to_owned();
            let sha = pr["head"]["sha"].as_str().ok_or("missing PR head")?;
            if !crate::owned(client, repo, sha, actor).await? {
                state.actions.push(action(
                    repo,
                    &name,
                    Some(pr),
                    ActionStatus::Protected,
                    Some("obsolete PR branch has unowned history; left untouched".into()),
                ));
                continue;
            }
            // Refresh the pull immediately before editing to catch user changes.
            let number = pr["number"].as_u64().ok_or("missing PR number")?;
            let fresh = client
                .get_fresh(&format!("repos/{repo}/pulls/{number}"))
                .await
                .map_err(|e| e.to_string())?;
            if !owned_pr(&fresh, actor, repo) || fresh["head"]["sha"] != sha {
                return Err("obsolete PR changed during inspection; rerun".into());
            }
            if fresh["state"] != "open" {
                state.actions.push(action(
                    repo,
                    &name,
                    Some(&fresh),
                    ActionStatus::Skipped,
                    Some("closed-by-owner: PR closed during this run; left closed".into()),
                ));
                *pr = fresh;
                continue;
            }
            // The base lease on deletion remains authoritative; don't close on
            // a stale extraction either.
            let meta = client
                .get_fresh(&format!("repos/{repo}"))
                .await
                .map_err(|e| e.to_string())?;
            if meta["default_branch"] != default_branch {
                return Err("default branch changed before PR closure; rerun".into());
            }
            let current = client
                .get_fresh(&format!("repos/{repo}/branches/{default_branch}"))
                .await
                .map_err(|e| e.to_string())?;
            if current["commit"]["sha"] != base {
                return Err("base moved before obsolete PR closure; rerun".into());
            }
            let mut text = truncate(body(&fresh), FOREIGN_LIMIT - 100);
            text.push_str("\n\n");
            text.push_str(OBSOLETE);
            client
                .patch(
                    &format!("repos/{repo}/pulls/{number}"),
                    json!({"state":"closed","body":text}),
                )
                .await
                .map_err(|e| e.to_string())?;
            pr["state"] = json!("closed");
            pr["body"] = json!(text);
            state.actions.push(action(
                repo,
                &name,
                Some(pr),
                ActionStatus::Closed,
                Some("superseded or already satisfied update".into()),
            ));
        }
        let owned: Vec<_> = state
            .prs
            .iter()
            .filter(|p| owned_pr(p, actor, repo))
            .collect();
        let mut active = owned.iter().filter(|p| p["state"] == "open").count() as u32;
        let mut hourly = owned
            .iter()
            .filter(|p| {
                p["created_at"]
                    .as_str()
                    .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                    .is_some_and(|t| t >= now - Duration::hours(1))
            })
            .count() as u32;
        for plan in plans
            .iter_mut()
            .filter(|p| p.action.status == ActionStatus::Planned)
        {
            let name = &plan.action.branch;
            let existing: Vec<_> = state
                .prs
                .iter()
                .filter(|p| p["state"] == "open" && branch(p) == name && same_repo(p, repo))
                .collect();
            if existing.len() > 1 || existing.first().is_some_and(|p| !owned_pr(p, actor, repo)) {
                plan.action.status = ActionStatus::Protected;
                plan.action.reason = Some("unowned or ambiguous open PR for branch".into());
                continue;
            }
            let key = update_key(plan);
            if existing.is_empty()
                && !controls.recreate.contains(&key)
                && owned.iter().any(|p| {
                    p["state"] == "closed"
                        && p["merged_at"].is_null()
                        && suppresses(p)
                        && marker(body(p)).is_some_and(|m| m.update == key)
                })
            {
                limit(plan, CLOSED_BY_OWNER);
                continue;
            }
            if let Some(pr) = existing.first() {
                // A newer target must not take over a PR that just moved into
                // the batch; it gets its own PR once that one closes.
                if marker(body(pr)).is_some_and(|m| state.batched.contains(&m.update)) {
                    limit(plan, "waiting for its batched PR to close");
                    continue;
                }
                plan.retry |= checked(pr);
                continue;
            }
            // Security updates never queue behind ordinary ones.
            let bypass = controls.create.contains(&key)
                || controls.recreate.contains(&key)
                || plan.changes.iter().any(|c| !c.advisories.is_empty());
            if !bypass && config.prs.concurrent.is_some_and(|n| active >= n) {
                limit(plan, "prs.concurrent limit reached");
                continue;
            }
            if !bypass && config.prs.per_hour.is_some_and(|n| hourly >= n) {
                limit(plan, "prs.perHour limit reached");
                continue;
            }
            active = active.saturating_add(1);
            hourly = hourly.saturating_add(1);
        }
        Ok(state)
    }

    pub async fn publish(&mut self, client: &Client, plans: &mut [Plan]) {
        let sources =
            patchula_datasource::Client::new(client.clone(), client.public_http(), client.cache())
                .notes_batch();
        self.publish_with_sources(client, &sources, plans).await;
    }
    pub async fn publish_with_sources(
        &mut self,
        client: &Client,
        sources: &patchula_datasource::Client,
        plans: &mut [Plan],
    ) {
        let mut ranges = BTreeSet::new();
        for plan in plans.iter().filter(|p| {
            p.cached_pr.is_none()
                && matches!(
                    p.action.status,
                    ActionStatus::Created
                        | ActionStatus::Updated
                        | ActionStatus::Rebased
                        | ActionStatus::Unchanged
                )
        }) {
            let existing = self.prs.iter().find(|p| {
                p["state"] == "open" && branch(p) == plan.action.branch && same_repo(p, &self.repo)
            });
            let (_, marker) = self.body_intent(plan);
            if self.reusable_body(plan, existing, &marker) {
                continue;
            }
            let mut plan_ranges = BTreeSet::new();
            for change in plan
                .changes
                .iter()
                .filter(|c| c.kind != "pin" && c.release_notes)
            {
                if let Some(target) = change.version.as_ref().or(change.value.as_ref()) {
                    plan_ranges.insert((
                        change.source.clone(),
                        change.package.clone(),
                        change
                            .current_version
                            .as_ref()
                            .unwrap_or(&change.current)
                            .clone(),
                        target.clone(),
                    ));
                    if plan_ranges.len() == 8 {
                        break;
                    }
                }
            }
            ranges.extend(plan_ranges);
        }
        let ranges: Vec<_> = ranges
            .into_iter()
            .filter_map(|(source, package, current, target)| {
                Some((
                    serde_json::from_value(json!(source)).ok()?,
                    package,
                    current,
                    target,
                ))
            })
            .collect();
        sources.prepare_release_notes(&ranges).await;
        for plan in plans {
            if let Some(pr) = &plan.cached_pr {
                self.actions.push(action(
                    &self.repo,
                    &plan.action.branch,
                    Some(pr),
                    ActionStatus::Unchanged,
                    None,
                ));
                continue;
            }
            if !matches!(
                plan.action.status,
                ActionStatus::Created
                    | ActionStatus::Updated
                    | ActionStatus::Rebased
                    | ActionStatus::Unchanged
            ) {
                continue;
            }
            if let Err(error) = self.publish_one(client, sources, plan).await {
                self.actions.push(action(
                    &self.repo,
                    &plan.action.branch,
                    None,
                    ActionStatus::Error,
                    Some(error),
                ));
            }
        }
    }
    /// Close each open update PR the batch now carries, once the batch PR is
    /// published. The batched marker keeps the closure from suppressing the
    /// update, so removing it from the batch brings back an individual PR.
    pub async fn close_batched(&mut self, client: &Client, plans: &[Plan]) -> Result<(), String> {
        let Some(plan) = plans.iter().find(|p| p.batch.is_some()) else {
            return Ok(());
        };
        if !matches!(
            plan.action.status,
            ActionStatus::Created
                | ActionStatus::Updated
                | ActionStatus::Rebased
                | ActionStatus::Unchanged
        ) {
            return Ok(());
        }
        let Some(number) = self
            .prs
            .iter()
            .find(|p| {
                p["state"] == "open" && branch(p) == plan.action.branch && same_repo(p, &self.repo)
            })
            .and_then(|p| p["number"].as_u64())
        else {
            return Ok(());
        };
        let targets: Vec<_> = self
            .prs
            .iter()
            .filter(|p| {
                p["state"] == "open"
                    && branch(p) != plan.action.branch
                    && owned_pr(p, &self.actor, &self.repo)
                    && marker(body(p)).is_some_and(|m| self.batched.contains(&m.update))
            })
            .filter_map(|p| p["number"].as_u64())
            .collect();
        for target in targets {
            let path = format!("repos/{}/pulls/{target}", self.repo);
            let fresh = client.get_fresh(&path).await.map_err(|e| e.to_string())?;
            if fresh["state"] != "open"
                || !owned_pr(&fresh, &self.actor, &self.repo)
                || !marker(body(&fresh)).is_some_and(|m| self.batched.contains(&m.update))
            {
                continue;
            }
            client
                .post(
                    &format!("repos/{}/issues/{target}/comments", self.repo),
                    json!({"body":format!("Batched by the Count into #{number}. Tick this update's box there to bring it back.")}),
                )
                .await
                .map_err(|e| e.to_string())?;
            let mut text = truncate(body(&fresh), FOREIGN_LIMIT - 100);
            text.push_str("\n\n");
            text.push_str(crate::batch::BATCHED);
            let closed = client
                .patch(&path, json!({"state":"closed","body":text}))
                .await
                .map_err(|e| e.to_string())?;
            self.actions.push(action(
                &self.repo,
                branch(&closed),
                Some(&closed),
                ActionStatus::Closed,
                Some(format!("moved to the batch PR #{number}")),
            ));
            if let Some(old) = self.prs.iter_mut().find(|p| p["number"] == target) {
                *old = closed;
            }
        }
        Ok(())
    }
    /// A failed branch attempt is acknowledged too; its diagnostic remains in
    /// the report and the operator can explicitly request another attempt.
    pub async fn acknowledge_rechecks(&mut self, client: &Client) -> Result<(), String> {
        for pr in &mut self.prs {
            if pr["state"] != "open" || !owned_pr(pr, &self.actor, &self.repo) || !checked(pr) {
                continue;
            }
            let number = pr["number"].as_u64().ok_or("missing PR number")?;
            let path = format!("repos/{}/pulls/{number}", self.repo);
            let fresh = client.get_fresh(&path).await.map_err(|e| e.to_string())?;
            if fresh["state"] != "open" || !owned_pr(&fresh, &self.actor, &self.repo) {
                continue;
            }
            if checked(&fresh) {
                *pr = client
                    .patch(&path, json!({"body":untick(body(&fresh))}))
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }
    async fn publish_one(
        &mut self,
        client: &Client,
        sources: &patchula_datasource::Client,
        plan: &Plan,
    ) -> Result<(), String> {
        if self
            .branch_permits
            .get(&plan.action.branch)
            .is_some_and(|permits| {
                crate::dashboard::permit_marker(permits).len() + 1000 > BODY_LIMIT
            })
        {
            return Err(
                "manual release-age evidence exceeds the PR body budget; split this update group"
                    .into(),
            );
        }
        let existing = self
            .prs
            .iter()
            .find(|p| {
                p["state"] == "open" && branch(p) == plan.action.branch && same_repo(p, &self.repo)
            })
            .cloned();
        let text = self.render(sources, plan, existing.as_ref()).await;
        let title = truncate(&plan.action.message.replace(['\n', '\r'], " "), 250);
        let path = format!("repos/{}/pulls", self.repo);
        let (mut pr, status) = if let Some(pr) = existing {
            // A user can close a PR while branches are being written. Never
            // reopen it; the next inventory applies closed-target suppression.
            let number = pr["number"].as_u64().ok_or("missing PR number")?;
            let mut attempt = 0;
            let fresh = loop {
                let fresh = client
                    .get_fresh(&format!("{path}/{number}"))
                    .await
                    .map_err(|e| e.to_string())?;
                if !owned_pr(&fresh, &self.actor, &self.repo)
                    || branch(&fresh) != plan.action.branch
                    || marker(body(&fresh)) != marker(body(&pr))
                {
                    return Err(
                        "PR ownership marker or repository changed during this run; left untouched"
                            .into(),
                    );
                }
                if fresh["state"] != "open" {
                    self.actions.push(action(
                        &self.repo,
                        &plan.action.branch,
                        Some(&fresh),
                        ActionStatus::Skipped,
                        Some("closed-by-owner: PR closed during this run; left closed".into()),
                    ));
                    if let Some(old) = self.prs.iter_mut().find(|p| p["number"] == number) {
                        *old = fresh;
                    }
                    return Ok(());
                }
                if fresh["head"]["sha"] == plan.action.commit.as_deref().unwrap_or("") {
                    break fresh;
                }
                // Only the exact head verified before our publication is an
                // expected stale value. Never retry an unfamiliar commit.
                if fresh["head"]["sha"] != plan.action.previous_commit.as_deref().unwrap_or("") {
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
            if fresh["title"] == title
                && body_hash(body(&fresh)) == body_hash(&text)
                && fresh["base"]["ref"] == self.base
            {
                (fresh, ActionStatus::Unchanged)
            } else {
                let updated = client
                    .patch(
                        &format!("{path}/{number}"),
                        json!({"title":title,"body":text,"base":self.base}),
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                (updated, ActionStatus::Updated)
            }
        } else {
            let created = client.post(&path,json!({"title":title,"body":text,"head":plan.action.branch,"base":self.base,"maintainer_can_modify":false})).await.map_err(|e|e.to_string())?;
            (created, ActionStatus::Created)
        };
        let number = pr["number"].as_u64().ok_or("missing created PR number")?;
        let labels: BTreeSet<_> = plan
            .changes
            .iter()
            .flat_map(|c| c.labels.iter().cloned())
            .chain(
                plan.maintenance
                    .then_some(&self.maintenance_policy.labels)
                    .into_iter()
                    .flatten()
                    .cloned(),
            )
            .collect();
        let existing: BTreeSet<_> = pr["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l["name"].as_str().map(str::to_owned))
            .collect();
        if !labels.is_subset(&existing) {
            let labels: BTreeSet<_> = labels.union(&existing).cloned().collect();
            client
                .put(
                    &format!("repos/{}/issues/{number}/labels", self.repo),
                    json!({"labels":labels}),
                )
                .await
                .map_err(|e| e.to_string())?;
            pr["labels"] = json!(
                labels
                    .iter()
                    .map(|name| json!({"name":name}))
                    .collect::<Vec<_>>()
            );
        }
        let merge = !plan.action.lockfile_pending
            && if plan.maintenance {
                self.maintenance_policy.merge
            } else {
                !plan.changes.is_empty() && plan.changes.iter().all(|c| c.merge)
            };
        let enabled = !pr["auto_merge"].is_null();
        let mut native = enabled;
        if merge && !enabled {
            if self.merge_settings.is_none() {
                let meta = client
                    .get_fresh(&format!("repos/{}", self.repo))
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
                let id = pr["node_id"].as_str().ok_or("missing PR node ID")?;
                // A PR that is already mergeable cannot take auto-merge; the
                // Count's own merge handles it on a later run.
                native = crate::automerge::enable_native(
                    client,
                    &json!(id),
                    method,
                    &json!(plan.action.commit),
                )
                .await?
                    == crate::automerge::Native::Enabled;
            }
        } else if !merge && enabled {
            let id = pr["node_id"].as_str().ok_or("missing PR node ID")?;
            client
                .graphql(DISABLE_AUTO_MERGE, json!({"input":{"pullRequestId":id}}))
                .await
                .map_err(|e| e.to_string())?;
        }
        pr["auto_merge"] = if merge && native {
            json!({"enabled":true})
        } else {
            Value::Null
        };
        if merge && !native {
            pr["patchula_merge"] = json!(
                "auto-merge waiting for a later run with a verified receipt and passing checks"
            );
        } else if merge && !pr["auto_merge"].is_null() {
            pr["patchula_merge"] = json!("native auto-merge enabled");
        }
        self.actions.push(action(
            &self.repo,
            &plan.action.branch,
            Some(&pr),
            status,
            None,
        ));
        pr["state"] = json!("open");
        if let Some(old) = self.prs.iter_mut().find(|p| p["number"] == number) {
            *old = pr;
        } else {
            self.prs.push(pr);
        }
        Ok(())
    }

    fn body_intent(&self, plan: &Plan) -> (String, String) {
        let mut text = tooling_section(&plan.tooling);
        text.push_str(&security_section(&plan.changes));
        text.push_str(&image_section(&plan.changes));
        text.push_str("| Package | Change | Type | File |\n| --- | --- | --- | --- |\n");
        if plan.maintenance {
            text.push_str(&format!(
                "\nScheduled {} using the repository's mise tool versions.\n",
                safe(&self.maintenance_group)
            ));
            for file in &plan.action.files {
                append(&mut text, &format!("\n- {}", cell(&file.path)), 18_000);
            }
        }
        for c in &plan.changes {
            let target = format!(
                "{}{}",
                c.value
                    .as_deref()
                    .or(c.version.as_deref())
                    .unwrap_or("unchanged"),
                c.digest
                    .as_ref()
                    .map(|d| format!("@{d}"))
                    .unwrap_or_default()
            );
            let row = format!(
                "| {} | {} → {} | {} | {} |\n",
                cell(&c.package),
                cell(&c.current),
                cell(&target),
                cell(&c.kind),
                cell(&c.file)
            );
            if text.len() + row.len() > 18_000 {
                text.push_str("\nAdditional updates omitted from this table to keep the PR body within its size budget.\n");
                break;
            }
            text.push_str(&row);
        }
        if plan.action.lockfile_pending {
            text.push_str("\nLockfile pending: regenerate and validate it before merging.\n");
        }
        if let Some(items) = &plan.batch {
            text.push_str(&crate::batch::render(items));
        }
        let body_notes: BTreeSet<_> = plan.changes.iter().flat_map(|c| &c.body_notes).collect();
        for note in body_notes {
            append(&mut text, &format!("\n{}\n", safe(note)), 22_000);
        }
        let ranges: Vec<_> = plan
            .changes
            .iter()
            .map(|c| {
                (
                    &c.source,
                    &c.package,
                    &c.current,
                    &c.current_version,
                    &c.version,
                    &c.value,
                    &c.kind,
                    c.release_notes,
                )
            })
            .collect();
        let inputs = (
            "notes-render-v5",
            &text,
            ranges,
            self.recheck,
            self.branch_permits.get(&plan.action.branch),
        );
        // Only an advertised server joins the hash, so bodies written without
        // one keep their hash.
        let content_hash = hash(
            &match &self.server {
                None => serde_json::to_vec(&inputs),
                Some(url) => serde_json::to_vec(&(inputs, url)),
            }
            .expect("serializable body intent"),
        );
        let content_marker = format!("<!-- patchula-content:{content_hash} -->");
        (text, content_marker)
    }
    fn reusable_body(&self, plan: &Plan, existing: Option<&Value>, content_marker: &str) -> bool {
        existing.is_some_and(|pr| {
            body(pr).contains(content_marker)
                && !checked(pr)
                && !plan.retry
                && marker(body(pr))
                    .is_some_and(|m| m.identity == plan.identity && m.update == update_key(plan))
        })
    }
    async fn render(
        &mut self,
        sources: &patchula_datasource::Client,
        plan: &Plan,
        existing: Option<&Value>,
    ) -> String {
        let (mut text, content_marker) = self.body_intent(plan);
        if let Some(pr) = existing.filter(|_| self.reusable_body(plan, existing, &content_marker)) {
            // Rebases, label reconciliation and receipt repair do not change
            // body inputs. Reuse their notes without metadata or network reads.
            return body(pr).into();
        }
        // The reviewed targets travel with the PR, so batching keeps them even
        // after a newer release changes this branch's plan. A group too large
        // to record within the body budget cannot be batched.
        let targets = crate::batch::hidden_marker(crate::batch::TARGETS, &plan.changes);
        let controls = if plan.batch.is_none() && !plan.maintenance && targets.len() <= 12_000 {
            format!(
                "- [ ] Add to the Count's batch PR. This one closes and the update moves there. {}\n{targets}\n",
                crate::batch::ADD,
            )
        } else {
            String::new()
        };
        let reserve = controls.len();
        let mut releases = BTreeSet::new();
        for change in &plan.changes {
            if releases.len() >= 8 || text.len() + reserve > BODY_LIMIT - 2500 {
                text.push_str("\nThe Count ran out of room for more release notes. The source repositories have the rest.\n");
                break;
            }
            if change.kind == "pin" || !change.release_notes {
                continue;
            }
            let Some(version) = change.version.as_ref().or(change.value.as_ref()) else {
                continue;
            };
            let current = change.current_version.as_ref().unwrap_or(&change.current);
            let key = (
                change.source.clone(),
                change.package.clone(),
                current.clone(),
                version.clone(),
            );
            if !releases.insert(key.clone()) {
                continue;
            }
            if !self.notes.contains_key(&key) {
                let note =
                    if let Ok(source) = serde_json::from_value(serde_json::json!(change.source)) {
                        sources
                            .release_notes(source, &change.package, current, version)
                            .await
                            .unwrap_or_default()
                    } else {
                        Default::default()
                    };
                self.notes.insert(key.clone(), note);
            }
            let budget = (BODY_LIMIT - 1500)
                .saturating_sub(text.len() + reserve + 2)
                .min(12_000);
            text.push('\n');
            text.push_str(&self.notes[&key].render(&change.package, budget));
            text.push('\n');
        }
        if let Some(permits) = self.branch_permits.get(&plan.action.branch) {
            let marker = crate::dashboard::permit_marker(permits);
            // Keep policy evidence with the PR even if its local cache is lost.
            text = patchula_datasource::notes::fit(
                &text,
                BODY_LIMIT.saturating_sub(marker.len() + reserve + 1000),
            )
            .0;
            text.push('\n');
            text.push_str(&marker);
        }
        if let Some(url) = &self.server {
            text.push('\n');
            text.push_str(&crate::advertise::marker(url));
        }
        text.push('\n');
        text.push_str(&content_marker);
        let timing = match self.recheck {
            Recheck::Now => "now",
            Recheck::NextRun => "on the next run",
        };
        text.push_str(&format!(
            "\n- [ ] Tick this and the Count will look at it again {timing}. {RECHECK}\n"
        ));
        text.push_str(&controls);
        text.push('\n');
        let m = Marker {
            version: 1,
            identity: plan.identity.clone(),
            update: update_key(plan),
        };
        text.push_str(MARKER);
        text.push_str(&serde_json::to_string(&m).expect("marker"));
        text.push_str(" -->\n");
        debug_assert!(text.len() + START.len() < BODY_LIMIT);
        format!("{START}\n{text}")
    }
}
/// One caution per tooling warning on this PR, ahead of everything else.
fn tooling_section(callouts: &[crate::tooling::Callout]) -> String {
    use crate::tooling::*;
    let mut text = String::new();
    for callout in callouts {
        let branch = safe(&callout.branch);
        let body = match (callout.rule, callout.detail) {
            (MERGE_WITHOUT_CI, Some(NATIVE)) => "**This PR is set to merge itself, and the Count can't find any CI.** No workflow or CI file runs on pull requests, and no checks have reported on recent commits, so nothing will have tested it when GitHub merges it.".to_owned(),
            (MERGE_WITHOUT_CI, _) => "**This PR is set to merge itself, and the Count can't find any CI.** No workflow or CI file runs on pull requests, and no checks have reported on recent commits. He waits for at least one passing check before merging, so this PR will stay open.".to_owned(),
            (MERGE_WITHOUT_REQUIRED_CHECKS, Some(REVIEWS_ONLY)) => format!("**Nothing makes this PR pass its checks before GitHub merges it.** The Count uses GitHub's auto-merge here, and `{branch}` requires a review but no status checks, so once approved a red PR merges as readily as a green one. Add a required check in a ruleset."),
            (MERGE_WITHOUT_REQUIRED_CHECKS, _) => format!("**Nothing makes this PR pass its checks before GitHub merges it.** The Count uses GitHub's auto-merge here, and `{branch}` requires no status checks, so a red PR merges as readily as a green one. Add a required check in a ruleset."),
            (SECURITY_MERGE_WITHOUT_DELIVERY, Some(TAGGED_RELEASE)) => format!("**Merging this fix doesn't ship it.** It will merge on its own, and nothing runs when `{branch}` changes. The only workflow here that could deliver it runs on tags or releases, so it ships only if you cut one."),
            (_, _) => format!("**Merging this fix ships nothing.** This PR fixes a known vulnerability and will merge on its own, but nothing in this repository builds, releases or deploys when `{branch}` changes. Whatever runs the old code stays bitten. If something outside this repository delivers it, say so in `warnings.quiet`."),
        };
        text.push_str("> [!CAUTION]\n> ");
        text.push_str(&body);
        text.push_str("\n\n");
    }
    text
}
/// Advisories the changes leave behind, after any tooling warnings.
fn security_section(changes: &[crate::Change]) -> String {
    let mut rows = BTreeSet::new();
    for change in changes.iter().filter(|c| c.image.is_none()) {
        for advisory in &change.advisories {
            let cves: Vec<_> = advisory
                .aliases
                .iter()
                .filter(|a| a.starts_with("CVE-"))
                .map(|a| safe(a))
                .collect();
            rows.insert(format!(
                "| [{}]({}){} | {} | {} | {} |\n",
                cell(&advisory.id),
                safe(&advisory.url),
                if cves.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", cves.join(", "))
                },
                cell(advisory.severity.as_deref().unwrap_or("unrated")),
                cell(&change.package),
                prose_cell(advisory.summary.as_deref().unwrap_or("")),
            ));
        }
    }
    if rows.is_empty() {
        return String::new();
    }
    let mut text = format!(
        "> [!IMPORTANT]\n> This update fixes {} known vulnerabilit{}. The Count does not care for anyone else drawing blood.\n\n| Advisory | Severity | Package | Summary |\n| --- | --- | --- | --- |\n",
        rows.len(),
        if rows.len() == 1 { "y" } else { "ies" }
    );
    for row in rows {
        append(&mut text, &row, 6_000);
    }
    text.push('\n');
    text
}
/// What the Count found comparing each rebuilt image's old and new SBOMs:
/// the advisories fixed, then one line per image he can't vouch for, then
/// plain notes.
fn image_section(changes: &[crate::Change]) -> String {
    let mut rows = Vec::new();
    let mut listed = BTreeSet::new();
    let mut fixed_ids = BTreeSet::new();
    let mut brings_in = Vec::new();
    let mut brings_in_listed = BTreeSet::new();
    let mut warnings = BTreeSet::new();
    let mut notes = BTreeSet::new();
    for change in changes {
        let Some(check) = &change.image else {
            continue;
        };
        let image = code(&change.package);
        let order = patchula_lookup::images::most_severe(check.fixed.iter().map(|f| f.id.as_str()));
        for id in order {
            fixed_ids.insert(id.to_owned());
            let Some(advisory) = change.advisories.iter().find(|a| a.id == id) else {
                listed.insert(id.to_owned());
                continue;
            };
            for fixed in check.fixed.iter().filter(|f| f.id == id) {
                let row = format!(
                    "| [{}]({}) | {} | {} | {} | {} | {} |\n",
                    cell(&fixed.id),
                    safe(&advisory.url),
                    cell(advisory.severity.as_deref().unwrap_or("unrated")),
                    cell(&fixed.package),
                    cell(&fixed.old),
                    cell(fixed.new.as_deref().unwrap_or("removed")),
                    cell(&fixed.platforms.join(", ")),
                );
                if !rows.contains(&row) {
                    rows.push(row);
                }
            }
        }
        for id in &check.introduced {
            match check.introduced_details.iter().find(|a| &a.id == id) {
                Some(advisory) => {
                    let shown = format!(
                        "[{}]({}) ({})",
                        cell(id),
                        safe(&advisory.url),
                        cell(advisory.severity.as_deref().unwrap_or("unrated"))
                    );
                    if !brings_in.contains(&shown) {
                        brings_in.push(shown);
                    }
                }
                None => {
                    brings_in_listed.insert(id.clone());
                }
            }
        }
        let previous = change
            .current
            .split_once('@')
            .map(|(_, digest)| digest.chars().take(14).collect::<String>())
            .unwrap_or_default();
        let warning = match check.state.as_str() {
            "no-sbom" => format!(
                "{image} publishes no SBOM for this image, and the Count doesn't open up image layers to look for himself. Treat it as an ordinary rebuild, or ask the publisher to attach an SBOM."
            ),
            "previous-unavailable" => format!(
                "The previous {image} image (`{}…`) is no longer available from its registry, so he has nothing to compare the new one against. Some registries, quay.io among them, delete an image once its tag moves on.",
                cell(&previous)
            ),
            "sbom-without-os" => format!(
                "The SBOM for {image} lists no OS packages, and it was made by {}, which doesn't look for them. The image may have none, or the SBOM may leave them out.",
                code(check.detail.as_deref().unwrap_or("an unknown tool"))
            ),
            "os-not-covered" => format!(
                "{image} is built on {}, and OSV.dev has no advisories for it.",
                cell(check.os.as_deref().unwrap_or("an OS"))
            ),
            _ => String::new(),
        };
        if !warning.is_empty() {
            if check.silenced.is_none() {
                warnings.insert(warning);
            }
            continue;
        }
        let note = match check.state.as_str() {
            "no-os-packages" => format!(
                "According to its SBOM, {image} has no OS packages, so there is nothing at the OS level for a rebuild to fix."
            ),
            "unreadable" => format!(
                "The Count couldn't read {image}'s SBOM on this run ({}). He'll try again on the next run.",
                cell(check.detail.as_deref().unwrap_or("no answer"))
            ),
            _ if !check.fixed.is_empty() => String::new(),
            _ => format!(
                "The Count compared {image}'s old and new package lists. OSV.dev knows of no vulnerability that the new one fixes."
            ),
        };
        if !note.is_empty() {
            notes.insert(note);
        }
        if check.state == "partial-sources" {
            notes.insert(format!(
                "{image}'s SBOM doesn't say which source package its {} OS packages come from, so the Count checked them under their own names. Advisories filed under a different source name may be missing.",
                check.packages
            ));
        }
    }
    let mut text = String::new();
    if !rows.is_empty() {
        let verified = fixed_ids.len() - listed.len();
        text.push_str(&if listed.is_empty() {
            format!(
                "> [!IMPORTANT]\n> This rebuild fixes {verified} known vulnerabilit{} in the image's OS packages, according to OSV.dev. The Count does not care for anyone else drawing blood.\n\n",
                if verified == 1 { "y" } else { "ies" }
            )
        } else {
            format!(
                "> [!IMPORTANT]\n> This rebuild fixes known vulnerabilities in the image's OS packages. The Count read OSV.dev's record for the {verified} below, and OSV.dev lists {} more that the new image no longer has. The Count does not care for anyone else drawing blood.\n\n",
                listed.len()
            )
        });
        text.push_str("| Advisory | Severity | Package | Old | New | Platforms |\n| --- | --- | --- | --- | --- | --- |\n");
        for row in &rows {
            append(&mut text, row, 8_000);
        }
        if !listed.is_empty() {
            append(
                &mut text,
                &format!(
                    "\nAnd {} more listed by OSV: {}.\n",
                    listed.len(),
                    id_links(&listed, 3_000)
                ),
                11_000,
            );
        }
        text.push('\n');
    } else if !listed.is_empty() {
        append(
            &mut text,
            &format!(
                "OSV.dev lists {} advisor{} that the new image no longer has, but the Count read no record that confirms one, so he doesn't count this rebuild as a security fix: {}.\n\n",
                listed.len(),
                if listed.len() == 1 { "y" } else { "ies" },
                id_links(&listed, 3_000)
            ),
            11_000,
        );
    }
    let also = if text.is_empty() { "" } else { " also" };
    let mut introduced = brings_in;
    if !brings_in_listed.is_empty() {
        let links = id_links(&brings_in_listed, 1_500);
        introduced.push(if introduced.is_empty() {
            format!(
                "{} advisor{} listed by OSV: {links}",
                brings_in_listed.len(),
                if brings_in_listed.len() == 1 {
                    "y"
                } else {
                    "ies"
                }
            )
        } else {
            format!("and {} more listed by OSV: {links}", brings_in_listed.len())
        });
    }
    if !introduced.is_empty() {
        append(
            &mut text,
            &format!(
                "The new image{also} brings in {}.\n\n",
                introduced.join("; ")
            ),
            13_000,
        );
    }
    if !warnings.is_empty() {
        text.push_str(
            "> [!WARNING]\n> **The Count can't tell whether this rebuild fixes any vulnerabilities.**\n",
        );
        for warning in warnings {
            append(&mut text, &format!(">\n> {warning}\n"), 11_000);
        }
        text.push('\n');
    }
    for note in notes {
        append(&mut text, &format!("{note}\n\n"), 12_000);
    }
    text
}
/// Advisory IDs as osv.dev links, as many as fit in `budget` bytes, with a
/// count of the rest.
fn id_links(ids: &BTreeSet<String>, budget: usize) -> String {
    let ordered = patchula_lookup::images::most_severe(ids.iter().map(String::as_str));
    let mut links = Vec::new();
    let mut length = 0;
    for id in &ordered {
        let link = format!("[{}](https://osv.dev/vulnerability/{})", cell(id), safe(id));
        if length + link.len() > budget {
            break;
        }
        length += link.len() + 2;
        links.push(link);
    }
    let rest = ordered.len() - links.len();
    let mut text = links.join(", ");
    if rest > 0 {
        text.push_str(&format!(" and {rest} not listed here"));
    }
    text
}
fn truncate(text: &str, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].into()
}
fn safe(text: &str) -> String {
    text.replace("<!--", "&lt;!--").replace("-->", "--&gt;")
}
/// Text as an inline code span that no backtick inside it can end early.
fn code(text: &str) -> String {
    let text = truncate(&safe(text).replace(['\n', '\r'], " "), 240);
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{pad}{text}{pad}{fence}")
}
/// A table cell of generated text: package names, versions, files. Those are
/// data and stay as they are.
fn cell(text: &str) -> String {
    truncate(
        &safe(text).replace('|', "\\|").replace(['\n', '\r'], " "),
        240,
    )
}
/// A table cell of third-party prose, such as an advisory summary.
fn prose_cell(text: &str) -> String {
    cell(&crate::text::inert(text))
}
fn append(text: &mut String, addition: &str, limit: usize) {
    let remaining = limit.saturating_sub(text.len());
    if remaining > 0 {
        text.push_str(&truncate(addition, remaining));
    }
}

#[cfg(test)]
mod security_tests {
    use super::security_section;

    #[test]
    fn fixed_advisories_lead_the_body_once_each() {
        let advisory: patchula_report::Advisory = serde_json::from_value(serde_json::json!({
            "id":"GHSA-jf85-cpcp-j695","aliases":["CVE-2019-10744","SNYK-JS-LODASH-450202"],
            "severity":"critical","summary":"Prototype | pollution",
            "url":"https://osv.dev/vulnerability/GHSA-jf85-cpcp-j695"
        }))
        .unwrap();
        let change = crate::Change {
            source: "npm".into(),
            package: "lodash".into(),
            file: "package.json".into(),
            current: "4.17.11".into(),
            current_version: None,
            value: Some("4.17.21".into()),
            version: Some("4.17.21".into()),
            digest: None,
            kind: "patch".into(),
            merge: false,
            labels: Vec::new(),
            release_notes: true,
            body_notes: Vec::new(),
            advisories: vec![advisory],
            image: None,
        };
        let mut other = change.clone();
        other.file = "web/package.json".into();
        let text = security_section(&[change.clone(), other]);
        assert!(text.starts_with("> [!IMPORTANT]\n> This update fixes 1 known vulnerability."));
        assert!(text.contains("| [GHSA-jf85-cpcp-j695](https://osv.dev/vulnerability/GHSA-jf85-cpcp-j695) (CVE-2019-10744) | critical | lodash | Prototype \\| pollution |\n"), "{text}");
        let mut plain = change;
        plain.advisories.clear();
        assert!(security_section(&[plain]).is_empty());
    }

    #[test]
    fn advisory_text_and_package_names_never_mention_anyone() {
        let advisory: patchula_report::Advisory = serde_json::from_value(serde_json::json!({
            "id":"GHSA-x","severity":"high",
            "summary":"Reported by @mallory, see #12 and https://github.com/a/b/issues/3",
            "url":"https://osv.dev/vulnerability/GHSA-x"
        }))
        .unwrap();
        let change = crate::Change {
            source: "npm".into(),
            package: "@scope/pkg".into(),
            file: "package.json".into(),
            current: "1.0.0".into(),
            current_version: None,
            value: Some("1.0.1".into()),
            version: Some("1.0.1".into()),
            digest: None,
            kind: "patch".into(),
            merge: false,
            labels: Vec::new(),
            release_notes: true,
            body_notes: vec!["Held for @mallory".into()],
            advisories: vec![advisory],
            image: None,
        };
        let text = security_section(&[change]);
        for forged in ["@mallory", " #12", "//github.com/a/b/issues/3"] {
            assert!(!text.contains(forged), "{forged}: {text}");
        }
        assert!(text.contains("@\u{200d}mallory"), "{text}");
        // Package names are data, not prose.
        assert!(text.contains("| @scope/pkg |"), "{text}");
        assert!(super::prose_cell("@mallory").contains("@\u{200d}mallory"));
        assert!(
            crate::batch::render(&[crate::batch::Item {
                origin: "a".repeat(64),
                number: Some(1),
                title: "update @mallory/pkg".into(),
                targets: vec![],
            }])
            .contains("update @\u{200d}mallory/pkg")
        );
    }
}

#[cfg(test)]
mod image_tests {
    use super::image_section;
    use patchula_report::{ImageAdvisory, ImageCheck};

    fn change(package: &str, check: ImageCheck) -> crate::Change {
        crate::Change {
            source: "docker".into(),
            package: package.into(),
            file: "Dockerfile".into(),
            current: format!("1.0@sha256:{}", "4c49fc0".repeat(9)),
            current_version: None,
            value: Some("1.0".into()),
            version: Some("1.0".into()),
            digest: Some(format!("sha256:{}", "b".repeat(64))),
            kind: "digest".into(),
            merge: false,
            labels: Vec::new(),
            release_notes: true,
            body_notes: Vec::new(),
            advisories: Vec::new(),
            image: Some(check),
        }
    }
    fn state(state: &str) -> ImageCheck {
        ImageCheck {
            state: state.into(),
            ..ImageCheck::default()
        }
    }

    #[test]
    fn a_fixing_rebuild_lists_its_advisories_with_old_and_new_versions() {
        let mut check = state("checked");
        check.fixed = vec![
            ImageAdvisory {
                id: "DSA-1".into(),
                package: "openssl".into(),
                old: "3.0.1".into(),
                new: Some("3.0.2".into()),
                platforms: vec!["linux/amd64".into(), "linux/arm64".into()],
            },
            ImageAdvisory {
                id: "DSA-2".into(),
                package: "perl".into(),
                old: "5.36".into(),
                new: None,
                platforms: vec!["linux/amd64".into()],
            },
        ];
        check.introduced = vec!["DSA-3".into()];
        let mut fixing = change("debian", check);
        fixing.advisories = vec![
            serde_json::from_value(serde_json::json!({
                "id": "DSA-1", "severity": "high", "url": "https://osv.dev/vulnerability/DSA-1"
            }))
            .unwrap(),
        ];
        let text = image_section(&[fixing]);
        assert!(text.starts_with("> [!IMPORTANT]\n> This rebuild fixes known vulnerabilities in the image's OS packages. The Count read OSV.dev's record for the 1 below, and OSV.dev lists 1 more that the new image no longer has."), "{text}");
        assert!(text.contains("| [DSA-1](https://osv.dev/vulnerability/DSA-1) | high | openssl | 3.0.1 | 3.0.2 | linux/amd64, linux/arm64 |\n"), "{text}");
        assert!(
            text.contains(
                "\nAnd 1 more listed by OSV: [DSA-2](https://osv.dev/vulnerability/DSA-2).\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("The new image also brings in 1 advisory listed by OSV: [DSA-3](https://osv.dev/vulnerability/DSA-3).\n"),
            "{text}"
        );
        assert!(
            !text.contains("OSV.dev knows of no vulnerability"),
            "{text}"
        );
    }

    #[test]
    fn only_advisories_with_a_record_get_a_row_and_the_rest_are_counted_ids() {
        let advisory = |id: &str, severity: &str| -> patchula_report::Advisory {
            serde_json::from_value(serde_json::json!({
                "id": id, "severity": severity, "url": format!("https://osv.dev/vulnerability/{id}")
            }))
            .unwrap()
        };
        let mut check = state("checked");
        check.fixed = (1..=8)
            .map(|n| ImageAdvisory {
                id: format!("DEBIAN-CVE-2025-{n}"),
                package: "openssl".into(),
                old: "3.0.1".into(),
                new: Some("3.0.2".into()),
                platforms: vec!["linux/amd64".into()],
            })
            .collect();
        check.introduced = vec!["USN-9-1".into(), "UBUNTU-CVE-2025-7".into()];
        check.introduced_details = vec![advisory("USN-9-1", "moderate")];
        let mut fixing = change("debian", check);
        fixing.advisories = (4..=8)
            .map(|n| advisory(&format!("DEBIAN-CVE-2025-{n}"), "high"))
            .collect();
        let text = image_section(&[fixing]);
        assert!(
            text.contains(
                "The Count read OSV.dev's record for the 5 below, and OSV.dev lists 3 more"
            ),
            "{text}"
        );
        assert_eq!(text.matches("| high | openssl |").count(), 5, "{text}");
        assert!(
            text.find("[DEBIAN-CVE-2025-8]").unwrap() < text.find("[DEBIAN-CVE-2025-4]").unwrap(),
            "{text}"
        );
        assert!(text.contains("\nAnd 3 more listed by OSV: [DEBIAN-CVE-2025-3](https://osv.dev/vulnerability/DEBIAN-CVE-2025-3), [DEBIAN-CVE-2025-2](https://osv.dev/vulnerability/DEBIAN-CVE-2025-2), [DEBIAN-CVE-2025-1](https://osv.dev/vulnerability/DEBIAN-CVE-2025-1).\n"), "{text}");
        assert!(text.contains("The new image also brings in [USN-9-1](https://osv.dev/vulnerability/USN-9-1) (moderate); and 1 more listed by OSV: [UBUNTU-CVE-2025-7](https://osv.dev/vulnerability/UBUNTU-CVE-2025-7).\n"), "{text}");
    }

    #[test]
    fn a_rebuild_that_only_brings_advisories_in_still_says_so() {
        let mut check = state("checked");
        check.introduced = vec!["USN-9-1".into()];
        let text = image_section(&[change("debian", check)]);
        assert!(!text.contains("[!IMPORTANT]"), "{text}");
        assert!(
            text.contains("The new image brings in 1 advisory listed by OSV: [USN-9-1](https://osv.dev/vulnerability/USN-9-1).\n"),
            "{text}"
        );
    }

    #[test]
    fn a_generator_name_cannot_close_its_code_span() {
        let mut ko = state("sbom-without-os");
        ko.detail = Some("ko`\n> [!NOTE] `x".into());
        let text = image_section(&[change("ghcr.io/acme/api", ko)]);
        assert!(
            text.contains("made by ``ko` > [!NOTE] `x``, which doesn't look for them"),
            "{text}"
        );
    }

    #[test]
    fn a_rebuild_with_no_verified_record_claims_no_fix() {
        let mut check = state("checked");
        check.fixed = vec![ImageAdvisory {
            id: "DSA-7".into(),
            package: "openssl".into(),
            old: "3.0.1".into(),
            new: Some("3.0.2".into()),
            platforms: vec!["linux/amd64".into()],
        }];
        let text = image_section(&[change("debian", check)]);
        assert!(!text.contains("[!IMPORTANT]"), "{text}");
        assert!(text.contains("OSV.dev lists 1 advisory that the new image no longer has, but the Count read no record that confirms one, so he doesn't count this rebuild as a security fix: [DSA-7](https://osv.dev/vulnerability/DSA-7)."), "{text}");
    }

    #[test]
    fn each_image_gets_its_own_line_and_silenced_warnings_are_left_off() {
        let mut ko = state("sbom-without-os");
        ko.detail = Some("ko".into());
        let mut silenced = state("no-sbom");
        silenced.silenced = Some("scanned in CI".into());
        let mut partial = state("partial-sources");
        partial.packages = 131;
        let mut unreadable = state("unreadable");
        unreadable.detail = Some("registry rate limit reached; retry after 60 seconds".into());
        let mut oracle = state("os-not-covered");
        oracle.os = Some("ol 9".into());
        let text = image_section(&[
            change("quay.io/argoproj/argocd", state("no-sbom")),
            change("ghcr.io/acme/api", ko),
            change("ghcr.io/acme/internal", silenced),
            change("debian", partial),
            change("mysql", oracle),
            change("ghcr.io/astral-sh/uv", state("no-os-packages")),
            change("quay.io/old", state("previous-unavailable")),
            change("alpine", unreadable),
        ]);
        assert_eq!(text.matches("[!WARNING]").count(), 1, "{text}");
        assert!(
            text.contains("> `quay.io/argoproj/argocd` publishes no SBOM for this image"),
            "{text}"
        );
        assert!(
            text.contains("made by `ko`, which doesn't look for them"),
            "{text}"
        );
        assert!(
            text.contains("> `mysql` is built on ol 9, and OSV.dev has no advisories for it."),
            "{text}"
        );
        assert!(
            text.contains(
                "The previous `quay.io/old` image (`sha256:4c49fc0…`) is no longer available"
            ),
            "{text}"
        );
        assert!(!text.contains("internal"), "{text}");
        assert!(
            text.contains("The Count compared `debian`'s old and new package lists."),
            "{text}"
        );
        assert!(text.contains("its 131 OS packages come from"), "{text}");
        assert!(
            text.contains("According to its SBOM, `ghcr.io/astral-sh/uv` has no OS packages"),
            "{text}"
        );
        assert!(text.contains("The Count couldn't read `alpine`'s SBOM on this run (registry rate limit reached; retry after 60 seconds)."), "{text}");
    }
}

#[cfg(test)]
mod tooling_tests {
    use super::tooling_section;
    use crate::tooling::*;

    fn callout(rule: &'static str, detail: Option<&'static str>) -> Callout {
        Callout {
            rule,
            detail,
            branch: "main".into(),
        }
    }

    #[test]
    fn each_tooling_warning_is_a_caution_in_the_design_wording() {
        let text = tooling_section(&[
            callout(MERGE_WITHOUT_CI, None),
            callout(MERGE_WITHOUT_CI, Some(NATIVE)),
            callout(MERGE_WITHOUT_REQUIRED_CHECKS, None),
            callout(SECURITY_MERGE_WITHOUT_DELIVERY, None),
        ]);
        assert_eq!(text.matches("> [!CAUTION]\n").count(), 4, "{text}");
        for line in [
            "> **This PR is set to merge itself, and the Count can't find any CI.** No workflow or CI file runs on pull requests, and no checks have reported on recent commits, so nothing will have tested it when GitHub merges it.\n",
            "> **This PR is set to merge itself, and the Count can't find any CI.** No workflow or CI file runs on pull requests, and no checks have reported on recent commits. He waits for at least one passing check before merging, so this PR will stay open.\n",
            "> **Nothing makes this PR pass its checks before GitHub merges it.** The Count uses GitHub's auto-merge here, and `main` requires no status checks, so a red PR merges as readily as a green one. Add a required check in a ruleset.\n",
            "> **Merging this fix ships nothing.** This PR fixes a known vulnerability and will merge on its own, but nothing in this repository builds, releases or deploys when `main` changes. Whatever runs the old code stays bitten. If something outside this repository delivers it, say so in `warnings.quiet`.\n",
        ] {
            assert!(text.contains(line), "{line}\n{text}");
        }
        let narrower = tooling_section(&[
            callout(MERGE_WITHOUT_REQUIRED_CHECKS, Some(REVIEWS_ONLY)),
            callout(SECURITY_MERGE_WITHOUT_DELIVERY, Some(TAGGED_RELEASE)),
        ]);
        assert!(
            narrower.contains("`main` requires a review but no status checks"),
            "{narrower}"
        );
        assert!(narrower.contains("> **Merging this fix doesn't ship it.** It will merge on its own, and nothing runs when `main` changes. The only workflow here that could deliver it runs on tags or releases, so it ships only if you cut one.\n"), "{narrower}");
        assert!(tooling_section(&[]).is_empty());
    }
}

#[cfg(test)]
mod foreign_text_tests {
    use super::{FOREIGN_LIMIT, MARKER, START, body_hash, regions, with_foreign};

    /// What the Count writes: his section, opened by the start marker and
    /// closed by the ownership marker.
    fn section(table: &str) -> String {
        format!(
            "{START}\n{table}{MARKER}{{\"version\":1,\"identity\":\"{}\",\"update\":\"{}\"}} -->\n",
            "a".repeat(64),
            "b".repeat(64)
        )
    }
    /// A block some other tool or person added to the description.
    const BLOCK: &str = "<!-- tool:summary:start -->\nA generated summary, with a [link](https://example.org/x) and @someone.\n<!-- tool:summary:end -->";

    #[test]
    fn the_section_is_what_the_counts_markers_enclose() {
        let body = format!("Intro by a person.\n\n{}\n\n{BLOCK}", section("table\n"));
        let (before, middle, after) = regions(&body).unwrap();
        assert_eq!(before, "Intro by a person.\n\n");
        assert_eq!(middle, section("table\n").trim_end());
        assert_eq!(after, format!("\n\n\n{BLOCK}"));
        // An older body has no start marker: all of it up to his marker is his.
        let legacy = format!(
            "table\n{MARKER}{{\"version\":1,\"identity\":\"{}\",\"update\":\"{}\"}} -->\nrest",
            "a".repeat(64),
            "b".repeat(64)
        );
        assert_eq!(regions(&legacy).unwrap().0, "");
        assert!(regions("no markers at all").is_none());
    }

    #[test]
    fn text_before_and_after_the_section_survives_rewrites_exactly_once() {
        for current in [
            format!("{}{BLOCK}", section("old\n")),
            format!("{BLOCK}\n\n{}", section("old\n")),
            format!("{BLOCK}\n\n{}\n\n{BLOCK} again", section("old\n")),
        ] {
            let rewritten = with_foreign(section("new\n"), &current);
            assert!(
                rewritten.contains("new\n") && !rewritten.contains("old\n"),
                "{rewritten}"
            );
            assert_eq!(rewritten.matches(START).count(), 1);
            assert_eq!(rewritten.matches(MARKER).count(), 1);
            assert_eq!(
                rewritten.matches("tool:summary:start").count(),
                current.matches("tool:summary:start").count()
            );
            // Rendering again over the result is a fixed point.
            assert_eq!(with_foreign(section("new\n"), &rewritten), rewritten);
            // So is taking the existing body as the text, as an unchanged publish does.
            assert_eq!(with_foreign(current.clone(), &current), current);
            assert_eq!(
                body_hash(&with_foreign(current.clone(), &current)),
                body_hash(&current)
            );
        }
    }

    #[test]
    fn what_others_wrote_comes_from_the_latest_read_not_an_older_one() {
        let stale = format!("{}{BLOCK}", section("t\n"));
        // The block was rewritten in place, and another was prepended, since.
        let fresh = format!(
            "Added later.\n\n{}<!-- tool:summary:start -->\nrewritten\n<!-- tool:summary:end -->",
            section("t\n")
        );
        let merged = with_foreign(stale, &fresh);
        assert_eq!(merged, fresh);
    }

    #[test]
    fn an_edit_inside_the_section_loses_to_the_counts_rewrite() {
        let edited = section("table\nsomeone typed here\n");
        let merged = with_foreign(section("table\n"), &format!("{edited}{BLOCK}"));
        assert!(!merged.contains("someone typed here"), "{merged}");
        assert!(merged.ends_with(BLOCK));
    }

    #[test]
    fn a_large_amount_of_foreign_text_fits_and_an_excessive_amount_is_dropped() {
        let big = format!("\n\n{}", "x".repeat(30_000));
        let kept = with_foreign(section("t\n"), &format!("{}{big}", section("o\n")));
        assert!(kept.ends_with(&big));
        let huge = format!("\n\n{}", "x".repeat(FOREIGN_LIMIT));
        let dropped = with_foreign(section("t\n"), &format!("{}{huge}", section("o\n")));
        assert_eq!(dropped, format!("{}\n", section("t\n").trim_end()));
    }
}
