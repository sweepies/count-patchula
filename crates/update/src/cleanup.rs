use crate::{Plan, REF_UPDATE, trailers};
use patchula_github::{
    Client,
    model::{BranchHead, Commit, Comparison, GitRef, Repository},
};
use patchula_report::{BranchAction, BranchStatus};
use serde_json::json;
use std::collections::BTreeSet;

/// The base commit a branch tip records, when it can name one in a URL path:
/// up to 64 ASCII letters and digits, which covers SHA-1 and SHA-256 names.
/// Writing and cleanup both decide ownership from this one reading. The base
/// only bounds the history inspected; ownership itself comes from every
/// commit since it being the Count's own (see [`all_ours`]).
pub(crate) fn recorded_base(message: &str) -> Option<&str> {
    trailers::base(message).filter(|base| {
        !base.is_empty() && base.len() <= 64 && base.bytes().all(|b| b.is_ascii_alphanumeric())
    })
}

/// Whether every commit is the Count's own: by `actor`, verified and marked.
pub(crate) fn all_ours(commits: &[Commit], actor: &str) -> bool {
    !commits.is_empty()
        && commits.iter().all(|c| {
            let message = c.commit.message.as_str();
            c.author.as_ref().is_some_and(|a| a.login == actor)
                && c.committer
                    .as_ref()
                    .is_some_and(|a| a.login == actor || a.login == "web-flow")
                && c.commit.verification.as_ref().is_some_and(|v| v.verified)
                && trailers::version(message)
                && trailers::branch(message).is_some()
        })
}

/// Validate every commit since the recorded base, including author and signature.
/// A copied marker alone never authorizes deleting or replacing human work.
pub async fn owned(client: &Client, repo: &str, head: &str, actor: &str) -> Result<bool, String> {
    let tip = client
        .get_fresh::<Commit>(&format!("repos/{repo}/commits/{head}"))
        .await
        .map_err(|e| e.to_string())?;
    let Some(base) = recorded_base(&tip.commit.message) else {
        return Ok(false);
    };
    let history = match client
        .get_fresh::<Comparison>(&format!(
            "repos/{repo}/compare/{base}...{head}?per_page=100"
        ))
        .await
    {
        // A recorded base GitHub cannot compare is no evidence of ownership:
        // the branch is not the Count's, and the run carries on.
        Err(patchula_github::Error::Status(404 | 422)) => return Ok(false),
        result => result.map_err(|e| e.to_string())?,
    };
    Ok(history.total_commits == history.commits.len() as u64
        && history.commits.last().is_some_and(|c| c.sha == head)
        && all_ours(&history.commits, actor))
}

/// Inspect all heads, regardless of naming, and remove obsolete owned branches
/// with a lease on the branch head, once the default branch is confirmed not to
/// have moved since it was read. `actor` is the run's one viewer login, read
/// with the inventory. Dry runs deliberately perform no API requests.
pub async fn cleanup(
    client: &Client,
    repo: &str,
    base: &str,
    default_branch: &str,
    plans: &[Plan],
    actor: &str,
    dry_run: bool,
) -> Result<Vec<BranchAction>, String> {
    if dry_run {
        return Ok(Vec::new());
    }
    let refs = client
        .rest_pages::<BranchHead>(&format!("repos/{repo}/branches?per_page=100"), |_| false)
        .await
        .map_err(|e| e.to_string())?;
    let desired: BTreeSet<_> = plans.iter().map(|p| p.action().branch.as_str()).collect();
    let candidates: Vec<_> = refs
        .iter()
        .filter(|r| r.name != default_branch && !desired.contains(r.name.as_str()))
        .collect();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let metadata = client
        .get_fresh::<Repository>(&format!("repos/{repo}"))
        .await
        .map_err(|e| e.to_string())?;
    if metadata.default_branch != default_branch {
        return Err("default branch changed before cleanup; rerun".into());
    }
    // The deletions don't lease the default branch, so check here that the
    // plans still describe it; if it moved, the next run cleans up instead.
    let current = client
        .get_fresh::<GitRef>(&crate::write::ref_path(repo, default_branch))
        .await
        .map_err(|e| e.to_string())?;
    if current.object.sha != base {
        return Ok(Vec::new());
    }
    let node = &metadata.node_id;
    let mut actions = Vec::new();
    for r in candidates {
        let name = &r.name;
        let head = &r.commit.sha;
        if !owned(client, repo, head, actor).await? {
            continue;
        }
        delete_owned(client, node, default_branch, name, head).await?;
        actions.push(BranchAction {
            repository: repo.into(),
            branch: name.clone(),
            base_commit: base.into(),
            status: BranchStatus::Deleted,
            reason: Some("obsolete patchula update".into()),
            message: String::new(),
            previous_commit: Some(head.clone()),
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
    branch: &str,
    head: &str,
) -> Result<(), String> {
    if branch == default_branch {
        return Err("refusing to delete default branch".into());
    }
    let result = client.graphql::<crate::write::RefUpdates>(REF_UPDATE, json!({"input":{"repositoryId":node,"refUpdates":[
        {"name":format!("refs/heads/{branch}"),"beforeOid":head,"afterOid":"0000000000000000000000000000000000000000","force":true}
    ]}})).await.map_err(|e| e.to_string())?;
    if !result.acknowledged() {
        return Err("missing deletion acknowledgement".into());
    }
    Ok(())
}
