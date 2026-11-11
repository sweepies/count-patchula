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
