//! Receipts are proof of previously verified writes, never proof from a marker alone.
use crate::{Lifecycle, Plan, Recheck, plan::hash};
use rustovate_cache::{Cache, ExtractionKey};
use rustovate_config::{Config, Rebase};
use rustovate_github::{Client, GraphqlPage};
use rustovate_report::{ActionStatus, Repository};
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
  defaultBranchRef{name target{oid}}
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
    files: Vec<rustovate_report::FileAction>,
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
fn key(repo: &str, prefix: &str) -> ExtractionKey {
    ExtractionKey {
        repo: repo.into(),
        default_branch_commit: prefix.into(),
        config_hash: "verified-write-receipts-v3".into(),
    }
}
fn intent(plan: &Plan, config: &Config, recheck: Recheck) -> String {
    hash(
        serde_json::to_string(&(
            &plan.identity,
            &plan.input_fingerprint,
            &plan.changes,
            config,
            recheck,
        ))
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
    json!({"number":pr["number"],"title":pr["title"],"body":pr["body"],
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
    tracing::debug!(target: "rustovate_update::receipts", repo = %repository.repository,
        branch = %plan.action.branch, reason, "write receipt verification fallback");
}
fn not_restored(repository: &Repository, plans: &[Plan], reason: &str) -> bool {
    for plan in plans
        .iter()
        .filter(|p| p.action.status == ActionStatus::Planned)
    {
        fallback(repository, plan, reason);
    }
    tracing::debug!(target: "rustovate_update::receipts", repo = %repository.repository,
        restored = 0, complete = false, reason, "write receipts restored");
    false
}

pub(crate) fn rest_pr(pr: &Value) -> Option<Value> {
    if pr["labels"]["pageInfo"]["hasNextPage"] != false {
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

/// One fresh paginated inventory replaces every per-branch verification when
/// the head, intent and complete PR state still match a successful receipt.
/// Returns true only when obsolete-head cleanup can also be skipped.
pub async fn restore_unchanged(
    client: &Client,
    cache: &Cache,
    repository: &Repository,
    config: &Config,
    plans: &mut [Plan],
    recheck: Recheck,
) -> Result<bool, String> {
    let inventory = Inventory::read(client, cache, repository, config).await?;
    restore_inventory(cache, repository, config, plans, recheck, &inventory).await
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
            .extraction_get(&key(&repository.repository, &config.prs.branch))
            .await
            .map_err(|e| e.to_string())?
            .and_then(|b| serde_json::from_slice::<Receipts>(&b).ok())
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
            let head = &data["repository"]["defaultBranchRef"];
            if head["name"].as_str() != repository.default_branch.as_deref()
                || head["target"]["oid"].as_str() != repository.commit.as_deref()
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
    inventory: &Inventory,
) -> Result<bool, String> {
    let bytes = cache
        .extraction_get(&key(&repository.repository, &config.prs.branch))
        .await
        .map_err(|e| e.to_string())?;
    let pending = plans
        .iter()
        .any(|p| p.action.status == ActionStatus::Planned);
    let receipts = match bytes {
        None if pending => return Ok(not_restored(repository, plans, "missing-receipt")),
        Some(bytes) => match serde_json::from_slice::<Receipts>(&bytes) {
            Ok(receipts) => receipts,
            Err(_) if pending => return Ok(not_restored(repository, plans, "invalid-receipts")),
            Err(_) => Receipts::default(),
        },
        None => Receipts::default(),
    };
    if (receipts.writes.is_empty() && receipts.closed.is_empty() || receipts.closed.len() > 100)
        && plans
            .iter()
            .any(|p| p.action.status == ActionStatus::Planned)
    {
        return Ok(not_restored(repository, plans, "unusable-receipts"));
    }
    let actor = inventory.actor.as_str();
    let refs = &inventory.refs;
    let closed = &inventory.closed;
    let desired: BTreeSet<_> = plans.iter().map(|p| p.action.branch.as_str()).collect();
    let obsolete = refs.iter().any(|r| {
        let name = r["name"].as_str().unwrap_or("");
        Some(name) != repository.default_branch.as_deref()
            && !desired.contains(name)
            && r["target"]["author"]["user"]["login"] == actor
            && (r["target"]["committer"]["user"]["login"] == actor
                || r["target"]["committer"]["user"]["login"] == "web-flow")
            && r["target"]["signature"]["isValid"] == true
            && (name.starts_with(&config.prs.branch)
                || r["target"]["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("\nRustovate: v1")))
    });
    let mut restored = 0;
    for plan in plans
        .iter_mut()
        .filter(|p| p.action.status == ActionStatus::Planned)
    {
        let result = if plan.retry {
            Err("checkbox-recheck")
        } else if let Some(receipt) = receipts.closed.get(&plan.action.branch) {
            if receipt.actor != actor {
                Err("actor-mismatch")
            } else if receipt.intent != intent(plan, config, recheck) {
                Err("intent-changed")
            } else if !closed.iter().any(|pr| {
                pr["id"] == receipt.pr["node_id"]
                    && pr["state"] == "CLOSED"
                    && pr["merged"] == false
                    && pr["body"] == receipt.pr["body"]
                    && actor_login(&pr["author"]).as_deref() == Some(actor)
                    && pr["headRepository"]["nameWithOwner"] == repository.repository
                    && pr["baseRepository"]["nameWithOwner"] == repository.repository
            }) {
                Err("pr-state-changed")
            } else {
                plan.action.status = ActionStatus::Skipped;
                plan.action.reason = Some(crate::pull_requests::CLOSED_BY_OWNER.into());
                Ok(())
            }
        } else {
            restore_write(&receipts, refs, actor, plan, config, recheck)
        };
        match result {
            Ok(()) => {
                restored += 1;
                tracing::debug!(target: "rustovate_update::receipts", repo = %repository.repository,
                    branch = %plan.action.branch, "write receipt restored");
            }
            Err(reason) => fallback(repository, plan, reason),
        }
    }
    let complete = !obsolete
        && plans
            .iter()
            .all(|p| p.action.status != ActionStatus::Planned);
    tracing::debug!(target: "rustovate_update::receipts", repo = %repository.repository,
        restored, complete, obsolete, "write receipts restored");
    Ok(complete)
}

fn restore_write(
    receipts: &Receipts,
    refs: &[Value],
    actor: &str,
    plan: &mut Plan,
    config: &Config,
    recheck: Recheck,
) -> Result<(), &'static str> {
    let receipt = receipts
        .writes
        .get(&plan.action.branch)
        .ok_or("missing-receipt")?;
    if receipt.actor != actor {
        return Err("actor-mismatch");
    }
    if receipt.intent != intent(plan, config, recheck) {
        return Err("intent-changed");
    }
    let r = refs
        .iter()
        .find(|r| r["name"] == plan.action.branch)
        .ok_or("missing-head")?;
    if r["target"]["oid"] != receipt.commit {
        return Err("head-oid-changed");
    }
    if r["target"]["tree"]["oid"] != receipt.tree {
        return Err("tree-oid-changed");
    }
    if r["target"]["signature"]["isValid"] != true {
        return Err("signature-invalid");
    }
    let prs = &r["associatedPullRequests"];
    let nodes = prs["nodes"].as_array().ok_or("pr-state-incomplete")?;
    if nodes.len() != 1 || prs["pageInfo"]["hasNextPage"] != false {
        return Err("pr-state-changed");
    }
    let pr = rest_pr(&nodes[0]).ok_or("pr-state-incomplete")?;
    if pr["user"]["login"] != actor {
        return Err("actor-mismatch");
    }
    if pr["head"]["sha"] != receipt.commit || pr_state(&pr) != receipt.pr {
        return Err("pr-state-changed");
    }
    if receipt.base != plan.action.base_commit
        && (config.prs.rebase != Rebase::Conflicts || !plan.lock_jobs.is_empty())
    {
        return Err("base-changed");
    }
    plan.action.status = ActionStatus::Unchanged;
    plan.action.previous_commit = Some(receipt.commit.clone());
    plan.action.commit = Some(receipt.commit.clone());
    plan.action.verified = Some(true);
    plan.tree = Some(receipt.tree.clone());
    plan.cached_pr = Some(pr);
    plan.fingerprint = receipt.fingerprint.clone();
    plan.action.files = receipt.files.clone();
    plan.lockfiles_ready = true;
    Ok(())
}

pub(crate) async fn save_merge_states(
    cache: &Cache,
    repository: &Repository,
    config: &Config,
    plans: &[Plan],
) -> Result<(), String> {
    let key = key(&repository.repository, &config.prs.branch);
    let Some(bytes) = cache
        .extraction_get(&key)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    let mut receipts: Receipts = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    for plan in plans {
        let Some(pr) = &plan.cached_pr else {
            continue;
        };
        if plan.action.status == ActionStatus::Merged {
            receipts.writes.remove(&plan.action.branch);
        } else if let Some(receipt) = receipts.writes.get_mut(&plan.action.branch) {
            receipt.pr = pr_state(pr);
        }
    }
    cache
        .extraction_put(
            &key,
            &serde_json::to_vec(&receipts).map_err(|e| e.to_string())?,
        )
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
        if plan.action.status == ActionStatus::Skipped
            && plan.action.reason.as_deref() == Some(crate::pull_requests::CLOSED_BY_OWNER)
            && let Some(pr) = lifecycle.closed_for(plan)
            && pr["node_id"].is_string()
        {
            let Some(actor) = pr["user"]["login"].as_str() else {
                fallback(repository, plan, "missing-pr-author");
                continue;
            };
            receipts.closed.insert(
                plan.action.branch.clone(),
                ClosedReceipt {
                    actor: actor.into(),
                    intent: intent(plan, config, lifecycle.recheck),
                    pr: json!({"node_id":pr["node_id"],"body":pr["body"]}),
                },
            );
        }
        if !matches!(
            plan.action.status,
            ActionStatus::Created
                | ActionStatus::Updated
                | ActionStatus::Rebased
                | ActionStatus::Unchanged
        ) || plan.action.verified != Some(true)
            || !lifecycle.actions.iter().any(|a| {
                a.branch == plan.action.branch
                    && matches!(
                        a.status,
                        ActionStatus::Created | ActionStatus::Updated | ActionStatus::Unchanged
                    )
            })
        {
            continue;
        }
        let (Some(commit), Some(tree)) = (&plan.action.commit, &plan.tree) else {
            continue;
        };
        let Some(pr) = plan.cached_pr.as_ref().or_else(|| {
            lifecycle
                .prs
                .iter()
                .find(|pr| pr["state"] == "open" && pr["head"]["ref"] == plan.action.branch)
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
            plan.action.branch.clone(),
            Receipt {
                actor: actor.into(),
                commit: commit.clone(),
                tree: tree.clone(),
                base: plan.action.base_commit.clone(),
                intent: intent(plan, config, lifecycle.recheck),
                pr: pr_state(pr),
                fingerprint: plan.fingerprint.clone(),
                files: plan.action.files.clone(),
            },
        );
    }
    cache
        .extraction_put(
            &key(&repository.repository, &config.prs.branch),
            &serde_json::to_vec(&receipts).map_err(|e| e.to_string())?,
        )
        .await
        .map_err(|e| e.to_string())?;
    tracing::debug!(target: "rustovate_update::receipts", repo = %repository.repository,
        count = receipts.writes.len() + receipts.closed.len(), writes = receipts.writes.len(),
        closed = receipts.closed.len(), "write receipts saved");
    Ok(())
}
