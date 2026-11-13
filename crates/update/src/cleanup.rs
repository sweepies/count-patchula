use crate::error::Error;
use crate::{Inventory, Plan, REF_UPDATE, trailers};
use futures_util::future::join_all;
use patchula_cache::{Cache, State};
use patchula_github::{
    Client,
    model::{BranchHead, Commit, Comparison, GitRef, Repository},
};
use patchula_report::{BranchAction, BranchStatus};
use serde::{Deserialize, Serialize};
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

/// A head commit whose message records no base, so it cannot be the
/// Count's. Only that answer is stored: it depends on nothing but the
/// immutable commit and the trailer parsing `tests/state-versions.tsv`
/// pins (hence code-versioned), so it is keyed by the commit OID and not
/// read again. A compare GitHub cannot answer, or history by another
/// author, depends on the run's actor and on GitHub, and is probed every
/// run; so are positive answers, whose branch the same run deletes.
#[derive(Serialize, Deserialize)]
pub(crate) struct NotOurs;
#[derive(Serialize)]
pub(crate) struct NotOursScope {
    repository: String,
    oid: String,
}
impl State for NotOurs {
    const NAMESPACE: &'static str = "not-ours";
    const VERSION: u32 = 2;
    const CODE_VERSIONED: bool = true;
    type Scope = NotOursScope;
}

/// The compare and signature checks of [`owned`], once the tip's recorded
/// base is known — from a fresh tip read, or from the inventory's copy of
/// the same commit.
async fn proven(
    client: &Client,
    repo: &str,
    base: &str,
    head: &str,
    actor: &str,
) -> Result<bool, Error> {
    let history = match client
        .get_fresh::<Comparison>(&format!(
            "repos/{repo}/compare/{base}...{head}?per_page=100"
        ))
        .await
    {
        // A recorded base GitHub cannot compare is no evidence of ownership:
        // the branch is not the Count's, and the run carries on.
        Err(patchula_github::Error::Status(404 | 422)) => return Ok(false),
        result => result?,
    };
    Ok(history.total_commits == history.commits.len() as u64
        && history.commits.last().is_some_and(|c| c.sha == head)
        && all_ours(&history.commits, actor))
}

/// Validate every commit since the recorded base, including author and signature.
/// A copied marker alone never authorizes deleting or replacing human work.
pub async fn owned(client: &Client, repo: &str, head: &str, actor: &str) -> Result<bool, Error> {
    let tip = client
        .get_fresh::<Commit>(&format!("repos/{repo}/commits/{head}"))
        .await?;
    let Some(base) = recorded_base(&tip.commit.message) else {
        return Ok(false);
    };
    proven(client, repo, base, head, actor).await
}

/// What answering `owned` for one candidate still costs: the inventory's
/// copy of the tip carries a recorded base, or nothing answered the tip
/// read and the whole probe runs.
enum Probe {
    /// Only the compare and signature checks remain.
    Compare(String),
    /// The full probe; a tip with no recorded base is kept in `not-ours`
    /// state.
    Full,
}

/// Inspect all heads, regardless of naming, and remove obsolete owned branches
/// with a lease on the branch head, once the default branch is confirmed not to
/// have moved since it was read. `inventory` is the run's one GraphQL
/// inventory: its copy of a ref's tip answers `owned`'s first step when the
/// branch still points at the commit the inventory saw, and `not-ours` state
/// answers heads seen before. The rest are probed eight at a time, while
/// deletions stay in list order, one at a time, and stop at the first failed
/// probe, so the outcome on an error is unchanged. `inventory.actor` is the
/// run's one viewer login, read with the inventory. Dry runs deliberately
/// perform no API requests.
#[allow(clippy::too_many_arguments)]
pub async fn cleanup(
    client: &Client,
    cache: &Cache,
    repo: &str,
    base: &str,
    default_branch: &str,
    plans: &[Plan],
    inventory: &Inventory,
    dry_run: bool,
) -> Result<Vec<BranchAction>, Error> {
    if dry_run {
        return Ok(Vec::new());
    }
    let actor = inventory.actor.as_str();
    let refs = client
        .rest_pages::<BranchHead>(&format!("repos/{repo}/branches?per_page=100"), |_| false)
        .await?;
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
        .await?;
    if metadata.default_branch != default_branch {
        return Err(Error::Raced("default branch changed before cleanup; rerun"));
    }
    // The deletions don't lease the default branch, so check here that the
    // plans still describe it; if it moved, the next run cleans up instead.
    let current = client
        .get_fresh::<GitRef>(&crate::write::ref_path(repo, default_branch))
        .await?;
    if current.object.sha != base {
        return Ok(Vec::new());
    }
    // The inventory's view of each ref's tip. A head that moved since the
    // inventory read, or a ref the inventory does not hold, misses this map
    // and goes through `not-ours` state or a probe below.
    let tips: std::collections::BTreeMap<&str, &crate::receipts::RefCommit> = inventory
        .refs
        .items
        .iter()
        .flatten()
        .filter_map(|r| match &r.target {
            Some(crate::receipts::RefTarget::Commit(commit)) => Some((r.name.as_str(), commit)),
            _ => None,
        })
        .collect();
    let mut work = Vec::new();
    for r in candidates {
        let head = r.commit.sha.as_str();
        match tips.get(r.name.as_str()) {
            Some(tip) if tip.oid == head => match recorded_base(&tip.message) {
                // The inventory's copy of this very commit carries no
                // recorded base: `owned`'s first step answers "not ours"
                // with no request.
                None => continue,
                Some(base) => work.push((r, Probe::Compare(base.to_owned()))),
            },
            _ => {
                let scope = NotOursScope {
                    repository: repo.into(),
                    oid: head.into(),
                };
                if cache.state::<NotOurs>(&scope).await?.is_some() {
                    continue;
                }
                work.push((r, Probe::Full));
            }
        }
    }
    let node = &metadata.node_id;
    let mut actions = Vec::new();
    // Probe eight candidates at a time. The whole batch's requests are made
    // together before the next batch starts, so a failed probe may overlap
    // the rest of its batch but no deletion happens past it — the same
    // deletions and the same error as sequential probes produce.
    for chunk in work.chunks(8) {
        // Each result says whether the commit is the Count's, and whether a
        // negative answer came from the tip's message alone.
        let results = join_all(chunk.iter().map(|(r, probe)| {
            let head = r.commit.sha.as_str();
            async move {
                match probe {
                    Probe::Compare(base) => (proven(client, repo, base, head, actor).await, false),
                    Probe::Full => {
                        let tip = match client
                            .get_fresh::<Commit>(&format!("repos/{repo}/commits/{head}"))
                            .await
                        {
                            Ok(tip) => tip,
                            Err(error) => return (Err(error.into()), false),
                        };
                        match recorded_base(&tip.commit.message) {
                            None => (Ok(false), true),
                            Some(base) => (proven(client, repo, base, head, actor).await, false),
                        }
                    }
                }
            }
        }))
        .await;
        for ((r, _), (result, unrecorded)) in chunk.iter().zip(results) {
            if !result? {
                if unrecorded {
                    cache
                        .set_state::<NotOurs>(
                            &NotOursScope {
                                repository: repo.into(),
                                oid: r.commit.sha.clone(),
                            },
                            &NotOurs,
                        )
                        .await?;
                }
                continue;
            }
            let name = &r.name;
            let head = &r.commit.sha;
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
    }
    Ok(actions)
}

pub async fn delete_owned(
    client: &Client,
    node: &str,
    default_branch: &str,
    branch: &str,
    head: &str,
) -> Result<(), Error> {
    if branch == default_branch {
        return Err(Error::Refused("refusing to delete default branch".into()));
    }
    let result = client.graphql::<crate::write::RefUpdates>(REF_UPDATE, json!({"input":{"repositoryId":node,"refUpdates":[
        {"name":format!("refs/heads/{branch}"),"beforeOid":head,"afterOid":"0000000000000000000000000000000000000000","force":true}
    ]}})).await?;
    if result.update_refs.is_none() {
        return Err(Error::Refused("missing deletion acknowledgement".into()));
    }
    Ok(())
}
