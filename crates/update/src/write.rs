use crate::cleanup::{all_ours, recorded_base};
use crate::{Plan, Snapshot, blob_sha, trailers};
use futures_util::{FutureExt, StreamExt, stream};
use patchula_config::Rebase;
use patchula_github::{
    Client,
    model::{Comparison, GitCommit, GitRef, Repository, Tree, TreeEntry},
};
use patchula_report::BranchStatus;
use serde::Deserialize;
use serde_json::{Value, json};

#[allow(dead_code)]
mod ref_update {
    use graphql_client::GraphQLQuery;

    /// Scalars the generated variables resolve as `super::*`.
    type GitObjectID = String;
    type GitRefname = String;

    /// `ref-update.graphql` — GitHub's Git Data ref update, atomic across
    /// the branch refs it names — checked against the vendored schema.
    #[derive(GraphQLQuery)]
    #[graphql(
        schema_path = "../github/graphql/schema.docs.graphql",
        query_path = "src/ref-update.graphql",
        response_derives = "Debug"
    )]
    pub struct RefUpdate;
}
pub(crate) use ref_update::ref_update::ResponseData as RefUpdates;

/// The updateRefs document, exported for tests that compare request bodies.
/// The default branch is never named: rulesets that require a pull request
/// reject even a no-op update to it, so the base is checked when it is read.
pub const REF_UPDATE: &str = ref_update::ref_update::QUERY;

/// The `git-tree` fields a tree creation returns that the write reads: the
/// OpenAPI `git-tree` schema's required `sha`.
#[derive(Deserialize)]
struct CreatedTree {
    sha: String,
}

/// Why a write did not happen.
#[derive(Clone, Debug)]
enum Refusal {
    /// An expected extraction race: the default branch changed or moved, and
    /// no write was attempted against the stale base. A rerun retries it.
    Stale(&'static str),
    Failed(String),
}
impl From<String> for Refusal {
    fn from(error: String) -> Self {
        Self::Failed(error)
    }
}
impl From<&str> for Refusal {
    fn from(error: &str) -> Self {
        Self::Failed(error.into())
    }
}

/// Whether a branch's write should run again: its base went stale before
/// anything was written.
pub fn needs_requeue(action: &patchula_report::BranchAction) -> bool {
    action.status == BranchStatus::Stale
}

fn refuse(plan: &mut Plan, refusal: Refusal) {
    match refusal {
        Refusal::Stale(reason) => plan.stale(reason),
        Refusal::Failed(error) => plan.error(error),
    }
}

pub(crate) fn ref_path(repo: &str, branch: &str) -> String {
    let mut url = reqwest::Url::parse(&format!(
        "https://api.github.com/repos/{repo}/git/ref/heads/"
    ))
    .expect("validated repo");
    url.path_segments_mut().unwrap().pop_if_empty().push(branch);
    url.path().into()
}

struct Base {
    repository: String,
    commit: String,
    node_id: String,
    tree_sha: String,
    entries: Vec<TreeEntry>,
    default_branch: String,
    /// The run's one viewer login, read with the inventory.
    actor: String,
}
async fn prepare(
    client: &Client,
    repo: &str,
    base_commit: &str,
    default_branch: &str,
    actor: &str,
) -> Result<Base, Refusal> {
    let metadata = client
        .get_fresh::<Repository>(&format!("repos/{repo}"))
        .await
        .map_err(|e| e.to_string())?;
    if metadata.default_branch != default_branch {
        return Err(Refusal::Stale(
            "default branch changed since extraction; rerun",
        ));
    }
    let head = client
        .get_fresh::<GitRef>(&ref_path(repo, default_branch))
        .await
        .map_err(|e| e.to_string())?;
    if head.object.sha != base_commit {
        return Err(Refusal::Stale(
            "default branch moved since extraction; stale blob refusal, rerun",
        ));
    }
    let base = client
        .get_fresh::<GitCommit>(&format!("repos/{repo}/git/commits/{base_commit}"))
        .await
        .map_err(|e| e.to_string())?;
    let tree_sha = base.tree.sha;
    let tree = client
        .get_fresh::<Tree>(&format!("repos/{repo}/git/trees/{tree_sha}?recursive=1"))
        .await
        .map_err(|e| e.to_string())?;
    if tree.truncated {
        return Err("truncated tree; cannot validate extracted blobs".into());
    }
    Ok(Base {
        default_branch: default_branch.into(),
        repository: repo.into(),
        commit: base_commit.into(),
        node_id: metadata.node_id,
        tree_sha,
        entries: tree.tree,
        actor: actor.into(),
    })
}

/// Shared base validation, bounded independent branch preparation and governed writes.
/// `actor` is the run's one viewer login, read with the inventory. Dry runs
/// end at the pure planner and send no GitHub requests here.
pub async fn apply_all(
    client: &Client,
    plans: &mut [Plan],
    snapshot: &Snapshot,
    default_branch: &str,
    actor: &str,
    rebase: Rebase,
    dry_run: bool,
) {
    if dry_run {
        return;
    }
    for plan in plans
        .iter_mut()
        .filter(|p| p.action().status == BranchStatus::Planned && !p.lockfiles_ready)
    {
        plan.error("lockfiles must be generated successfully before publication");
    }
    let Some(first) = plans
        .iter()
        .find(|p| p.action().status == BranchStatus::Planned)
    else {
        return;
    };
    let base = prepare(
        client,
        &first.action().repository,
        &first.action().base_commit,
        default_branch,
        actor,
    )
    .await;
    let pending: std::collections::BTreeSet<_> = plans
        .iter()
        .filter(|p| p.action().status == BranchStatus::Planned)
        .map(|p| p.action().branch.clone())
        .collect();
    // Build the borrowed futures before awaiting the stream. This also keeps
    // the engine future Send when a long-lived server spawns its batch worker.
    let writes: Vec<_> = plans
        .iter_mut()
        .map(|plan: &mut Plan| {
            let base = &base;
            async move {
                if plan.action().status != BranchStatus::Planned {
                    return;
                }
                let result = match base {
                    Ok(base) => apply_inner(client, plan, snapshot, rebase, base)
                        .await
                        .map_err(Refusal::Failed),
                    Err(refusal) => Err(refusal.clone()),
                };
                if let Err(error) = result {
                    refuse(plan, error);
                }
            }
            .boxed()
        })
        .collect();
    stream::iter(writes)
        .buffer_unordered(client.concurrency().min(4))
        .collect::<Vec<_>>()
        .await;
    if let Ok(base) = &base {
        // Bound each atomic mutation to fifty branch refs. The default branch is
        // never named: rulesets that require pull requests reject even a no-op
        // update of it.
        for chunk in plans.chunks_mut(50) {
            let eligible = |p: &Plan| {
                pending.contains(&p.action().branch)
                    && matches!(
                        p.action().status,
                        BranchStatus::Created | BranchStatus::Updated | BranchStatus::Rebased
                    )
            };
            let mut refs = Vec::new();
            for p in chunk.iter().filter(|p| eligible(p)) {
                refs.push(json!({"name":format!("refs/heads/{}",p.action().branch),"beforeOid":p.action().previous_commit.as_deref().unwrap_or("0000000000000000000000000000000000000000"),"afterOid":p.action().commit,"force":true}));
            }
            if refs.is_empty() {
                continue;
            }
            let result = client
                .graphql::<RefUpdates>(
                    REF_UPDATE,
                    json!({"input":{"repositoryId":base.node_id,"refUpdates":refs}}),
                )
                .await;
            let error = match result {
                Ok(value) if value.update_refs.is_some() => None,
                Ok(_) => {
                    Some("GitHub did not acknowledge atomic batch ref publication; rerun".into())
                }
                Err(e) => Some(format!(
                    "atomic batch ref lease failed; inspect current heads and rerun: {e}"
                )),
            };
            if let Some(error) = error {
                for p in chunk.iter_mut().filter(|p| eligible(p)) {
                    p.error(error.clone());
                }
            }
        }
    }
}
async fn apply_inner(
    client: &Client,
    plan: &mut Plan,
    snapshot: &Snapshot,
    rebase: Rebase,
    base: &Base,
) -> Result<(), String> {
    let default_branch = &base.default_branch;
    let repo = plan.action().repository.clone();
    if repo != base.repository || plan.action().base_commit != base.commit {
        return Err("mixed repositories or base commits in write batch".into());
    }
    if plan.action().branch == *default_branch {
        return Err("update branch equals default branch".into());
    }
    if plan.contents.len() != plan.action().files.len()
        || plan
            .action()
            .files
            .iter()
            .any(|f| !plan.contents.contains_key(&f.path))
    {
        return Err("plan contents and file preconditions differ".into());
    }
    let tree_sha = &base.tree_sha;
    let entries = &base.entries;
    for file in &plan.action().files {
        let blob = snapshot
            .files
            .get(&file.path)
            .ok_or("missing extracted blob")?;
        if file.blob_sha.is_empty() {
            if !blob.sha.is_empty() || entries.iter().any(|e| e.path == file.path) {
                return Err(format!(
                    "{}: expected new file is already present",
                    file.path
                ));
            }
            continue;
        }
        if blob.sha != file.blob_sha
            || blob_sha(&blob.content) != blob.sha
            || !entries.iter().any(|e| {
                e.path == file.path && e.sha == blob.sha && e.mode == blob.mode && e.kind == "blob"
            })
        {
            return Err(format!(
                "{}: stale blob SHA or contents; refusing all branch writes",
                file.path
            ));
        }
    }
    let old = match client
        .get_fresh::<GitRef>(&ref_path(&repo, &plan.action().branch))
        .await
    {
        Ok(reference) => Some(reference.object.sha),
        Err(patchula_github::Error::Status(404)) => None,
        Err(e) => return Err(e.to_string()),
    };
    plan.record_head(old.clone());
    let mut moved_base = false;
    if let Some(old) = &old {
        let actor = base.actor.as_str();
        let existing_tip = client
            .get_fresh::<GitCommit>(&format!("repos/{repo}/git/commits/{old}"))
            .await
            .map_err(|e| e.to_string())?;
        let tip_message = existing_tip.message.as_str();
        if trailers::base(tip_message).is_none() {
            plan.protect("unmarked branch history; left untouched");
            return Ok(());
        }
        let Some(recorded) = recorded_base(tip_message) else {
            plan.protect("invalid recorded branch base; left untouched");
            return Ok(());
        };
        let compare = match client
            .get_fresh::<Comparison>(&format!(
                "repos/{repo}/compare/{recorded}...{old}?per_page=100"
            ))
            .await
        {
            Err(patchula_github::Error::Status(404 | 422)) => {
                plan.protect("recorded branch base is not a commit here; left untouched");
                return Ok(());
            }
            result => result.map_err(|e| e.to_string())?,
        };
        let commits = &compare.commits;
        let total = compare.total_commits;
        let owned = total == commits.len() as u64 && all_ours(commits, actor);
        if !owned {
            plan.protect(
                "branch contains human, unverified, unmarked or uninspectable commits; left untouched",
            );
            return Ok(());
        }
        let latest = commits.last().ok_or("empty branch history")?;
        if latest.sha != *old {
            return Err("branch history does not end at the observed head".into());
        }
        let message = latest.commit.message.as_str();
        let parent = trailers::base(message).ok_or("missing patchula base marker")?;
        moved_base = parent != plan.action().base_commit;
        if !moved_base
            || rebase == Rebase::Conflicts
                && trailers::plan(message) == Some(plan.fingerprint.as_str())
        {
            let existing_tree = existing_tip.tree.sha;
            let existing = client
                .get_fresh::<Tree>(&format!(
                    "repos/{repo}/git/trees/{existing_tree}?recursive=1"
                ))
                .await
                .map_err(|e| e.to_string())?;
            if existing.truncated {
                return Err("incomplete existing branch tree".into());
            }
            let existing_entries = &existing.tree;
            let contents_match = plan.contents.iter().all(|(p, c)| {
                if plan.deleted.contains(p) {
                    return !existing_entries.iter().any(|e| e.path == *p);
                }
                existing_entries.iter().any(|e| {
                    e.path == *p
                        && e.sha == blob_sha(c)
                        && e.mode == snapshot.files[p].mode
                        && e.kind == "blob"
                })
            });
            // On the same base, also reject leftover edits from an older plan.
            // Directory OIDs change with children; compare leaf entries only.
            let leaves = |entries: &[TreeEntry]| -> std::collections::BTreeMap<String, Value> {
                entries
                    .iter()
                    .filter(|e| e.kind != "tree")
                    .map(|e| (e.path.clone(), json!([e.sha, e.mode, e.kind])))
                    .collect()
            };
            let mut expected = leaves(entries);
            for (path, content) in &plan.contents {
                if plan.deleted.contains(path) {
                    expected.remove(path);
                } else {
                    expected.insert(
                        path.clone(),
                        json!([blob_sha(content), snapshot.files[path].mode, "blob"]),
                    );
                }
            }
            if contents_match && (moved_base || leaves(existing_entries) == expected) {
                plan.unchanged(old.clone(), existing_tree);
                return Ok(());
            }
        }
    }
    let blobs: Vec<_> = plan.contents.iter().map(|(path, content)| {
        if plan.deleted.contains(path) {json!({"path":path,"mode":snapshot.files[path].mode,"type":"blob","sha":null})}
        else {json!({"path":path,"mode":snapshot.files[path].mode,"type":"blob","content":content})}
    }).collect();
    let tree = client
        .post::<CreatedTree>(
            &format!("repos/{repo}/git/trees"),
            json!({"base_tree":tree_sha,"tree":blobs}),
        )
        .await
        .map_err(|e| e.to_string())?;
    let tree = tree.sha;
    plan.tree = Some(tree.clone());
    let pending = if plan.action().lockfile_pending {
        "\n\nLockfile pending: manifest-only test branch; regenerate before merging."
    } else {
        ""
    };
    let message = format!(
        "{}{}\n\n{}",
        plan.action().message,
        pending,
        trailers::block(
            &plan.identity,
            &plan.action().base_commit,
            &plan.fingerprint
        )
    );
    // App bot signing requires GitHub-supplied authorship and signature.
    let commit = client
        .post::<GitCommit>(
            &format!("repos/{repo}/git/commits"),
            json!({"message":message,"tree":tree,"parents":[plan.action().base_commit]}),
        )
        .await
        .map_err(|e| e.to_string())?;
    let verified = commit.verification.verified;
    plan.set_verified(Some(verified));
    if !verified {
        return Err("GitHub returned an unverified Git Data commit; branch left untouched. Check App bot signing; createCommitOnBranch may be required".into());
    }
    let sha = commit.sha;
    let status = if old.is_none() {
        BranchStatus::Created
    } else if moved_base {
        BranchStatus::Rebased
    } else {
        BranchStatus::Updated
    };
    plan.committed(sha, status);
    Ok(())
}
