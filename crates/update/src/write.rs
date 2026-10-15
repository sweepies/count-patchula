use crate::{Plan, Snapshot, blob_sha};
use futures_util::{StreamExt, stream};
use rustovate_config::Rebase;
use rustovate_github::Client;
use rustovate_report::ActionStatus;
use serde_json::{Value, json};

/// GitHub's Git Data GraphQL ref lease is atomic, including the base check.
pub const REF_UPDATE: &str =
    "mutation($input:UpdateRefsInput!){updateRefs(input:$input){clientMutationId}}";

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("GitHub response has no {key}"))
}
fn trailer<'a>(message: &'a str, key: &str) -> Option<&'a str> {
    let mut lines = message.lines().filter_map(|l| l.strip_prefix(key));
    let value = lines.next()?;
    lines.next().is_none().then_some(value)
}
fn ref_path(repo: &str, branch: &str) -> String {
    let mut url = reqwest::Url::parse(&format!(
        "https://api.github.com/repos/{repo}/git/ref/heads/"
    ))
    .expect("validated repo");
    url.path_segments_mut().unwrap().pop_if_empty().push(branch);
    url.path().into()
}
async fn fresh(client: &Client, path: &str) -> Result<Value, String> {
    let response = client.get(path).await.map_err(|e| e.to_string())?;
    serde_json::from_slice(&response.body).map_err(|e| e.to_string())
}

/// Preconditions run before creating Git objects; atomic leases protect races.
pub async fn apply(
    client: &Client,
    plan: &mut Plan,
    snapshot: &Snapshot,
    default_branch: &str,
    rebase: Rebase,
    dry_run: bool,
) {
    if plan.action.status != ActionStatus::Planned {
        return;
    }
    if dry_run {
        return;
    }
    let result = match prepare(
        client,
        &plan.action.repository,
        &plan.action.base_commit,
        default_branch,
    )
    .await
    {
        Ok(base) => apply_inner(client, plan, snapshot, rebase, &base, true).await,
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        plan.action.status = ActionStatus::Error;
        plan.action.reason = Some(error);
    }
}

struct Base {
    repository: String,
    commit: String,
    node_id: String,
    tree_sha: String,
    entries: Vec<Value>,
    default_branch: String,
}
async fn prepare(
    client: &Client,
    repo: &str,
    base_commit: &str,
    default_branch: &str,
) -> Result<Base, String> {
    let metadata = fresh(client, &format!("repos/{repo}")).await?;
    if metadata["default_branch"] != default_branch {
        return Err("default branch changed since extraction; rerun".into());
    }
    let head = fresh(client, &ref_path(repo, default_branch)).await?;
    if head["object"]["sha"] != base_commit {
        return Err("default branch moved since extraction; stale blob refusal, rerun".into());
    }
    let base = fresh(client, &format!("repos/{repo}/git/commits/{base_commit}")).await?;
    let tree_sha = field(&base["tree"], "sha")?.to_owned();
    let tree = fresh(
        client,
        &format!("repos/{repo}/git/trees/{tree_sha}?recursive=1"),
    )
    .await?;
    if tree["truncated"] != false {
        return Err("truncated tree; cannot validate extracted blobs".into());
    }
    Ok(Base {
        default_branch: default_branch.into(),
        repository: repo.into(),
        commit: base_commit.into(),
        node_id: field(&metadata, "node_id")?.into(),
        tree_sha,
        entries: tree["tree"].as_array().ok_or("missing Git tree")?.clone(),
    })
}

/// Shared base validation, bounded independent branch preparation and governed writes.
/// Dry runs end at the pure planner and send no GitHub requests here.
pub async fn apply_all(
    client: &Client,
    plans: &mut [Plan],
    snapshot: &Snapshot,
    default_branch: &str,
    rebase: Rebase,
    dry_run: bool,
) {
    if dry_run {
        return;
    }
    let Some(first) = plans
        .iter()
        .find(|p| p.action.status == ActionStatus::Planned)
    else {
        return;
    };
    let base = prepare(
        client,
        &first.action.repository,
        &first.action.base_commit,
        default_branch,
    )
    .await;
    let pending: std::collections::BTreeSet<_> = plans
        .iter()
        .filter(|p| p.action.status == ActionStatus::Planned)
        .map(|p| p.action.branch.clone())
        .collect();
    stream::iter(plans.iter_mut())
        .map(|plan| {
            let base = &base;
            async move {
                if plan.action.status != ActionStatus::Planned {
                    return;
                }
                let result = match base {
                    Ok(base) => apply_inner(client, plan, snapshot, rebase, base, false).await,
                    Err(error) => Err(error.clone()),
                };
                if let Err(error) = result {
                    plan.action.status = ActionStatus::Error;
                    plan.action.reason = Some(error);
                }
            }
        })
        .buffer_unordered(client.concurrency().min(4))
        .collect::<Vec<_>>()
        .await;
    if let Ok(base) = &base {
        // Bound each atomic mutation to fifty refs, including the base lease.
        for chunk in plans.chunks_mut(49) {
            let eligible = |p: &Plan| {
                pending.contains(&p.action.branch)
                    && matches!(
                        p.action.status,
                        ActionStatus::Created | ActionStatus::Updated | ActionStatus::Rebased
                    )
            };
            let mut refs = vec![
                json!({"name":format!("refs/heads/{}",base.default_branch),"beforeOid":base.commit,"afterOid":base.commit,"force":false}),
            ];
            for p in chunk.iter().filter(|p| eligible(p)) {
                refs.push(json!({"name":format!("refs/heads/{}",p.action.branch),"beforeOid":p.action.previous_commit.as_deref().unwrap_or("0000000000000000000000000000000000000000"),"afterOid":p.action.commit,"force":true}));
            }
            if refs.len() == 1 {
                continue;
            }
            let result = client
                .graphql(
                    REF_UPDATE,
                    json!({"input":{"repositoryId":base.node_id,"refUpdates":refs}}),
                )
                .await;
            let error = match result {
                Ok(value) if value["updateRefs"].is_object() => None,
                Ok(_) => {
                    Some("GitHub did not acknowledge atomic batch ref publication; rerun".into())
                }
                Err(e) => Some(format!(
                    "atomic batch ref lease failed; inspect current heads and rerun: {e}"
                )),
            };
            if let Some(error) = error {
                for p in chunk.iter_mut().filter(|p| eligible(p)) {
                    p.action.status = ActionStatus::Error;
                    p.action.reason = Some(error.clone());
                    p.action.commit = None;
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
    publish_ref: bool,
) -> Result<(), String> {
    let default_branch = &base.default_branch;
    let repo = plan.action.repository.clone();
    if repo != base.repository || plan.action.base_commit != base.commit {
        return Err("mixed repositories or base commits in write batch".into());
    }
    if plan.action.branch == *default_branch {
        return Err("update branch equals default branch".into());
    }
    if plan.contents.len() != plan.action.files.len()
        || plan
            .action
            .files
            .iter()
            .any(|f| !plan.contents.contains_key(&f.path))
    {
        return Err("plan contents and file preconditions differ".into());
    }
    let node_id = &base.node_id;
    let tree_sha = &base.tree_sha;
    let entries = &base.entries;
    for file in &plan.action.files {
        let blob = snapshot
            .files
            .get(&file.path)
            .ok_or("missing extracted blob")?;
        if blob.sha != file.blob_sha
            || blob_sha(&blob.content) != blob.sha
            || !entries.iter().any(|e| {
                e["path"] == file.path
                    && e["sha"] == blob.sha
                    && e["mode"] == blob.mode
                    && e["type"] == "blob"
            })
        {
            return Err(format!(
                "{}: stale blob SHA or contents; refusing all branch writes",
                file.path
            ));
        }
    }
    let old = match client.get(&ref_path(&repo, &plan.action.branch)).await {
        Ok(response) => {
            let value: Value = serde_json::from_slice(&response.body).map_err(|e| e.to_string())?;
            Some(field(&value["object"], "sha")?.to_owned())
        }
        Err(rustovate_github::Error::Status(404)) => None,
        Err(e) => return Err(e.to_string()),
    };
    plan.action.previous_commit = old.clone();
    let mut moved_base = false;
    if let Some(old) = &old {
        let viewer = client
            .graphql("query{viewer{login}}", json!({}))
            .await
            .map_err(|e| e.to_string())?;
        let actor = field(&viewer["viewer"], "login")?;
        let existing_tip = fresh(client, &format!("repos/{repo}/git/commits/{old}")).await?;
        let Some(recorded) = existing_tip["message"]
            .as_str()
            .and_then(|m| trailer(m, "Rustovate-Base: "))
        else {
            plan.action.status = ActionStatus::Protected;
            plan.action.reason = Some("unmarked branch history; left untouched".into());
            return Ok(());
        };
        if recorded.is_empty()
            || recorded.len() > 64
            || !recorded.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            plan.action.status = ActionStatus::Protected;
            plan.action.reason = Some("invalid recorded branch base; left untouched".into());
            return Ok(());
        }
        let compare = fresh(
            client,
            &format!("repos/{repo}/compare/{}...{old}?per_page=100", recorded),
        )
        .await?;
        let commits = compare["commits"]
            .as_array()
            .ok_or("missing branch commit history")?;
        let total = compare["total_commits"]
            .as_u64()
            .ok_or("missing branch commit count")?;
        let owned = total > 0
            && total == commits.len() as u64
            && commits.iter().all(|c| {
                let message = c["commit"]["message"].as_str().unwrap_or("");
                c["author"]["login"] == actor
                    && (c["committer"]["login"] == actor || c["committer"]["login"] == "web-flow")
                    && c["commit"]["verification"]["verified"] == true
                    && trailer(message, "Rustovate: ") == Some("v1")
                    && trailer(message, "Rustovate-Branch: ").is_some()
            });
        if !owned {
            plan.action.status = ActionStatus::Protected;
            plan.action.reason = Some("branch contains human, unverified, unmarked or uninspectable commits; left untouched".into());
            return Ok(());
        }
        let latest = commits.last().ok_or("empty branch history")?;
        if latest["sha"] != *old {
            return Err("branch history does not end at the observed head".into());
        }
        let message = latest["commit"]["message"]
            .as_str()
            .ok_or("missing commit message")?;
        let parent = trailer(message, "Rustovate-Base: ").ok_or("missing rustovate base marker")?;
        moved_base = parent != plan.action.base_commit;
        if !plan.retry
            && trailer(message, "Rustovate-Plan: ") == Some(&plan.fingerprint)
            && (!moved_base || rebase == Rebase::Conflicts)
        {
            let existing_tree = field(&existing_tip["tree"], "sha")?;
            let existing = fresh(
                client,
                &format!("repos/{repo}/git/trees/{existing_tree}?recursive=1"),
            )
            .await?;
            let entries = existing["tree"]
                .as_array()
                .filter(|_| existing["truncated"] == false)
                .ok_or("incomplete existing branch tree")?;
            if plan.contents.iter().all(|(p, c)| {
                entries
                    .iter()
                    .any(|e| e["path"] == *p && e["sha"] == blob_sha(c))
            }) {
                plan.action.status = ActionStatus::Unchanged;
                plan.action.commit = Some(old.clone());
                plan.action.verified = Some(true);
                plan.tree = Some(existing_tree.into());
                return Ok(());
            }
        }
    }
    let blobs: Vec<_> = plan.contents.iter().map(|(path, content)| {
        json!({"path":path,"mode":snapshot.files[path].mode,"type":"blob","content":content})
    }).collect();
    let tree = client
        .post(
            &format!("repos/{repo}/git/trees"),
            json!({"base_tree":tree_sha,"tree":blobs}),
        )
        .await
        .map_err(|e| e.to_string())?;
    let tree = field(&tree, "sha")?;
    plan.tree = Some(tree.into());
    let pending = if plan.action.lockfile_pending {
        "\n\nLockfile pending: manifest-only test branch; regenerate before merging (SWE-109)."
    } else {
        ""
    };
    let message = format!(
        "{}{}\n\nRustovate: v1\nRustovate-Branch: {}\nRustovate-Base: {}\nRustovate-Plan: {}",
        plan.action.message, pending, plan.identity, plan.action.base_commit, plan.fingerprint
    );
    // App bot signing requires GitHub-supplied authorship and signature.
    let commit = client
        .post(
            &format!("repos/{repo}/git/commits"),
            json!({"message":message,"tree":tree,"parents":[plan.action.base_commit]}),
        )
        .await
        .map_err(|e| e.to_string())?;
    plan.action.verified = commit["verification"]["verified"].as_bool();
    if plan.action.verified != Some(true) {
        return Err("GitHub returned an unverified Git Data commit; branch left untouched. Check App bot signing; createCommitOnBranch may be required".into());
    }
    let sha = field(&commit, "sha")?;
    if publish_ref {
        let zero = "0000000000000000000000000000000000000000";
        let published = client.graphql(REF_UPDATE, json!({"input":{"repositoryId":node_id,"refUpdates":[
        {"name":format!("refs/heads/{default_branch}"),"beforeOid":plan.action.base_commit,"afterOid":plan.action.base_commit,"force":false},
        {"name":format!("refs/heads/{}",plan.action.branch),"beforeOid":old.as_deref().unwrap_or(zero),"afterOid":sha,"force":true}
    ]}})).await.map_err(|e| format!("atomic ref lease failed; branch may have changed, rerun: {e}"))?;
        if !published["updateRefs"].is_object() {
            return Err(
            "GitHub did not acknowledge the ref update; inspect the current branch before retrying"
                .into(),
        );
        }
    }
    plan.action.commit = Some(sha.into());
    plan.action.status = if old.is_none() {
        ActionStatus::Created
    } else if moved_base {
        ActionStatus::Rebased
    } else {
        ActionStatus::Updated
    };
    if plan.action.lockfile_pending {
        plan.action.reason =
            Some("lockfile pending: manifest-only test branch; regenerate before merging".into());
    }
    Ok(())
}
