//! Receipts are proof of previously verified writes, never proof from a marker alone.
use crate::{Lifecycle, Plan};
use patchula_cache::{Cache, State};
use patchula_config::{Config, Rebase};
use patchula_github::{Client, GraphqlPage};
use patchula_manager::hash;
use patchula_report::{BranchStatus, Repository};
use patchula_words::Recheck;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const INVENTORY_QUERY: &str = r#"query($owner:String!,$name:String!,$prefix:String!,$cursor:String,$issueCursor:String,$closed:[ID!]!){
 viewer{login}
 nodes(ids:$closed){... on PullRequest{id number state merged body headRefName
  author{__typename login} headRepository{nameWithOwner} baseRepository{nameWithOwner}
 }}
 repository(owner:$owner,name:$name){
  id autoMergeAllowed squashMergeAllowed mergeCommitAllowed rebaseMergeAllowed deleteBranchOnMerge
  defaultBranchRef{name target{oid ... on Commit{history(first:10){nodes{statusCheckRollup{state}}}}}
   rules(first:100){pageInfo{hasNextPage} nodes{type parameters{
    ... on RequiredStatusChecksParameters{requiredStatusChecks{context}}
    ... on PullRequestParameters{requiredApprovingReviewCount}
   }}}
   refUpdateRule{requiredStatusCheckContexts requiredApprovingReviewCount}
  }
  issues(states:[OPEN],first:100,after:$issueCursor){pageInfo{hasNextPage endCursor} nodes{
   id number title body state author{__typename login}
  }}
  refs(refPrefix:$prefix,first:100,after:$cursor){pageInfo{hasNextPage endCursor} nodes{
   name target{... on Commit{oid message tree{oid} author{user{login}} committer{user{login}} signature{isValid}}}
   associatedPullRequests(states:[OPEN],first:2){pageInfo{hasNextPage} nodes{
    number id url title body headRefName headRefOid baseRefName author{__typename login}
    headRepository{nameWithOwner} baseRepository{nameWithOwner}
    labels(first:100){pageInfo{hasNextPage} nodes{name}} autoMergeRequest{enabledAt}
   }}
  }}
 }
}"#;

#[derive(Serialize, Deserialize)]
struct Receipt {
    actor: String,
    commit: String,
    tree: String,
    base: String,
    intent: String,
    pr: Value,
    fingerprint: String,
    files: Vec<patchula_report::FileAction>,
}
#[derive(Default, Serialize, Deserialize)]
struct Receipts {
    writes: BTreeMap<String, Receipt>,
    closed: BTreeMap<String, ClosedReceipt>,
}
#[derive(Serialize, Deserialize)]
struct ClosedReceipt {
    actor: String,
    intent: String,
    pr: Value,
}
#[derive(Serialize)]
struct ReceiptsScope {
    repository: String,
    branch_prefix: String,
}
impl State for Receipts {
    const NAMESPACE: &'static str = "verified-write-receipts";
    const VERSION: u32 = 1;
    type Scope = ReceiptsScope;
}
fn receipts_scope(repo: &str, prefix: &str) -> ReceiptsScope {
    ReceiptsScope {
        repository: repo.into(),
        branch_prefix: prefix.into(),
    }
}
/// `server` is the advertised URL, which the PR body carries. Only a server
/// joins the hash, so receipts written without one stay valid.
fn intent(plan: &Plan, config: &Config, recheck: Recheck, server: Option<&str>) -> String {
    let inputs = (
        "release-notes-v4",
        &plan.identity,
        &plan.input_fingerprint,
        &plan.changes,
        config,
        crate::pull_requests::recheck_hashed(recheck),
    );
    // Tooling warnings join only when present, so other receipts stay valid.
    hash(
        match (server, plan.tooling.is_empty()) {
            (None, true) => serde_json::to_string(&inputs),
            (Some(url), true) => serde_json::to_string(&(inputs, url)),
            (server, false) => serde_json::to_string(&(inputs, server, &plan.tooling)),
        }
        .expect("serializable intent")
        .as_bytes(),
    )
}
fn pr_state(pr: &Value) -> Value {
    let labels: BTreeSet<_> = pr["labels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| l["name"].as_str())
        .collect();
    json!({"number":pr["number"],"title":pr["title"],"body_hash":crate::pull_requests::body_hash(pr["body"].as_str().unwrap_or("")),
        "head":{"ref":pr["head"]["ref"],"sha":pr["head"]["sha"],"repo":pr["head"]["repo"]["full_name"]},
        "base":{"ref":pr["base"]["ref"],"repo":pr["base"]["repo"]["full_name"]},"actor":pr["user"]["login"],
        "labels":labels,"merge":!pr["auto_merge"].is_null()})
}
// GraphQL Actor.login omits REST's suffix for Bots. Use the explicit type:
// stripping/appending suffixes for arbitrary Users would conflate identities.
pub(crate) fn actor_login(actor: &Value) -> Option<String> {
    let login = actor["login"].as_str()?;
    Some(
        if actor["__typename"] == "Bot" && !login.ends_with("[bot]") {
            format!("{login}[bot]")
        } else {
            login.into()
        },
    )
}
fn fallback(repository: &Repository, plan: &Plan, reason: &str) {
    tracing::debug!(target: "patchula_update::receipts", repo = %repository.repository,
        branch = %plan.action().branch, reason, "write receipt verification fallback");
}
/// What settling from verified receipts concluded for a repository's plans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreOutcome {
    /// Every plan settled, no owned branch is obsolete and no ref is
    /// unsettled (see `Inventory::unsettled`): nothing needs writing, and the
    /// shared tail reconciles from this inventory alone.
    Complete,
    /// Some plan or branch still needs the full write path.
    Partial,
}

fn not_restored(repository: &Repository, plans: &[Plan], reason: &str) -> RestoreOutcome {
    for plan in plans
        .iter()
        .filter(|p| p.action().status == BranchStatus::Planned)
    {
        fallback(repository, plan, reason);
    }
    tracing::debug!(target: "patchula_update::receipts", repo = %repository.repository,
        restored = 0, complete = false, reason, "write receipts restored");
    RestoreOutcome::Partial
}

pub(crate) fn rest_pr(pr: &Value) -> Option<Value> {
    if pr["labels"]["pageInfo"]["hasNextPage"].as_bool() != Some(false) {
        return None;
    }
    Some(
        json!({"number":pr["number"],"node_id":pr["id"],"html_url":pr["url"],
        "title":pr["title"],"body":pr["body"],"state":"open",
        "head":{"ref":pr["headRefName"],"sha":pr["headRefOid"],"repo":{"full_name":pr["headRepository"]["nameWithOwner"]}},
        "base":{"ref":pr["baseRefName"],"repo":{"full_name":pr["baseRepository"]["nameWithOwner"]}},
        "user":{"login":actor_login(&pr["author"])},"labels":pr["labels"]["nodes"],"auto_merge":pr["autoMergeRequest"]}),
    )
}

/// One recent commit on the default branch.
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultCommit {
    /// Whether it reported a `statusCheckRollup`.
    pub rollup: bool,
}
/// The default branch's head as `INVENTORY_QUERY` returns it: the fields
/// the freshness check and tooling evidence read, decoded beside the query
/// so its shape lives in one place.
#[derive(Debug, Default)]
pub struct DefaultBranch {
    /// `defaultBranchRef.name`.
    pub name: Option<String>,
    /// `defaultBranchRef.target.oid`.
    pub oid: Option<String>,
    /// `target.history.nodes`, `None` when the query did not return a list,
    /// so absent and empty stay distinct as the raw read had them.
    pub history: Option<Vec<DefaultCommit>>,
    /// `rules`, as returned: tooling decides what its nodes prove.
    pub rules: Value,
    /// `refUpdateRule`, as returned.
    pub ref_update_rule: Value,
}
impl DefaultBranch {
    /// Decode the query's `defaultBranchRef` node.
    fn read(head: &Value) -> Self {
        Self {
            name: head["name"].as_str().map(str::to_owned),
            oid: head["target"]["oid"].as_str().map(str::to_owned),
            history: head["target"]["history"]["nodes"].as_array().map(|nodes| {
                nodes
                    .iter()
                    .map(|node| DefaultCommit {
                        rollup: !node["statusCheckRollup"].is_null(),
                    })
                    .collect()
            }),
            rules: head["rules"].clone(),
            ref_update_rule: head["refUpdateRule"].clone(),
        }
    }
}
/// Fresh control and branch state shared by lookup, dashboard and write receipts.
#[derive(Default, Debug)]
pub struct Inventory {
    pub actor: String,
    pub refs: Vec<Value>,
    pub closed: Vec<Value>,
    pub issues: Vec<Value>,
    pub merge_settings: crate::automerge::Settings,
    pub merged_prs: Vec<Value>,
    /// The default branch's recent check rollups and protection, for tooling
    /// warnings.
    pub default_branch: DefaultBranch,
    /// Branch names whose labels or associated pull requests did not fit in
    /// the query's first page. Their PR view is incomplete, so the receipts
    /// fast path must not settle from this inventory.
    pub unsettled: BTreeSet<String>,
}
impl Inventory {
    pub fn pull_requests(&self, repo: &str) -> Vec<Value> {
        self.refs
            .iter()
            .flat_map(|r| {
                r["associatedPullRequests"]["nodes"]
                    .as_array()
                    .into_iter()
                    .flatten()
            })
            .filter_map(rest_pr)
            .filter(|p| crate::pull_requests::owned_pr(p, &self.actor, repo))
            .collect()
    }
    pub async fn read(
        client: &Client,
        cache: &Cache,
        repository: &Repository,
        config: &Config,
    ) -> Result<Self, String> {
        let receipts = cache
            .state::<Receipts>(&receipts_scope(&repository.repository, &config.prs.branch))
            .await
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        let closed_ids: Vec<_> = receipts
            .closed
            .values()
            .map(|r| &r.pr["node_id"])
            .take(100)
            .collect();
        let (owner, name) = repository
            .repository
            .split_once('/')
            .ok_or("invalid repository")?;
        let mut result = Self::default();
        let mut cursor = Value::Null;
        let mut issue_cursor = Value::Null;
        let mut refs_done = false;
        let mut issues_done = false;
        for _ in 0..1000 {
            let data = client.graphql(INVENTORY_QUERY, json!({"owner":owner,"name":name,
                "prefix":"refs/heads/","closed":closed_ids,"cursor":cursor,"issueCursor":issue_cursor}))
                .await.map_err(|e| e.to_string())?;
            let branch = DefaultBranch::read(&data["repository"]["defaultBranchRef"]);
            if branch.name.as_deref() != repository.default_branch.as_deref()
                || branch.oid.as_deref() != repository.commit.as_deref()
            {
                return Err("default branch changed since extraction; rerun".into());
            }
            let actor = data["viewer"]["login"]
                .as_str()
                .ok_or("missing viewer login")?;
            if !result.actor.is_empty() && result.actor != actor {
                return Err("inventory actor changed".into());
            }
            result.actor = actor.into();
            result.merge_settings = crate::automerge::Settings::from_graphql(&data["repository"]);
            result.default_branch = branch;
            result.closed = data["nodes"]
                .as_array()
                .cloned()
                .ok_or("missing closed inventory")?;
            for (path, done, next, output) in [
                (
                    "/repository/refs",
                    &mut refs_done,
                    &mut cursor,
                    &mut result.refs,
                ),
                (
                    "/repository/issues",
                    &mut issues_done,
                    &mut issue_cursor,
                    &mut result.issues,
                ),
            ] {
                if *done {
                    continue;
                }
                let page = GraphqlPage::from_connection(&data, path).map_err(|e| e.to_string())?;
                output.extend(page.nodes);
                if page.has_next_page {
                    let end = page.end_cursor.ok_or("missing inventory cursor")?;
                    if *next == end {
                        return Err("repeated inventory cursor".into());
                    }
                    *next = json!(end);
                } else {
                    *done = true;
                }
            }
            if refs_done && issues_done {
                result.merged_prs =
                    crate::automerge::load_history(cache, &repository.repository, &result.actor)
                        .await?;
                // Labels and associated pull requests come back only to their
                // first page; anything past it is invisible here, so the
                // branch cannot settle from receipts.
                for r in &result.refs {
                    let prs = &r["associatedPullRequests"];
                    let mut partial = prs["pageInfo"]["hasNextPage"].as_bool() != Some(false);
                    for pr in prs["nodes"].as_array().into_iter().flatten() {
                        partial |= pr["labels"]["pageInfo"]["hasNextPage"].as_bool() != Some(false);
                    }
                    if partial && let Some(name) = r["name"].as_str() {
                        result.unsettled.insert(name.into());
                    }
                }
                return Ok(result);
            }
            if result.refs.len() + result.issues.len() > 100_000 {
                return Err("inventory page budget exceeded".into());
            }
        }
        Err("inventory page budget exceeded".into())
    }
}

pub async fn restore_inventory(
    cache: &Cache,
    repository: &Repository,
    config: &Config,
    plans: &mut [Plan],
    recheck: Recheck,
    server: Option<&str>,
    inventory: &Inventory,
) -> Result<RestoreOutcome, String> {
    let receipts = cache
        .state::<Receipts>(&receipts_scope(&repository.repository, &config.prs.branch))
        .await
        .map_err(|e| e.to_string())?;
    let pending = plans
        .iter()
        .any(|p| p.action().status == BranchStatus::Planned);
    let receipts = match receipts {
        None if pending => return Ok(not_restored(repository, plans, "missing-receipt")),
        Some(receipts) => receipts,
        None => Receipts::default(),
    };
    if (receipts.writes.is_empty() && receipts.closed.is_empty() || receipts.closed.len() > 100)
        && plans
            .iter()
            .any(|p| p.action().status == BranchStatus::Planned)
    {
        return Ok(not_restored(repository, plans, "unusable-receipts"));
    }
    let actor = inventory.actor.as_str();
    let refs = &inventory.refs;
    let closed = &inventory.closed;
    let desired: BTreeSet<_> = plans.iter().map(|p| p.action().branch.as_str()).collect();
    let obsolete = refs.iter().any(|r| {
        let name = r["name"].as_str().unwrap_or("");
        Some(name) != repository.default_branch.as_deref()
            && !desired.contains(name)
            && r["target"]["author"]["user"]["login"] == actor
            && (r["target"]["committer"]["user"]["login"] == actor
                || r["target"]["committer"]["user"]["login"] == "web-flow")
            && r["target"]["signature"]["isValid"].as_bool() == Some(true)
            && (name.starts_with(&config.prs.branch)
                || r["target"]["message"]
                    .as_str()
                    .is_some_and(crate::trailers::version))
    });
    let mut restored = 0;
    for plan in plans
        .iter_mut()
        .filter(|p| p.action().status == BranchStatus::Planned)
    {
        let result = if plan.retry {
            Err("checkbox-recheck")
        } else if let Some(receipt) = receipts.closed.get(&plan.action().branch) {
            if receipt.actor != actor {
                Err("actor-mismatch")
            } else if receipt.intent != intent(plan, config, recheck, server) {
                Err("intent-changed")
            } else if !closed.iter().any(|pr| {
                pr["id"] == receipt.pr["node_id"]
                    && pr["state"] == "CLOSED"
                    && pr["merged"].as_bool() == Some(false)
                    && crate::pull_requests::body_hash(pr["body"].as_str().unwrap_or(""))
                        == crate::pull_requests::body_hash(
                            receipt.pr["body"].as_str().unwrap_or(""),
                        )
                    && actor_login(&pr["author"]).as_deref() == Some(actor)
                    && pr["headRepository"]["nameWithOwner"]
                        .as_str()
                        .is_some_and(|r| {
                            crate::pull_requests::same_repository(r, &repository.repository)
                        })
                    && pr["baseRepository"]["nameWithOwner"]
                        .as_str()
                        .is_some_and(|r| {
                            crate::pull_requests::same_repository(r, &repository.repository)
                        })
            }) {
                Err("pr-state-changed")
            } else {
                plan.hold(crate::plan::Hold::ClosedByOwner);
                Ok(())
            }
        } else {
            restore_write(&receipts, refs, actor, plan, config, recheck, server)
        };
        match result {
            Ok(()) => {
                restored += 1;
                tracing::debug!(target: "patchula_update::receipts", repo = %repository.repository,
                    branch = %plan.action().branch, "write receipt restored");
            }
            Err(reason) => fallback(repository, plan, reason),
        }
    }
    let complete = !obsolete
        && inventory.unsettled.is_empty()
        && plans
            .iter()
            .all(|p| p.action().status != BranchStatus::Planned);
    tracing::debug!(target: "patchula_update::receipts", repo = %repository.repository,
        restored, complete, obsolete, unsettled = inventory.unsettled.len(), "write receipts restored");
    Ok(if complete {
        RestoreOutcome::Complete
    } else {
        RestoreOutcome::Partial
    })
}

fn restore_write(
    receipts: &Receipts,
    refs: &[Value],
    actor: &str,
    plan: &mut Plan,
    config: &Config,
    recheck: Recheck,
    server: Option<&str>,
) -> Result<(), &'static str> {
    let receipt = receipts
        .writes
        .get(&plan.action().branch)
        .ok_or("missing-receipt")?;
    if receipt.actor != actor {
        return Err("actor-mismatch");
    }
    if receipt.intent != intent(plan, config, recheck, server) {
        return Err("intent-changed");
    }
    let r = refs
        .iter()
        .find(|r| r["name"] == plan.action().branch)
        .ok_or("missing-head")?;
    if r["target"]["oid"] != receipt.commit {
        return Err("head-oid-changed");
    }
    if r["target"]["tree"]["oid"] != receipt.tree {
        return Err("tree-oid-changed");
    }
    if r["target"]["signature"]["isValid"].as_bool() != Some(true) {
        return Err("signature-invalid");
    }
    let prs = &r["associatedPullRequests"];
    let nodes = prs["nodes"].as_array().ok_or("pr-state-incomplete")?;
    if nodes.len() != 1 || prs["pageInfo"]["hasNextPage"].as_bool() != Some(false) {
        return Err("pr-state-changed");
    }
    let pr = rest_pr(&nodes[0]).ok_or("pr-state-incomplete")?;
    if pr["user"]["login"] != actor {
        return Err("actor-mismatch");
    }
    if pr["head"]["sha"] != receipt.commit || pr_state(&pr) != receipt.pr {
        return Err("pr-state-changed");
    }
    if receipt.base != plan.action().base_commit
        && (config.prs.rebase != Rebase::Conflicts || !plan.lock_jobs.is_empty())
    {
        return Err("base-changed");
    }
    plan.restored(
        &receipt.commit,
        &receipt.tree,
        &receipt.fingerprint,
        receipt.files.clone(),
        pr,
    );
    Ok(())
}

pub(crate) async fn save_merge_states(
    cache: &Cache,
    repository: &Repository,
    config: &Config,
    plans: &[Plan],
) -> Result<(), String> {
    let scope = receipts_scope(&repository.repository, &config.prs.branch);
    let Some(mut receipts) = cache
        .state::<Receipts>(&scope)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    for plan in plans {
        let Some(pr) = &plan.cached_pr else {
            continue;
        };
        if plan.action().status == BranchStatus::Merged {
            receipts.writes.remove(&plan.action().branch);
        } else if let Some(receipt) = receipts.writes.get_mut(&plan.action().branch) {
            receipt.pr = pr_state(pr);
        }
    }
    cache
        .set_state(&scope, &receipts)
        .await
        .map_err(|e| e.to_string())
}

pub async fn save_receipts(
    cache: &Cache,
    repository: &Repository,
    config: &Config,
    plans: &[Plan],
    lifecycle: &Lifecycle,
) -> Result<(), String> {
    let mut receipts = Receipts::default();
    for plan in plans {
        if plan.held(&crate::plan::Hold::ClosedByOwner)
            && let Some(pr) = lifecycle.closed_for(plan)
            && pr["node_id"].is_string()
        {
            let Some(actor) = pr["user"]["login"].as_str() else {
                fallback(repository, plan, "missing-pr-author");
                continue;
            };
            receipts.closed.insert(
                plan.action().branch.clone(),
                ClosedReceipt {
                    actor: actor.into(),
                    intent: intent(plan, config, lifecycle.recheck, lifecycle.server.as_deref()),
                    pr: json!({"node_id":pr["node_id"],"body":pr["body"]}),
                },
            );
        }
        if !matches!(
            plan.action().status,
            BranchStatus::Created
                | BranchStatus::Updated
                | BranchStatus::Rebased
                | BranchStatus::Unchanged
        ) || plan.action().verified != Some(true)
            || !lifecycle.actions.iter().any(|a| {
                a.branch == plan.action().branch
                    && matches!(
                        a.status,
                        patchula_report::PullRequestStatus::Created
                            | patchula_report::PullRequestStatus::Updated
                            | patchula_report::PullRequestStatus::Unchanged
                    )
            })
        {
            continue;
        }
        let (Some(commit), Some(tree)) = (&plan.action().commit, &plan.tree) else {
            continue;
        };
        let Some(pr) = plan.cached_pr.as_ref().or_else(|| {
            lifecycle
                .prs
                .iter()
                .find(|pr| pr["state"] == "open" && pr["head"]["ref"] == plan.action().branch)
        }) else {
            continue;
        };
        let Some(actor) = pr["user"]["login"].as_str() else {
            fallback(repository, plan, "missing-pr-author");
            continue;
        };
        if pr["head"]["sha"] != *commit {
            fallback(repository, plan, "pr-head-oid-changed");
            continue;
        }
        receipts.writes.insert(
            plan.action().branch.clone(),
            Receipt {
                actor: actor.into(),
                commit: commit.clone(),
                tree: tree.clone(),
                base: plan.action().base_commit.clone(),
                intent: intent(plan, config, lifecycle.recheck, lifecycle.server.as_deref()),
                pr: pr_state(pr),
                fingerprint: plan.fingerprint.clone(),
                files: plan.action().files.clone(),
            },
        );
    }
    cache
        .set_state(
            &receipts_scope(&repository.repository, &config.prs.branch),
            &receipts,
        )
        .await
        .map_err(|e| e.to_string())?;
    tracing::debug!(target: "patchula_update::receipts", repo = %repository.repository,
        count = receipts.writes.len() + receipts.closed.len(), writes = receipts.writes.len(),
        closed = receipts.closed.len(), "write receipts saved");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bot_actors_are_marked_and_humans_pass_through() {
        assert_eq!(
            actor_login(&serde_json::json!({"login": "hubot", "__typename": "Bot"})).as_deref(),
            Some("hubot[bot]")
        );
        assert_eq!(
            actor_login(&serde_json::json!({"login": "dependabot[bot]", "__typename": "Bot"}))
                .as_deref(),
            Some("dependabot[bot]")
        );
        assert_eq!(
            actor_login(&serde_json::json!({"login": "octocat"})).as_deref(),
            Some("octocat")
        );
        assert!(actor_login(&serde_json::json!({})).is_none());
        assert!(actor_login(&serde_json::json!({"login": 7})).is_none());
    }
}
