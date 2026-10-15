use crate::{Plan, plan::hash};
use chrono::{DateTime, Duration, Utc};
use rustovate_config::Config;
use rustovate_github::Client;
use rustovate_report::{ActionStatus, PullRequestAction};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const BODY_LIMIT: usize = 32_000;
const MARKER: &str = "<!-- rustovate:";
const RECHECK: &str = "<!-- rustovate-recheck -->";
const OBSOLETE: &str = "<!-- rustovate-closed:obsolete -->";
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
    notes: BTreeMap<(String, String, String, String), rustovate_datasource::notes::Notes>,
    pub(crate) recheck: Recheck,
    merge_settings: Option<crate::automerge::Settings>,
    maintenance_policy: rustovate_config::Policy,
    maintenance_group: String,
    branch_permits: BTreeMap<String, BTreeSet<String>>,
}
impl Lifecycle {
    pub fn set_merge_settings(&mut self, settings: crate::automerge::Settings) {
        self.merge_settings = Some(settings);
    }
    pub fn record_merges(&mut self, plans: &[Plan], actions: Vec<PullRequestAction>) {
        for plan in plans {
            if let Some(pr) = &plan.cached_pr
                && let Some(old) = self.prs.iter_mut().find(|p| p["number"] == pr["number"])
            {
                *old = pr.clone();
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
                && !body(p).contains(OBSOLETE)
                && marker(body(p)).is_some_and(|m| m.update == key)
        })
    }
    /// Authoritative inventory includes closed history and all pages. No cached
    /// decisions can suppress a newly closed PR or permit an over-budget create.
    pub async fn prepare(
        client: &Client,
        repository: &rustovate_report::Repository,
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
        repository: &rustovate_report::Repository,
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
        };
        let desired: BTreeSet<_> = plans.iter().map(|p| p.action.branch.as_str()).collect();
        for pr in &mut state.prs {
            if !repository.warnings.is_empty()
                || pr["state"] != "open"
                || !owned_pr(pr, actor, repo)
                || desired.contains(branch(pr))
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
            let mut text = truncate(body(&fresh), BODY_LIMIT - 100);
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
                        && !body(p).contains(OBSOLETE)
                        && marker(body(p)).is_some_and(|m| m.update == key)
                })
            {
                limit(plan, CLOSED_BY_OWNER);
                continue;
            }
            if let Some(pr) = existing.first() {
                plan.retry |= checked(pr);
                continue;
            }
            let bypass = controls.create.contains(&key) || controls.recreate.contains(&key);
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
            rustovate_datasource::Client::new(client.clone(), client.public_http(), client.cache());
        self.publish_with_sources(client, &sources, plans).await;
    }
    pub async fn publish_with_sources(
        &mut self,
        client: &Client,
        sources: &rustovate_datasource::Client,
        plans: &mut [Plan],
    ) {
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
        sources: &rustovate_datasource::Client,
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
            if fresh["title"] == title && fresh["body"] == text && fresh["base"]["ref"] == self.base
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
                let result = client
                    .graphql(
                        ENABLE_AUTO_MERGE,
                        json!({"input":{"pullRequestId":id,"mergeMethod":method.to_uppercase(),"expectedHeadOid":plan.action.commit}}),
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                if !result["enablePullRequestAutoMerge"]["pullRequest"].is_object() {
                    return Err("GitHub did not acknowledge native auto-merge".into());
                }
            }
        } else if !merge && enabled {
            let id = pr["node_id"].as_str().ok_or("missing PR node ID")?;
            client
                .graphql(DISABLE_AUTO_MERGE, json!({"input":{"pullRequestId":id}}))
                .await
                .map_err(|e| e.to_string())?;
        }
        pr["auto_merge"] =
            if merge && (enabled || self.merge_settings.as_ref().is_some_and(|s| s.native)) {
                json!({"enabled":true})
            } else {
                Value::Null
            };
        if merge && self.merge_settings.as_ref().is_some_and(|s| !s.native) {
            pr["rustovate_merge"] = json!(
                "auto-merge waiting for a later run with a verified receipt and passing checks"
            );
        } else if merge && !pr["auto_merge"].is_null() {
            pr["rustovate_merge"] = json!("native auto-merge enabled");
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

    async fn render(
        &mut self,
        sources: &rustovate_datasource::Client,
        plan: &Plan,
        existing: Option<&Value>,
    ) -> String {
        let mut text =
            String::from("| Package | Change | Type | File |\n| --- | --- | --- | --- |\n");
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
            text.push_str(
                "\nLockfile pending: regenerate and validate before merging (SWE-109).\n",
            );
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
        let content_hash = hash(
            &serde_json::to_vec(&(
                "notes-render-v2",
                &text,
                ranges,
                self.recheck,
                self.branch_permits.get(&plan.action.branch),
            ))
            .expect("serializable body intent"),
        );
        let content_marker = format!("<!-- rustovate-content:{content_hash} -->");
        if let Some(pr) = existing
            && body(pr).contains(&content_marker)
            && !checked(pr)
            && marker(body(pr))
                .is_some_and(|m| m.identity == plan.identity && m.update == update_key(plan))
        {
            // Rebases, label reconciliation and receipt repair do not change
            // body inputs. Reuse their notes without metadata or network reads.
            return body(pr).into();
        }
        let mut releases = BTreeSet::new();
        for change in &plan.changes {
            if releases.len() >= 8 || text.len() > BODY_LIMIT - 2500 {
                text.push_str("\nAdditional release notes truncated to fit this PR. Consult the source repositories for the remaining updates.\n");
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
                .saturating_sub(text.len() + 2)
                .min(12_000);
            text.push('\n');
            text.push_str(&self.notes[&key].render(&change.package, budget));
            text.push('\n');
        }
        if let Some(permits) = self.branch_permits.get(&plan.action.branch) {
            let marker = crate::dashboard::permit_marker(permits);
            // Keep policy evidence with the PR even if its local cache is lost.
            text = truncate(&text, BODY_LIMIT.saturating_sub(marker.len() + 1000));
            text.push('\n');
            text.push_str(&marker);
        }
        text.push('\n');
        text.push_str(&content_marker);
        let timing = match self.recheck {
            Recheck::Now => "now",
            Recheck::NextRun => "on the next run",
        };
        text.push_str(&format!(
            "\n- [ ] Created by rustovate. Click to re-check {timing}. {RECHECK}\n\n"
        ));
        let m = Marker {
            version: 1,
            identity: plan.identity.clone(),
            update: update_key(plan),
        };
        text.push_str(MARKER);
        text.push_str(&serde_json::to_string(&m).expect("marker"));
        text.push_str(" -->\n");
        debug_assert!(text.len() <= BODY_LIMIT);
        text
    }
}
fn truncate(text: &str, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].into()
}
fn safe(text: &str) -> String {
    text.replace("<!--", "&lt;!--")
        .replace("-->", "--&gt;")
        .replace('@', "@\u{200d}")
}
fn cell(text: &str) -> String {
    truncate(
        &safe(text).replace('|', "\\|").replace(['\n', '\r'], " "),
        240,
    )
}
fn append(text: &mut String, addition: &str, limit: usize) {
    let remaining = limit.saturating_sub(text.len());
    if remaining > 0 {
        text.push_str(&truncate(addition, remaining));
    }
}
