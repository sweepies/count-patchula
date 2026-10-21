use crate::{Plan, REF_UPDATE};
use patchula_github::Client;
use patchula_report::{ActionStatus, BranchAction};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub(crate) fn trailer<'a>(message: &'a str, key: &str) -> Option<&'a str> {
    let mut lines = message.lines().filter_map(|l| l.strip_prefix(key));
    let value = lines.next()?;
    lines.next().is_none().then_some(value)
}

/// Validate every commit since the recorded base, including author and signature.
/// A copied marker alone never authorizes deleting or replacing human work.
pub async fn owned(client: &Client, repo: &str, head: &str, actor: &str) -> Result<bool, String> {
    let tip = client
        .get_fresh(&format!("repos/{repo}/commits/{head}"))
        .await
        .map_err(|e| e.to_string())?;
    let message = tip["commit"]["message"].as_str().unwrap_or("");
    let Some(base) = trailer(message, "Patchula-Base: ") else {
        return Ok(false);
    };
    if !base.bytes().all(|b| b.is_ascii_hexdigit()) || base.len() != 40 {
        return Ok(false);
    }
    let history = client
        .get_fresh(&format!(
            "repos/{repo}/compare/{base}...{head}?per_page=100"
        ))
        .await
        .map_err(|e| e.to_string())?;
    let Some(commits) = history["commits"].as_array() else {
        return Ok(false);
    };
    Ok(!commits.is_empty()
        && history["total_commits"].as_u64() == Some(commits.len() as u64)
        && commits.last().is_some_and(|c| c["sha"] == head)
        && commits.iter().all(|c| {
            let message = c["commit"]["message"].as_str().unwrap_or("");
            c["author"]["login"] == actor
                && (c["committer"]["login"] == actor || c["committer"]["login"] == "web-flow")
                && c["commit"]["verification"]["verified"] == true
                && trailer(message, "Patchula: ") == Some("v1")
                && trailer(message, "Patchula-Branch: ").is_some()
        }))
}

/// Inspect all heads, regardless of naming, and remove obsolete owned branches
/// with an atomic head/base lease. Dry runs deliberately perform no API requests.
pub async fn cleanup(
    client: &Client,
    repo: &str,
    base: &str,
    default_branch: &str,
    plans: &[Plan],
    dry_run: bool,
) -> Result<Vec<BranchAction>, String> {
    if dry_run {
        return Ok(Vec::new());
    }
    let refs: Vec<Value> = client
        .rest_pages(&format!("repos/{repo}/branches?per_page=100"), |_| false)
        .await
        .map_err(|e| e.to_string())?;
    let desired: BTreeSet<_> = plans.iter().map(|p| p.action.branch.as_str()).collect();
    let candidates: Vec<_> = refs
        .iter()
        .filter(|r| {
            r["name"]
                .as_str()
                .is_some_and(|n| n != default_branch && !desired.contains(n))
        })
        .collect();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let viewer = client
        .graphql("query{viewer{login}}", json!({}))
        .await
        .map_err(|e| e.to_string())?;
    let actor = viewer["viewer"]["login"]
        .as_str()
        .ok_or("missing viewer login")?;
    let metadata = client
        .get_fresh(&format!("repos/{repo}"))
        .await
        .map_err(|e| e.to_string())?;
    if metadata["default_branch"] != default_branch {
        return Err("default branch changed before cleanup; rerun".into());
    }
    let node = metadata["node_id"]
        .as_str()
        .ok_or("missing repository node ID")?;
    let mut actions = Vec::new();
    for r in candidates {
        let name = r["name"].as_str().ok_or("missing branch name")?;
        let head = r["commit"]["sha"].as_str().ok_or("missing branch SHA")?;
        if !owned(client, repo, head, actor).await? {
            continue;
        }
        delete_owned(client, node, default_branch, base, name, head).await?;
        actions.push(BranchAction {
            repository: repo.into(),
            branch: name.into(),
            base_commit: base.into(),
            status: ActionStatus::Deleted,
            reason: Some("obsolete patchula update".into()),
            message: String::new(),
            previous_commit: Some(head.into()),
            commit: None,
            verified: Some(true),
            lockfile_pending: false,
            files: Vec::new(),
        });
    }
    Ok(actions)
}

pub async fn delete_owned(
    client: &Client,
    node: &str,
    default_branch: &str,
    base: &str,
    branch: &str,
    head: &str,
) -> Result<(), String> {
    if branch == default_branch {
        return Err("refusing to delete default branch".into());
    }
    let result = client.graphql(REF_UPDATE, json!({"input":{"repositoryId":node,"refUpdates":[
        {"name":format!("refs/heads/{default_branch}"),"beforeOid":base,"afterOid":base,"force":false},
        {"name":format!("refs/heads/{branch}"),"beforeOid":head,"afterOid":"0000000000000000000000000000000000000000","force":true}
    ]}})).await.map_err(|e| e.to_string())?;
    if !result["updateRefs"].is_object() {
        return Err("missing deletion acknowledgement".into());
    }
    Ok(())
}
