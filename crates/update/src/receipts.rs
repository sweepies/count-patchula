//! Receipts are proof of previously verified writes, never proof from a marker alone.
use crate::{Lifecycle, Plan, Recheck, plan::hash};
use rustovate_cache::{Cache, ExtractionKey};
use rustovate_config::{Config, Rebase};
use rustovate_github::{Client, GraphqlPage};
use rustovate_report::{ActionStatus, Repository};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const INVENTORY_QUERY: &str = r#"query($owner:String!,$name:String!,$prefix:String!,$cursor:String){
 viewer{login}
 repository(owner:$owner,name:$name){
  defaultBranchRef{name target{oid}}
  refs(refPrefix:$prefix,first:100,after:$cursor){pageInfo{hasNextPage endCursor} nodes{
   name target{... on Commit{oid message tree{oid} author{user{login}} committer{user{login}} signature{isValid}}}
   associatedPullRequests(states:[OPEN],first:2){pageInfo{hasNextPage} nodes{
    number id url title body headRefName headRefOid baseRefName author{login}
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
fn key(repo: &str, prefix: &str) -> ExtractionKey {
    ExtractionKey {
        repo: repo.into(),
        default_branch_commit: prefix.into(),
        config_hash: "verified-write-receipts-v2".into(),
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
        "head":pr["head"],"base":pr["base"],"actor":pr["user"]["login"],
        "labels":labels,"merge":!pr["auto_merge"].is_null()})
}
fn rest_pr(pr: &Value) -> Option<Value> {
    if pr["labels"]["pageInfo"]["hasNextPage"] != false {
        return None;
    }
    Some(
        json!({"number":pr["number"],"node_id":pr["id"],"html_url":pr["url"],
        "title":pr["title"],"body":pr["body"],"state":"open",
        "head":{"ref":pr["headRefName"],"sha":pr["headRefOid"],"repo":{"full_name":pr["headRepository"]["nameWithOwner"]}},
        "base":{"ref":pr["baseRefName"],"repo":{"full_name":pr["baseRepository"]["nameWithOwner"]}},
        "user":pr["author"],"labels":pr["labels"]["nodes"],"auto_merge":pr["autoMergeRequest"]}),
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
    let Some(bytes) = cache
        .extraction_get(&key(&repository.repository, &config.prs.branch))
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(false);
    };
    let Ok(receipts) = serde_json::from_slice::<BTreeMap<String, Receipt>>(&bytes) else {
        return Ok(false);
    };
    if receipts.is_empty() {
        return Ok(false);
    }
    let (owner, name) = repository
        .repository
        .split_once('/')
        .ok_or("invalid repository")?;
    // Use refs/heads/ as the namespace, then filter locally. This also detects
    // legacy names and prefix changes without ignoring obsolete owned heads.
    let mut actor = String::new();
    let refs: Vec<Value> = client
        .graphql_pages(
            INVENTORY_QUERY,
            json!({"owner":owner,"name":name,"prefix":"refs/heads/"}),
            |data| {
                let head = &data["repository"]["defaultBranchRef"];
                if head["name"].as_str() != repository.default_branch.as_deref()
                    || head["target"]["oid"].as_str() != repository.commit.as_deref()
                {
                    return Err(rustovate_github::Error::Graphql);
                }
                let login = data["viewer"]["login"]
                    .as_str()
                    .ok_or(rustovate_github::Error::Json)?;
                if !actor.is_empty() && actor != login {
                    return Err(rustovate_github::Error::Graphql);
                }
                actor = login.into();
                GraphqlPage::from_connection(&data, "/repository/refs")
            },
            |_| false,
        )
        .await
        .map_err(|e| e.to_string())?;
    let desired: BTreeSet<_> = plans.iter().map(|p| p.action.branch.as_str()).collect();
    let obsolete = refs.iter().any(|r| {
        let name = r["name"].as_str().unwrap_or("");
        !desired.contains(name)
            && (name.starts_with(&config.prs.branch)
                || r["target"]["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("\nRustovate: v1")))
    });
    for plan in plans
        .iter_mut()
        .filter(|p| p.action.status == ActionStatus::Planned)
    {
        let Some(receipt) = receipts.get(&plan.action.branch) else {
            continue;
        };
        let Some(r) = refs.iter().find(|r| r["name"] == plan.action.branch) else {
            continue;
        };
        let prs = &r["associatedPullRequests"];
        let Some(nodes) = prs["nodes"].as_array() else {
            continue;
        };
        if nodes.len() != 1 || prs["pageInfo"]["hasNextPage"] != false {
            continue;
        }
        let Some(pr) = rest_pr(&nodes[0]) else {
            continue;
        };
        if receipt.actor != actor
            || receipt.intent != intent(plan, config, recheck)
            || r["target"]["oid"] != receipt.commit
            || r["target"]["tree"]["oid"] != receipt.tree
            || pr["head"]["sha"] != receipt.commit
            || pr_state(&pr) != receipt.pr
            || (receipt.base != plan.action.base_commit
                && (config.prs.rebase != Rebase::Conflicts || !plan.lock_jobs.is_empty()))
        {
            continue;
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
    }
    Ok(!obsolete
        && plans
            .iter()
            .all(|p| p.action.status != ActionStatus::Planned))
}

pub async fn save_receipts(
    cache: &Cache,
    repository: &Repository,
    config: &Config,
    plans: &[Plan],
    lifecycle: &Lifecycle,
) -> Result<(), String> {
    let mut receipts = BTreeMap::new();
    for plan in plans {
        if !matches!(
            plan.action.status,
            ActionStatus::Created
                | ActionStatus::Updated
                | ActionStatus::Rebased
                | ActionStatus::Unchanged
        ) || lifecycle
            .actions
            .iter()
            .any(|a| a.branch == plan.action.branch && a.status == ActionStatus::Error)
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
        receipts.insert(
            plan.action.branch.clone(),
            Receipt {
                actor: pr["user"]["login"]
                    .as_str()
                    .ok_or("missing PR author")?
                    .into(),
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
        .map_err(|e| e.to_string())
}
