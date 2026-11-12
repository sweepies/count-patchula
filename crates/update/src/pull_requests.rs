use crate::Plan;
use crate::error::Error;
use crate::markers;
use crate::pr::body::{
    Bodies, Closure, body_hash, closure, notes_key, same_owner, title, unticked, with_foreign,
};
use crate::pr::view;
use chrono::{DateTime, Duration, Utc};
use patchula_config::{Config, PackageId};
use patchula_github::{Client, model};
use patchula_report::{BranchStatus, PullRequestAction, PullRequestStatus};
use patchula_words::Recheck;
use serde::Deserialize;
use serde_json::{Value, json};
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
/// `ClosedPulls.nodes[]` element — the `PullRequest` fields the closed
/// read selects. Non-null unless the schema marks them nullable (`author`,
/// the repositories); `createdAt` is `DateTime!` and stays verbatim.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClosedPull {
    /// `PullRequest.id` — `ID!`; the `node_id` the closed receipt stores.
    id: String,
    /// `PullRequest.number` — `Int!`.
    number: u64,
    /// `PullRequest.url` — `URI!`.
    url: String,
    /// `PullRequest.title` — `String!`.
    title: String,
    /// `PullRequest.body` — `String!`.
    body: String,
    /// `PullRequest.state` — `PullRequestState!`; re-filtered on `CLOSED`.
    state: String,
    /// `PullRequest.merged` — `Boolean!`; re-filtered on `false`.
    merged: bool,
    /// `PullRequest.createdAt` — `DateTime!`.
    created_at: String,
    /// `PullRequest.headRefName` — `String!`; re-filtered on the branch.
    head_ref_name: String,
    /// `PullRequest.headRefOid` — `GitObjectID!`.
    head_ref_oid: String,
    /// `PullRequest.baseRefName` — `String!`.
    base_ref_name: String,
    /// `PullRequest.author` — `Actor`, nullable.
    author: Option<crate::receipts::Actor>,
    /// `PullRequest.headRepository` — `Repository`, nullable.
    head_repository: Option<crate::receipts::NamedRepository>,
    /// `PullRequest.baseRepository` — `Repository`, nullable.
    base_repository: Option<crate::receipts::NamedRepository>,
}
impl ClosedPull {
    /// The node projected onto `Tracked` the way `tracked_pr` projects the
    /// inventory's: fields this read does not select stay unset.
    fn tracked(self) -> Tracked {
        Tracked::from(model::PullRequest {
            node_id: self.id,
            number: self.number,
            html_url: self.url,
            state: self.state.to_ascii_lowercase(),
            title: self.title,
            body: Some(self.body),
            user: self.author.as_ref().map(|actor| model::Actor {
                login: crate::receipts::actor_login(actor),
            }),
            draft: None,
            labels: Vec::new(),
            merged_at: None,
            auto_merge: None,
            mergeable: None,
            mergeable_state: None,
            created_at: Some(self.created_at),
            updated_at: None,
            head: model::PullRequestHead {
                branch: self.head_ref_name,
                sha: self.head_ref_oid,
                repo: self.head_repository.map(|r| model::PullRequestRepository {
                    full_name: r.name_with_owner,
                }),
            },
            base: model::PullRequestBase {
                branch: self.base_ref_name,
                repo: self.base_repository.map(|r| model::PullRequestRepository {
                    full_name: r.name_with_owner,
                }),
            },
        })
    }
}
/// The node selection every `ClosedPulls` alias carries.
const CLOSED_NODE: &str = "id number url title body state merged createdAt \
    headRefName headRefOid baseRefName \
    author { __typename login } \
    headRepository { nameWithOwner } baseRepository { nameWithOwner }";
/// One alias' `pullRequests` connection decoded: the closed unmerged PRs
/// that pass the client-side re-filter plus the next page's cursor.
struct ClosedPage {
    pulls: Vec<Tracked>,
    next: Option<String>,
}
/// Decode `data.repository[alias]` — `None` when the alias is missing, the
/// connection is malformed or a node is null or unreadable: the caller's
/// `Err(Unread)`. Nodes decode element-wise; the `headRefName`/`CLOSED`/
/// `!merged` re-filter drops a node the server should never have sent —
/// the filter is trusted not to drop, never trusted to narrow.
fn closed_page(data: &Value, alias: &str, head: &str) -> Option<ClosedPage> {
    let connection = data.get("repository")?.get(alias)?;
    let nodes = connection.get("nodes")?.as_array()?;
    let mut pulls = Vec::with_capacity(nodes.len());
    for node in nodes {
        let pr = serde_json::from_value::<ClosedPull>(node.clone()).ok()?;
        if pr.state == "CLOSED" && !pr.merged && pr.head_ref_name == head {
            pulls.push(pr.tracked());
        }
    }
    let info = connection.get("pageInfo")?;
    let next = if info.get("hasNextPage")?.as_bool()? {
        Some(info.get("endCursor")?.as_str()?.to_owned())
    } else {
        None
    };
    Some(ClosedPage { pulls, next })
}
/// Page `head`'s closed connection on its own from `cursor` until it is
/// finished. `Ok(false)` marks the branch `Unread` — a missing alias, a
/// null node or the page cap reached; a request error fails the whole
/// repository as a listing failure does.
async fn page_closed_branch(
    client: &Client,
    owner: &str,
    name: &str,
    head: &str,
    mut cursor: String,
    pulls: &mut Vec<Tracked>,
) -> Result<bool, Error> {
    let query = format!(
        "query ClosedPulls($owner: String!, $name: String!, $b0: String!, $after: String) {{ \
            repository(owner: $owner, name: $name) {{ \
                b0: pullRequests(headRefName: $b0, states: [CLOSED], first: 100, after: $after, \
                                 orderBy: {{field: CREATED_AT, direction: DESC}}) {{ \
                    pageInfo {{ hasNextPage endCursor }} nodes {{ {CLOSED_NODE} }} }} }} }}"
    );
    for _ in 0..client.max_pages() {
        let data = client
            .graphql::<Value>(
                &query,
                json!({"owner":owner,"name":name,"b0":head,"after":cursor}),
            )
            .await?;
        let Some(page) = closed_page(&data, "b0", head) else {
            return Ok(false);
        };
        pulls.extend(page.pulls);
        match page.next {
            Some(next) => cursor = next,
            None => return Ok(true),
        }
    }
    Ok(false)
}
/// The closed unmerged PRs of each branch in `branches` — one `ClosedPulls`
/// document per up to 25 of them, each alias a `pullRequests` connection
/// filtered server-side on `headRefName`/`CLOSED` and paged on its own.
/// A branch ends `Err(Unread)` on a null or unreadable node, a missing
/// alias or an unfinished page — `admit` protects its plan — while a
/// request failure propagates as a listing failure does.
async fn closed_pulls(
    client: &Client,
    repo: &str,
    branches: &[String],
) -> Result<BTreeMap<String, Result<Vec<Tracked>, view::Unread>>, Error> {
    let (owner, name) = repo
        .split_once('/')
        .ok_or(Error::Refused("invalid repository".into()))?;
    let mut out = BTreeMap::new();
    for chunk in branches.chunks(25) {
        let parameters: String = (0..chunk.len())
            .map(|i| format!(", $b{i}: String!"))
            .collect();
        let fields: String = (0..chunk.len())
            .map(|i| {
                format!(
                    "b{i}: pullRequests(headRefName: $b{i}, states: [CLOSED], first: 100, \
                     orderBy: {{field: CREATED_AT, direction: DESC}}) {{ \
                     pageInfo {{ hasNextPage endCursor }} nodes {{ {CLOSED_NODE} }} }}"
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        let query = format!(
            "query ClosedPulls($owner: String!, $name: String!{parameters}) {{ \
                repository(owner: $owner, name: $name) {{ {fields} }} }}"
        );
        let mut variables = json!({"owner":owner, "name":name});
        for (i, head) in chunk.iter().enumerate() {
            variables[format!("b{i}")] = head.clone().into();
        }
        let data = client.graphql::<Value>(&query, variables).await?;
        for (i, head) in chunk.iter().enumerate() {
            let result = match closed_page(&data, &format!("b{i}"), head) {
                Some(page) => {
                    let mut pulls = page.pulls;
                    match page.next {
                        Some(cursor) => {
                            if page_closed_branch(client, owner, name, head, cursor, &mut pulls)
                                .await?
                            {
                                Ok(pulls)
                            } else {
                                Err(view::Unread)
                            }
                        }
                        None => Ok(pulls),
                    }
                }
                None => Err(view::Unread),
            };
            out.insert(head.clone(), result);
        }
    }
    Ok(out)
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
/// The pull request status an error leaves on an action: a race — the
/// repository changed under the run — goes stale so a rerun retries it, and
/// anything else is the request's error. Publishing and auto-merge report a
/// race the same way.
pub(crate) fn failure_status(error: &Error) -> PullRequestStatus {
    match error {
        Error::Raced(_) => PullRequestStatus::Stale,
        _ => PullRequestStatus::Error,
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
    /// The pull requests the reads supplied: the open ones, plus each
    /// planned branch's closed unmerged PRs the `ClosedPulls` read
    /// returned — enough for [`Self::closed_for`] to name a suppressor.
    pub(crate) prs: Vec<Tracked>,
    /// `branch → pull number` for each plan `admit` held `ClosedByOwner`:
    /// the suppressor the view's `closed_for` named, recorded at admission
    /// so the closed receipt's PR is that decision, not another search's.
    suppressors: BTreeMap<String, u64>,
    /// Every element of the reads decoded. `false` when one was
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
    /// The closed unmerged PR `admit` held `plan` on — the same `Tracked`
    /// the view's `closed_for` returned, so the closed receipt names the
    /// PR the decision was made on.
    pub(crate) fn closed_for(&self, plan: &Plan) -> Option<&Tracked> {
        let number = self.suppressors.get(plan.action().branch.as_str())?;
        self.prs.iter().find(|t| t.pr.number == *number)
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
    ) -> Result<Self, Error> {
        let repo = repository.repository.as_str();
        let base = repository
            .commit
            .as_deref()
            .ok_or(Error::Refused("missing repository commit".into()))?;
        let default_branch = repository
            .default_branch
            .as_deref()
            .ok_or(Error::Refused("missing default branch".into()))?;
        // The open listing decodes element-wise: a PR that fails stays out
        // of `items` and marks the set incomplete — see `complete`.
        let page = model::Page::<model::PullRequest>::decode(
            client
                .rest_pages(
                    &format!(
                        "repos/{repo}/pulls?state=open&sort=created&direction=desc&per_page=100"
                    ),
                    |_| false,
                )
                .await?,
        );
        let mut state = Self {
            repo: repo.into(),
            actor: actor.into(),
            base: default_branch.into(),
            prs: page.items.into_iter().map(Tracked::from).collect(),
            suppressors: BTreeMap::new(),
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
        // The hourly count only exists when `prs.perHour` is set: the same
        // listing URL as the old whole-history read, walked only until a
        // page ends with a PR created before the last hour — an element
        // without a readable `created_at` never stops the walk.
        let mut recent = None;
        if state.complete && config.prs.per_hour.is_some() {
            let page = model::Page::<model::PullRequest>::decode(
                client
                    .rest_pages(
                        &format!(
                            "repos/{repo}/pulls?state=all&sort=created&direction=desc&per_page=100"
                        ),
                        |page: &[Value]| {
                            page.last()
                                .and_then(|pr| pr["created_at"].as_str())
                                .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
                                .is_some_and(|at| at < now - Duration::hours(1))
                        },
                    )
                    .await?,
            );
            state.complete &= page.complete;
            let recent_prs: Vec<Tracked> = page.items.into_iter().map(Tracked::from).collect();
            recent = Some(view::recent(&recent_prs, owner, now));
        }
        // Which branches `admit` will ask closed PRs about, and which open
        // owned PRs are obsolete, are decisions and live in the view; a
        // degraded repository selects none, as it skipped them all before.
        let desired: BTreeSet<String> = plans.iter().map(|p| p.action().branch.clone()).collect();
        let (closed_branches, obsolete) = {
            let pulls = view::Pulls::from_open(state.prs.iter().collect(), recent, state.complete);
            let branches = if state.complete {
                view::closed_reads(&pulls, plans, controls, owner)
            } else {
                Vec::new()
            };
            let obsolete: BTreeSet<u64> = if repository.degraded() {
                BTreeSet::new()
            } else {
                view::obsolete(&pulls, &desired, controls, owner)
                    .into_iter()
                    .map(|tracked| tracked.pr.number)
                    .collect()
            };
            (branches, obsolete)
        };
        // The closed read supplies only the branches `closed_reads` named;
        // its PRs join `prs` so the receipt path's `closed_for` still finds
        // a held plan's suppressor.
        let closed = if closed_branches.is_empty() {
            BTreeMap::new()
        } else {
            closed_pulls(client, repo, &closed_branches).await?
        };
        for prs in closed.values().flatten() {
            state.prs.extend(prs.iter().cloned());
        }
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
                .await?;
            if !owned_pr(&fresh, actor, repo) || fresh.head.sha != sha {
                return Err(Error::Raced("obsolete PR changed during inspection; rerun"));
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
                        .await?,
                );
            }
            let meta = meta.as_ref().unwrap();
            if meta.default_branch != default_branch {
                return Err(Error::Raced(
                    "default branch changed before PR closure; rerun",
                ));
            }
            // The default branch's head is a lease on each closure: a base
            // that moved while earlier PRs closed must stop the next one.
            let head = client
                .get_fresh::<model::BranchHead>(&format!("repos/{repo}/branches/{default_branch}"))
                .await?;
            if head.commit.sha != base {
                return Err(Error::Raced("base moved before obsolete PR closure; rerun"));
            }
            let text = closure(body(&fresh), Closure::Obsolete);
            client
                .patch::<serde::de::IgnoredAny>(
                    &format!("repos/{repo}/pulls/{number}"),
                    json!({"state":"closed","body":text}),
                )
                .await?;
            pr.mark_closed(text);
            state.actions.push(action(
                repo,
                &name,
                Some(&pr.pr),
                PullRequestStatus::Closed,
                Some("superseded or already satisfied update".into()),
            ));
        }
        let mut pulls = view::Pulls::from_open(
            state.prs.iter().filter(|t| t.pr.state == "open").collect(),
            recent,
            state.complete,
        );
        for (head, result) in &closed {
            pulls.read_closed(
                head.clone(),
                result.as_ref().map_err(|_| view::Unread).map(|_| {
                    state
                        .prs
                        .iter()
                        .filter(|t| {
                            t.pr.state == "closed"
                                && t.pr.merged_at.is_none()
                                && branch(&t.pr) == head.as_str()
                        })
                        .collect()
                }),
            );
        }
        view::admit(&pulls, plans, controls, config, owner);
        // Record which PR `admit`'s `closed_for` held each `ClosedByOwner`
        // plan on: the receipt path's `closed_for` resolves by that number
        // so it names the same PR the decision read.
        for plan in plans.iter() {
            if plan.held(&crate::plan::Hold::ClosedByOwner)
                && let Ok(Some(suppressor)) = pulls.closed_for(plan, owner)
            {
                state
                    .suppressors
                    .insert(plan.action().branch.clone(), suppressor.pr.number);
            }
        }
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
                    failure_status(&error),
                    Some(error.to_string()),
                ));
            }
        }
    }
    /// Close each open update PR the batch now carries, once the batch PR is
    /// published. The batched marker keeps the closure from suppressing the
    /// update, so removing it from the batch brings back an individual PR.
    pub async fn close_batched(&mut self, client: &Client, plans: &[Plan]) -> Result<(), Error> {
        if !self.complete {
            // The target list is read off the whole listing; an unreadable
            // PR could be a batch member or the batch PR itself.
            return Err(Error::Refused(
                "repository pull request list is incomplete".into(),
            ));
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
            let fresh = client.get_fresh::<model::PullRequest>(&path).await?;
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
                .await?;
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
    pub async fn acknowledge_rechecks(&mut self, client: &Client) -> Result<(), Error> {
        for pr in &mut self.prs {
            if pr.pr.state != "open"
                || !owned_pr(&pr.pr, &self.actor, &self.repo)
                || !checked(&pr.pr)
            {
                continue;
            }
            let number = pr.pr.number;
            let path = format!("repos/{}/pulls/{number}", self.repo);
            let fresh = client.get_fresh::<model::PullRequest>(&path).await?;
            if fresh.state != "open" || !owned_pr(&fresh, &self.actor, &self.repo) {
                continue;
            }
            if checked(&fresh) {
                *pr = Tracked::from(
                    client
                        .patch::<model::PullRequest>(&path, json!({"body":unticked(body(&fresh))}))
                        .await?,
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
    ) -> Result<(), Error> {
        if self.bodies().reserved(plan) > BODY_LIMIT {
            return Err(Error::Refused(
                "manual release-age evidence exceeds the PR body budget; split this update group"
                    .into(),
            ));
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
                    .await?;
                if !owned_pr(&fresh, &self.actor, &self.repo)
                    || branch(&fresh) != plan.action().branch
                    || !same_owner(body(&fresh), body(&pr.pr))
                {
                    return Err(Error::Refused(
                        "PR ownership marker or repository changed during this run; left untouched"
                            .into(),
                    ));
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
                    return Err(Error::Raced(
                        "PR head moved after branch publication to an unexpected commit; rerun",
                    ));
                }
                if attempt == 6 {
                    return Err(Error::Raced(
                        "PR head has not reflected our published commit after bounded retries; rerun",
                    ));
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
                    .await?;
                (Tracked::from(updated), PullRequestStatus::Updated)
            }
        } else {
            let created = client.post::<model::PullRequest>(&path,json!({"title":title,"body":text,"head":plan.action().branch,"base":self.base,"maintainer_can_modify":false})).await?;
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
                .await?;
            pr.set_labels(labels);
        }
        let merge = merge_requested(plan, &self.maintenance_policy);
        let enabled = pr.pr.auto_merge.is_some();
        let mut native = enabled;
        if merge && !enabled {
            if self.merge_settings.is_none() {
                let meta = client
                    .get_fresh::<model::Repository>(&format!("repos/{}", self.repo))
                    .await?;
                self.merge_settings = Some(crate::automerge::Settings::from_rest(&meta));
            }
            let settings = self.merge_settings.as_ref().expect("repository settings");
            if settings.native {
                let method = settings.method.as_ref().ok_or(Error::Refused(
                    "repository has no enabled merge method".into(),
                ))?;
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
                .await?;
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

#[cfg(test)]
mod closed_pulls_tests {
    use super::*;
    use patchula_github::Network;
    use std::path::Path;
    use std::sync::Arc;

    /// The recorded `sweepies/count-patchula` closed read: three real
    /// branches' `ClosedPulls` aliases replay through the production
    /// decoder and every returned pull projects onto `Tracked` — closed,
    /// unmerged, on the requested branch, its GraphQL `id` the `node_id`.
    /// Recording needs a `GITHUB_TOKEN`/`GH_TOKEN` and `PATCHULA_RECORD=1`.
    #[tokio::test]
    async fn closed_pulls_decode_the_recorded_repository() {
        let recording = patchula_testkit::recording();
        let token = if recording {
            std::env::var(patchula_testkit::GITHUB_TOKEN_VAR)
                .or_else(|_| std::env::var(patchula_testkit::GH_TOKEN_VAR))
                .expect("fixture recording requires an existing GITHUB_TOKEN or GH_TOKEN")
        } else {
            "offline-test-credential".into()
        };
        let cache = patchula_cache::Cache::open(":memory:", Default::default())
            .await
            .unwrap();
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/github/closed-pulls.json");
        let options = patchula_testkit::Options {
            redactor: patchula_testkit::Redactor::default().with_literals([token.clone()]),
            ..Default::default()
        };
        let transport =
            patchula_testkit::from_env(&path, options, || Ok(Arc::new(Network::new()?))).unwrap();
        let client = Client::with_transport(&token, cache, Default::default(), transport).unwrap();
        let branches = [
            "patchula/uv-0.x".to_owned(),
            "patchula/node-24.x".to_owned(),
            "patchula/batch".to_owned(),
        ];
        let read = closed_pulls(&client, "sweepies/count-patchula", &branches)
            .await
            .unwrap();
        assert_eq!(read.len(), 3, "{read:?}");
        for branch in &branches {
            let pulls = read[branch]
                .as_ref()
                .unwrap_or_else(|_| panic!("{branch} unread"));
            for tracked in pulls {
                assert_eq!(tracked.pr.state, "closed");
                assert!(tracked.pr.merged_at.is_none());
                assert_eq!(tracked.pr.head.branch, *branch);
                assert!(!tracked.pr.node_id.is_empty());
            }
        }
        // The real repository keeps a closed unmerged pull on at least one
        // of these branches, so the projection was exercised.
        assert!(
            read.values()
                .any(|pulls| pulls.as_ref().is_ok_and(|p| !p.is_empty()))
        );
    }
}
