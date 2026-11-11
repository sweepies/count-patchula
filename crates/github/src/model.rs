//! Typed GitHub REST resources the client decodes responses into: the git
//! data types the write path reads, plus [`Page`] for lists. Every type is
//! `Deserialize` only — never `Serialize`, so a decoded object cannot be
//! sent back to GitHub — and ignores unknown fields. Required means
//! required: a field is `Option` only where GitHub's OpenAPI description
//! documents null, cited on the field.
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;

/// A GitHub user as REST resources embed one — the OpenAPI `simple-user`
/// schema, trimmed to the one field consumers read.
#[derive(Clone, Debug, Deserialize)]
pub struct Actor {
    /// `login` — required on `simple-user`.
    pub login: String,
}

/// `GET /repos/{owner}/{repo}` — the OpenAPI `repository` schema, trimmed
/// to the fields the git data reads need.
#[derive(Clone, Debug, Deserialize)]
pub struct Repository {
    /// `default_branch` — required on `repository`.
    pub default_branch: String,
    /// `node_id` — required; the repository's global node id, which the
    /// `updateRefs` mutation takes as `repositoryId`.
    pub node_id: String,
    /// `allow_auto_merge` — whether native auto-merge is on; absent reads
    /// as off, the `as_bool() == Some(true)` the indexing before this did.
    pub allow_auto_merge: Option<bool>,
    /// `allow_squash_merge` — same.
    pub allow_squash_merge: Option<bool>,
    /// `allow_merge_commit` — same.
    pub allow_merge_commit: Option<bool>,
    /// `allow_rebase_merge` — same.
    pub allow_rebase_merge: Option<bool>,
    /// `delete_branch_on_merge` — same.
    pub delete_branch_on_merge: Option<bool>,
}

/// `GET /repos/{owner}/{repo}/git/ref/{ref}` — the OpenAPI `git-ref` schema.
#[derive(Clone, Debug, Deserialize)]
pub struct GitRef {
    /// `object` — required; the object the ref points at.
    pub object: GitRefObject,
}
/// `git-ref.object` — the ref's target.
#[derive(Clone, Debug, Deserialize)]
pub struct GitRefObject {
    /// `sha` — required; the target's object ID.
    pub sha: String,
}

/// `GET` and `POST /repos/{owner}/{repo}/git/commits` — the OpenAPI
/// `git-commit` schema.
#[derive(Clone, Debug, Deserialize)]
pub struct GitCommit {
    /// `sha` — required.
    pub sha: String,
    /// `message` — required.
    pub message: String,
    /// `tree` — required; the commit's root tree.
    pub tree: CommitTree,
    /// `verification` — required on `git-commit`.
    pub verification: Verification,
}

/// `git-commit.tree` and `commit.commit.tree` — a tree by object ID.
#[derive(Clone, Debug, Deserialize)]
pub struct CommitTree {
    /// `sha` — required on both.
    pub sha: String,
}

/// A commit signature check — the OpenAPI `verification` schema, trimmed
/// to the one field consumers read.
#[derive(Clone, Debug, Deserialize)]
pub struct Verification {
    /// `verified` — required.
    pub verified: bool,
}

/// `commit.commit` — the content object of a repository `commit`.
#[derive(Clone, Debug, Deserialize)]
pub struct CommitDetail {
    /// `message` — required.
    pub message: String,
    /// `tree` — required.
    pub tree: CommitTree,
    /// `verification` — optional: the `commit` schema's inner object
    /// requires `author`, `committer`, `comment_count`, `message`, `tree`
    /// and `url`, not `verification`.
    pub verification: Option<Verification>,
}

/// A repository commit — the OpenAPI `commit` schema, returned by
/// `GET /repos/{owner}/{repo}/commits/{ref}` and embedded in
/// `commit-comparison` and `branch-with-protection`.
#[derive(Clone, Debug, Deserialize)]
pub struct Commit {
    /// `sha` — required.
    pub sha: String,
    /// `commit` — required; the commit's content object.
    pub commit: CommitDetail,
    /// `author` — nullable in the `commit` schema, and `oneOf` a user or an
    /// empty object (a deleted account): both read as `None`, since the git
    /// author need not map to a GitHub account.
    #[serde(deserialize_with = "account")]
    pub author: Option<Actor>,
    /// `committer` — the same shape and reading as `author`.
    #[serde(deserialize_with = "account")]
    pub committer: Option<Actor>,
}

/// `GET /repos/{owner}/{repo}/compare/{base}...{head}` — the OpenAPI
/// `commit-comparison` schema.
#[derive(Clone, Debug, Deserialize)]
pub struct Comparison {
    /// `total_commits` — required; `commits` can hold fewer.
    pub total_commits: u64,
    /// `commits` — required; `commit` schema elements.
    pub commits: Vec<Commit>,
}

/// `GET /repos/{owner}/{repo}/git/trees/{sha}` — the OpenAPI `git-tree`
/// schema.
#[derive(Clone, Debug, Deserialize)]
pub struct Tree {
    /// `tree` — required; the entries.
    pub tree: Vec<TreeEntry>,
    /// `truncated` — required; true means the listing is incomplete.
    pub truncated: bool,
}

/// One `git-tree.tree[]` element; the schema requires `path`, `mode`,
/// `type` and `sha`.
#[derive(Clone, Debug, Deserialize)]
pub struct TreeEntry {
    /// `path` — required.
    pub path: String,
    /// `mode` — required (for example `100644`).
    pub mode: String,
    /// `type` — required (`blob`, `tree` or `commit` for a submodule).
    #[serde(rename = "type")]
    pub kind: String,
    /// `sha` — required.
    pub sha: String,
}

/// A branch's name and head — one `GET /repos/{owner}/{repo}/branches`
/// element (the `short-branch` schema) or a
/// `GET /repos/{owner}/{repo}/branches/{branch}` response
/// (`branch-with-protection`); both carry `name` and `commit.sha`.
#[derive(Clone, Debug, Deserialize)]
pub struct BranchHead {
    /// `name` — required on both endpoint shapes.
    pub name: String,
    /// `commit` — required on both endpoint shapes.
    pub commit: BranchHeadCommit,
}
/// `BranchHead.commit`: `sha` in both endpoint shapes, plus the single-
/// branch read's commit content.
#[derive(Clone, Debug, Deserialize)]
pub struct BranchHeadCommit {
    /// `sha` — required in both `short-branch.commit` and the `commit`
    /// schema.
    pub sha: String,
    /// `commit` — the `commit` schema's content object, carried only by the
    /// single-branch read; a `branches` list element's `commit` holds just
    /// `sha` and `url`.
    pub commit: Option<CommitDetail>,
}

/// `GET`/`POST`/`PATCH /repos/{owner}/{repo}/pulls[/{n}]` — the OpenAPI
/// `pull-request` schema, trimmed to the fields the lifecycle, receipts,
/// dashboard, batch and server read. The GraphQL inventory projection of a
/// PR also fills one; where the schema requires a field the projection
/// cannot name, the field stays `Option` and its doc says so.
#[derive(Clone, Debug, Deserialize)]
pub struct PullRequest {
    /// `node_id` — required; the auto-merge mutations address a PR by it.
    pub node_id: String,
    /// `number` — required.
    pub number: u64,
    /// `html_url` — required.
    pub html_url: String,
    /// `state` — required; the schema enumerates `open` and `closed`. A
    /// string, not an enum: an unfamiliar value reads as neither, the same
    /// failure direction the raw comparisons had.
    pub state: String,
    /// `title` — required.
    pub title: String,
    /// `body` — `string`, nullable.
    pub body: Option<String>,
    /// `user` — `simple-user`, required by the schema. `Option` because
    /// the inventory projection's `author` may be absent and GitHub can
    /// answer `null` for a deleted account: either reads as unowned. A
    /// malformed object fails the PR's decode.
    #[serde(deserialize_with = "account")]
    pub user: Option<Actor>,
    /// `draft` — `boolean`, optional in the schema.
    pub draft: Option<bool>,
    /// `labels` — required array of `label`; the lifecycle reads `name`.
    pub labels: Vec<Label>,
    /// `merged_at` — `date-time`, nullable; `None` while open or closed
    /// unmerged. Verbatim: readers only ask presence or parse it themselves.
    pub merged_at: Option<String>,
    /// `auto_merge` — `auto-merge` object or `null`; readers only ask
    /// presence.
    pub auto_merge: Option<AutoMerge>,
    /// `mergeable` — `boolean`, nullable while GitHub computes it.
    pub mergeable: Option<bool>,
    /// `mergeable_state` — required `string` in the schema, but GitHub
    /// answers `null` while mergeability is computed.
    pub mergeable_state: Option<String>,
    /// `created_at` — required `date-time` in the schema; `Option` because
    /// the inventory projection cannot name it.
    pub created_at: Option<String>,
    /// `updated_at` — required `date-time` in the schema; `Option` for the
    /// same reason as `created_at`.
    pub updated_at: Option<String>,
    /// `head` — required; the branch and repository the PR comes from.
    pub head: PullRequestHead,
    /// `base` — required; the branch and repository the PR targets.
    pub base: PullRequestBase,
}
/// `pull-request.head` — the schema requires `label`, `ref`, `repo`,
/// `sha` and `user`; only `ref`, `sha` and `repo.full_name` are read.
#[derive(Clone, Debug, Deserialize)]
pub struct PullRequestHead {
    /// `ref` — required; the head branch name.
    #[serde(rename = "ref")]
    pub branch: String,
    /// `sha` — required; the head commit.
    pub sha: String,
    /// `repo` — `repository`, required by the schema, but GitHub answers
    /// `null` when the head repository was deleted: absent reads as a
    /// different repository, never as ours.
    pub repo: Option<PullRequestRepository>,
}
/// `pull-request.base` — same shape as [`PullRequestHead`] minus `sha`,
/// which no consumer reads.
#[derive(Clone, Debug, Deserialize)]
pub struct PullRequestBase {
    /// `ref` — required; the base branch name.
    #[serde(rename = "ref")]
    pub branch: String,
    /// `repo` — `repository`, required by the schema; `Option` because the
    /// inventory projection marks the base repository nullable too.
    pub repo: Option<PullRequestRepository>,
}
/// The `repository` a pull-request head or base carries — the lifecycle
/// reads `full_name` (required) to pin the PR to its repository.
#[derive(Clone, Debug, Deserialize)]
pub struct PullRequestRepository {
    /// `full_name` — required on `repository`: `owner/name`.
    pub full_name: String,
}
/// A `label` on a pull request — the schema requires `name`, the only
/// field readers ask for.
#[derive(Clone, Debug, Deserialize)]
pub struct Label {
    /// `name` — required.
    pub name: String,
}
/// The `auto-merge` object on a pull request — readers ask only whether
/// it is present, so the type holds nothing: any object decodes, a
/// non-object fails the PR's decode.
#[derive(Clone, Debug, Deserialize)]
pub struct AutoMerge {}
/// `PUT /repos/{owner}/{repo}/pulls/{n}/merge` — the merge response.
#[derive(Clone, Debug, Deserialize)]
pub struct MergeResult {
    /// `merged` — required.
    pub merged: bool,
}
/// `POST`/`PATCH /repos/{owner}/{repo}/issues[/{n}]` — the OpenAPI `issue`
/// schema, trimmed to the one field the dashboard reads.
#[derive(Clone, Debug, Deserialize)]
pub struct Issue {
    /// `number` — required.
    pub number: u64,
}

/// One page of a list endpoint or connection: the decoded `items` and
/// whether the set is `complete`. An incomplete page is data, not an
/// error — callers decide what it blocks. An element that fails stays out
/// of `items`; `complete` is what records it.
#[derive(Clone, Debug)]
pub struct Page<T> {
    /// The elements that decoded.
    pub items: Vec<T>,
    /// False when part of the set could not be read: an element that
    /// failed to decode, or a page never fetched.
    pub complete: bool,
}
impl<T: DeserializeOwned> Page<T> {
    /// Decode each element of `values` separately: one that fails marks the
    /// page incomplete and stays out of `items` — the failure is recorded,
    /// never silently dropped the way `filter_map` would drop it, and the
    /// rest of the set is never dropped with it.
    pub fn decode(values: Vec<Value>) -> Self {
        let mut items = Vec::with_capacity(values.len());
        let mut complete = true;
        for value in values {
            match serde_json::from_value(value) {
                Ok(item) => items.push(item),
                Err(_) => complete = false,
            }
        }
        Self { items, complete }
    }
}

/// A `simple-user` that may be `null` or `{}` (a deleted account): either
/// reads as no account.
fn account<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Actor>, D::Error> {
    let value = Option::<serde_json::Map<String, Value>>::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(map) if map.is_empty() => Ok(None),
        Some(map) => Actor::deserialize(Value::Object(map))
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_deleted_account_author_reads_as_no_account() {
        let commit = |author: Value| -> Commit {
            serde_json::from_value(serde_json::json!({
                "sha": "abc",
                "commit": {"tree": {"sha": "t"}, "message": "m"},
                "author": author,
                "committer": null,
            }))
            .unwrap()
        };
        assert!(commit(serde_json::json!({})).author.is_none());
        assert!(commit(Value::Null).author.is_none());
        assert_eq!(
            commit(serde_json::json!({"login": "octocat", "id": 1}))
                .author
                .unwrap()
                .login,
            "octocat"
        );
    }

    #[test]
    fn a_page_decodes_element_wise_and_marks_unreadable_ones_incomplete() {
        let page = Page::<BranchHead>::decode(vec![
            json!({"name":"main","commit":{"sha":"01","protected":true}}),
            // A number where `name` wants a string: unreadable, marks the
            // page incomplete.
            json!({"name":12,"commit":{"sha":"02"}}),
            json!({"name":"next","commit":{"sha":"03","commit":{"message":"m","tree":{"sha":"04"}}}}),
        ]);
        assert!(!page.complete);
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].name, "main");
        assert_eq!(page.items[0].commit.sha, "01");
        assert!(page.items[0].commit.commit.is_none());
        assert_eq!(page.items[1].commit.sha, "03");
        assert_eq!(
            page.items[1]
                .commit
                .commit
                .as_ref()
                .map(|detail| detail.tree.sha.as_str()),
            Some("04")
        );
        assert!(Page::<BranchHead>::decode(vec![]).complete);
        assert!(
            Page::<BranchHead>::decode(vec![json!({"name":"ok","commit":{"sha":"05"}})]).complete
        );
    }
}
