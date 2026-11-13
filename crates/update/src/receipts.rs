//! Receipts are proof of previously verified writes, never proof from a marker alone.
use crate::error::Error;
use crate::{Lifecycle, Plan};
use patchula_cache::{Cache, RepositoryScope, State};
use patchula_config::{Config, Rebase};
use patchula_github::{Client, Error as GithubError, model};
use patchula_manager::hash;
use patchula_report::{BranchStatus, Repository};
use patchula_words::Recheck;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[allow(dead_code)]
mod inventory {
    use graphql_client::GraphQLQuery;

    /// Scalars the generated types resolve as `super::*`.
    type DateTime = String;
    type GitObjectID = String;
    /// graphql_client resolves the `URI` scalar as `super::URI`.
    #[allow(clippy::upper_case_acronyms)]
    type URI = String;

    /// `inventory.graphql`, checked against the vendored GitHub schema at
    /// build time. The generated `inventory_query` module carries the
    /// response types and schema enums the consumers share.
    #[derive(GraphQLQuery)]
    #[graphql(
        schema_path = "../github/graphql/schema.docs.graphql",
        query_path = "src/inventory.graphql",
        response_derives = "Debug, PartialEq"
    )]
    pub struct InventoryQuery;

    /// `inventory-next.graphql` — a continuation page carrying every
    /// connection that still has a page, with the `viewer` and
    /// default-branch head the per-page checks read.
    #[derive(GraphQLQuery)]
    #[graphql(
        schema_path = "../github/graphql/schema.docs.graphql",
        query_path = "src/inventory-next.graphql",
        response_derives = "Debug, PartialEq"
    )]
    pub struct InventoryNextQuery;

    /// `inventory-refs.graphql` — a `refs` continuation page, with the
    /// same checks.
    #[derive(GraphQLQuery)]
    #[graphql(
        schema_path = "../github/graphql/schema.docs.graphql",
        query_path = "src/inventory-refs.graphql",
        response_derives = "Debug, PartialEq"
    )]
    pub struct InventoryRefsQuery;

    /// `inventory-issues.graphql` — an `issues` continuation page, with
    /// the same checks.
    #[derive(GraphQLQuery)]
    #[graphql(
        schema_path = "../github/graphql/schema.docs.graphql",
        query_path = "src/inventory-issues.graphql",
        response_derives = "Debug, PartialEq"
    )]
    pub struct InventoryIssuesQuery;

    /// `inventory-last.graphql` — the closing re-read of the fields the
    /// first page answers once: the split walk gathers them from its first
    /// page, so a walk that paged asks them again and the last answer
    /// wins, as it did when every page carried them.
    #[derive(GraphQLQuery)]
    #[graphql(
        schema_path = "../github/graphql/schema.docs.graphql",
        query_path = "src/inventory-last.graphql",
        response_derives = "Debug, PartialEq"
    )]
    pub struct InventoryLastQuery;
}

pub use inventory::inventory_query::{
    InventoryQueryRepositoryDefaultBranchRefRulesNodesParameters as RuleParameters, IssueState,
    PullRequestState, RepositoryRuleType,
};

/// The first-page inventory document, exported for tests that compare
/// request bodies.
pub const INVENTORY_QUERY: &str = inventory::inventory_query::QUERY;
/// The continuation document for the pages both connections still owe.
pub const INVENTORY_NEXT_QUERY: &str = inventory::inventory_next_query::QUERY;
/// The `refs`-only continuation document.
pub const INVENTORY_REFS_QUERY: &str = inventory::inventory_refs_query::QUERY;
/// The `issues`-only continuation document.
pub const INVENTORY_ISSUES_QUERY: &str = inventory::inventory_issues_query::QUERY;
/// The closing document a paged walk ends on.
pub const INVENTORY_LAST_QUERY: &str = inventory::inventory_last_query::QUERY;

/// `Connection.pageInfo` — `hasNextPage` is `Boolean!`, `endCursor` a
/// nullable `String` (schema: `PageInfo`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}
/// A connection envelope as the queries read it: `nodes` may be absent or
/// null even where the schema allows a list — an absent list proves
/// nothing, so the node array stays raw until [`Page`] reads it
/// element-wise.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Connection {
    nodes: Option<Vec<Option<Value>>>,
    page_info: Option<PageInfo>,
}

/// A connection read element by element. `items` holds every node in
/// order — `None` for null nodes (legal in `nodes` lists) and for elements
/// that failed to decode; `complete` distinguishes them: it is `true` only
/// when every element decoded and the connection did not report a further
/// page. Consumers must not trust an incomplete page.
#[derive(Debug)]
pub struct Page<T> {
    /// Elements in order; `None` is a null node or an unreadable one.
    pub items: Vec<Option<T>>,
    /// Every element decoded and no further page was reported.
    pub complete: bool,
}
impl<T> Default for Page<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            complete: false,
        }
    }
}
impl<T: DeserializeOwned> Page<T> {
    /// A nested connection (`labels`, `associatedPullRequests`, `rules`):
    /// `nodes` absent or null is unreadable, not empty, and `pageInfo` must
    /// explicitly report `hasNextPage: false`.
    fn connection(connection: Option<Connection>) -> Self {
        let Some(connection) = connection else {
            return Self::default();
        };
        let mut complete = connection
            .page_info
            .as_ref()
            .is_some_and(|p| !p.has_next_page);
        let items = match connection.nodes {
            Some(nodes) => Self::elements(&nodes, &mut complete, true),
            None => {
                complete = false;
                Vec::new()
            }
        };
        Self { items, complete }
    }
    /// A plain node list — top-level pagination is the caller's, so
    /// `complete` records only that every element decoded. `strict` marks a
    /// null element incomplete too: `nodes(ids:)` legitimately answers null
    /// for a deleted node, but a null `refs`/`issues` element hides data
    /// the walk cannot name.
    fn nodes(nodes: &[Option<Value>], strict: bool) -> Self {
        let mut complete = true;
        let items = Self::elements(nodes, &mut complete, strict);
        Self { items, complete }
    }
    /// A fully-populated page, for tests that construct one by hand.
    pub fn of(items: Vec<T>) -> Self {
        Self {
            items: items.into_iter().map(Some).collect(),
            complete: true,
        }
    }
    fn elements(nodes: &[Option<Value>], complete: &mut bool, strict: bool) -> Vec<Option<T>> {
        nodes
            .iter()
            .map(|node| {
                let Some(node) = node else {
                    if strict {
                        *complete = false;
                    }
                    return None;
                };
                match serde_json::from_value::<T>(node.clone()) {
                    Ok(decoded) => Some(decoded),
                    Err(_) => {
                        *complete = false;
                        None
                    }
                }
            })
            .collect()
    }
}
impl<'de, T: DeserializeOwned> Deserialize<'de> for Page<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::connection(Some(Connection::deserialize(
            deserializer,
        )?)))
    }
}

/// The inventory response's `data`: the envelope decodes strictly —
/// `Query.viewer` is `User!`, `Query.repository` a nullable `Repository`,
/// `Query.nodes` a non-null list — while every connection inside stays raw
/// until [`Page`] reads it element-wise.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InventoryData {
    /// `Query.viewer` — `User!`.
    viewer: Viewer,
    /// `Query.nodes` — `[Node]!`, decoded element-wise into `closed`.
    nodes: Option<Vec<Option<Value>>>,
    /// `Query.repository` — `Repository`, nullable.
    repository: Option<InventoryRepository>,
}
/// `Query.viewer` — `User`, the run's one actor.
#[derive(Debug, Deserialize)]
struct Viewer {
    /// `User.login` — `String!`.
    login: String,
}
/// A continuation document's `data`: `viewer` for the per-page actor
/// check, the default branch's name and head OID for the race check, and
/// the one connection the page continues.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContinuationData {
    /// `Query.viewer` — `User!`.
    viewer: Viewer,
    /// `Query.repository` — `Repository`, nullable.
    repository: Option<ContinuationRepository>,
}
/// `repository` as the continuations read it: the default branch for the
/// race check plus the paged connections the request carried. A document
/// asks only for the connections it continues, so fields default when the
/// page omits them — a continuation that reads them then finds no nodes
/// and fails closed.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ContinuationRepository {
    /// `Repository.defaultBranchRef` — `Ref`, nullable.
    default_branch_ref: Option<DefaultBranchRef>,
    /// `Repository.refs` — `RefConnection`, nullable.
    refs: Option<Connection>,
    /// `Repository.issues` — `IssueConnection!`.
    issues: Connection,
}
/// `repository`'s scalar fields (`ID!`/`Boolean!`); its connections stay
/// [`Connection`]s until each list is read.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InventoryRepository {
    /// `Repository.id` — `ID!`.
    pub id: String,
    /// `Repository.autoMergeAllowed` — `Boolean!`.
    pub auto_merge_allowed: bool,
    /// `Repository.squashMergeAllowed` — `Boolean!`.
    pub squash_merge_allowed: bool,
    /// `Repository.mergeCommitAllowed` — `Boolean!`.
    pub merge_commit_allowed: bool,
    /// `Repository.rebaseMergeAllowed` — `Boolean!`.
    pub rebase_merge_allowed: bool,
    /// `Repository.deleteBranchOnMerge` — `Boolean!`.
    pub delete_branch_on_merge: bool,
    /// `Repository.defaultBranchRef` — `Ref`, nullable.
    pub default_branch_ref: Option<DefaultBranchRef>,
    /// `Repository.issues` — `IssueConnection!`; documents that do not ask
    /// for it (the closing read) decode to an empty, unreadable connection.
    #[serde(default)]
    pub issues: Connection,
    /// `Repository.refs` — `RefConnection`, nullable.
    pub refs: Option<Connection>,
}
/// `defaultBranchRef` — the `Ref` fields tooling evidence reads.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DefaultBranchRef {
    /// `Ref.name` — `String!`.
    name: String,
    /// `Ref.target` — `GitObject`, nullable; `oid` is on the interface.
    target: Option<DefaultTarget>,
    /// `Ref.rules` — `RepositoryRuleConnection`, nullable.
    rules: Option<Page<Rule>>,
    /// `Ref.refUpdateRule` — `RefUpdateRule`, nullable.
    ref_update_rule: Option<RefUpdateRule>,
}
/// `defaultBranchRef.target` — `oid` lives on the `GitObject` interface, so
/// any target type carries it; `history` exists only on `Commit`.
#[derive(Debug, Deserialize)]
struct DefaultTarget {
    /// `GitObject.oid` — `GitObjectID!`.
    oid: String,
    /// `Commit.history` — `CommitHistoryConnection!` on commits, absent on
    /// other target types.
    history: Option<Connection>,
}
/// `rules.nodes` element — `RepositoryRule`: `type` is required and
/// `parameters` a nullable `RuleParameters` union.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    /// `RepositoryRule.type` — `RepositoryRuleType!`.
    #[serde(rename = "type")]
    pub rule_type: RepositoryRuleType,
    /// `RepositoryRule.parameters` — `RuleParameters`, nullable.
    pub parameters: Option<RuleParameters>,
}
/// `refUpdateRule` — `requiredStatusCheckContexts` is a nullable list of
/// nullable strings, `requiredApprovingReviewCount` a nullable `Int`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefUpdateRule {
    pub required_status_check_contexts: Option<Vec<Option<String>>>,
    pub required_approving_review_count: Option<i64>,
}
/// `issues.nodes` element — `Issue`; `author` is a nullable `Actor`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    /// `Issue.id` — `ID!`.
    pub id: String,
    /// `Issue.number` — `Int!`.
    pub number: i64,
    /// `Issue.title` — `String!`.
    pub title: String,
    /// `Issue.body` — `String!`.
    pub body: String,
    /// `Issue.state` — `IssueState!`.
    pub state: IssueState,
    /// `Issue.author` — `Actor`, nullable.
    pub author: Option<Actor>,
}
/// `nodes(ids:)` element — `PullRequest` on the `Node` interface; any
/// other node type is `Other`.
#[derive(Debug, Deserialize)]
#[serde(tag = "__typename")]
pub enum ClosedNode {
    PullRequest(Box<ClosedPullRequest>),
    #[serde(other)]
    Other,
}
/// The `PullRequest` fragment's fields — all non-null except the nullable
/// `author`, `headRepository` and `baseRepository`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClosedPullRequest {
    /// `PullRequest.id` — `ID!`.
    pub id: String,
    /// `PullRequest.number` — `Int!`.
    pub number: i64,
    /// `PullRequest.state` — `PullRequestState!`.
    pub state: PullRequestState,
    /// `PullRequest.merged` — `Boolean!`.
    pub merged: bool,
    /// `PullRequest.body` — `String!`.
    pub body: String,
    /// `PullRequest.headRefName` — `String!`.
    pub head_ref_name: String,
    /// `PullRequest.author` — `Actor`, nullable.
    pub author: Option<Actor>,
    /// `PullRequest.headRepository` — `Repository`, nullable.
    pub head_repository: Option<NamedRepository>,
    /// `PullRequest.baseRepository` — `Repository`, nullable.
    pub base_repository: Option<NamedRepository>,
}
/// `refs.nodes` element — `Ref`: `name` and `associatedPullRequests` are
/// required; `target` is the nullable `GitObject` interface.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryRef {
    /// `Ref.name` — `String!`.
    pub name: String,
    /// `Ref.target` — `GitObject`, nullable.
    pub target: Option<RefTarget>,
    /// `Ref.associatedPullRequests` — `PullRequestConnection!`.
    pub associated_pull_requests: Page<InventoryPr>,
}
/// `Ref.target` — the query reads `Commit` fields; every other `GitObject`
/// type is `Other`.
#[derive(Debug, Deserialize)]
#[serde(tag = "__typename")]
pub enum RefTarget {
    Commit(RefCommit),
    #[serde(other)]
    Other,
}
/// The `Commit` fragment on a ref target.
#[derive(Debug, Deserialize)]
pub struct RefCommit {
    /// `Commit.oid` — `GitObjectID!`.
    pub oid: String,
    /// `Commit.message` — `String!`.
    pub message: String,
    /// `Commit.tree` — `Tree!`.
    pub tree: CommitTree,
    /// `Commit.author` — `GitActor`, nullable.
    pub author: Option<GitActor>,
    /// `Commit.committer` — `GitActor`, nullable.
    pub committer: Option<GitActor>,
    /// `Commit.signature` — `GitSignature`, nullable.
    pub signature: Option<Signature>,
}
/// `GitActor.user` — `User`, nullable.
#[derive(Debug, Deserialize)]
pub struct GitActor {
    pub user: Option<Login>,
}
/// `User.login` — `String!`.
#[derive(Debug, Deserialize)]
pub struct Login {
    pub login: String,
}
/// `Tree.oid` — `GitObjectID!`.
#[derive(Debug, Deserialize)]
pub struct CommitTree {
    pub oid: String,
}
/// `GitSignature.isValid` — `Boolean!`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Signature {
    pub is_valid: bool,
}
/// `associatedPullRequests.nodes` element — the `PullRequest` fields the
/// write path needs, all non-null unless the schema marks them nullable
/// (`author`, the repositories, `labels`, `autoMergeRequest`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryPr {
    /// `PullRequest.number` — `Int!`.
    pub number: i64,
    /// `PullRequest.id` — `ID!`.
    pub id: String,
    /// `PullRequest.url` — `URI!`.
    pub url: String,
    /// `PullRequest.title` — `String!`.
    pub title: String,
    /// `PullRequest.body` — `String!`.
    pub body: String,
    /// `PullRequest.headRefName` — `String!`.
    pub head_ref_name: String,
    /// `PullRequest.headRefOid` — `GitObjectID!`.
    pub head_ref_oid: String,
    /// `PullRequest.baseRefName` — `String!`.
    pub base_ref_name: String,
    /// `PullRequest.author` — `Actor`, nullable.
    pub author: Option<Actor>,
    /// `PullRequest.headRepository` — `Repository`, nullable.
    pub head_repository: Option<NamedRepository>,
    /// `PullRequest.baseRepository` — `Repository`, nullable.
    pub base_repository: Option<NamedRepository>,
    /// `PullRequest.labels` — `LabelConnection`, nullable.
    pub labels: Option<Page<Label>>,
    /// `PullRequest.autoMergeRequest` — `AutoMergeRequest`, nullable.
    pub auto_merge_request: Option<AutoMergeRequest>,
}
/// `Actor` — `__typename` separates Bots, whose REST shape carries a
/// `[bot]` suffix the GraphQL login omits; `login` is `String!`.
#[derive(Debug, Deserialize)]
pub struct Actor {
    #[serde(rename = "__typename")]
    pub typename: String,
    pub login: String,
}
/// `Repository.nameWithOwner` — `String!`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NamedRepository {
    pub name_with_owner: String,
}
/// `Label.name` — `String!`.
#[derive(Debug, Deserialize)]
pub struct Label {
    pub name: String,
}
/// `AutoMergeRequest.enabledAt` — `DateTime`, nullable.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoMergeRequest {
    pub enabled_at: Option<String>,
}
/// `history.nodes` element — `Commit.statusCheckRollup` is nullable; the
/// `state` inside stays verbatim since only its presence matters.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DefaultCommit {
    pub status_check_rollup: Option<Value>,
}
impl DefaultCommit {
    /// Whether the commit reported a status check rollup at all.
    pub fn rollup(&self) -> bool {
        self.status_check_rollup.is_some()
    }
}

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
/// The `viewer.login` an inventory's first page reported, remembered per
/// credential as soon as it is known — a walk that then fails still leaves
/// it for the next read, which asks `filterBy.createdBy` for it. The
/// answer's `viewer.login` proves or disproves the hint, so a wrong entry
/// only costs one request and never narrows the set wrongly.
#[derive(Serialize, Deserialize)]
struct ActorHint(String);
/// `ActorHint`'s scope: one entry per credential scope.
#[derive(Serialize)]
struct ActorScope {
    credential_scope: String,
}
impl State for ActorHint {
    const NAMESPACE: &'static str = "inventory-actor";
    const VERSION: u32 = 1;
    type Scope = ActorScope;
}
/// The prefix the repository's most recent write receipts were saved
/// under, and when. Receipts are scoped by repository and prefix both, so
/// a `prs.branch` flip away and back leaves an older prefix's entries in
/// place; they prove nothing about the branches written under the other
/// prefix since, and only this marker naming `config.prs.branch` lets the
/// inventory's refs walk narrow to the prefix.
#[derive(Serialize, Deserialize)]
struct ReceiptsPrefix {
    prefix: String,
    saved: i64,
}
impl State for ReceiptsPrefix {
    const NAMESPACE: &'static str = "receipts-prefix";
    const VERSION: u32 = 1;
    type Scope = RepositoryScope;
}
fn receipts_scope(repo: &str, prefix: &str) -> ReceiptsScope {
    ReceiptsScope {
        repository: repo.into(),
        branch_prefix: prefix.into(),
    }
}
/// The bundled schema's `prs.branch`: the prefix a repository whose
/// config is not yet known most likely uses.
fn default_branch_prefix() -> &'static str {
    static PREFIX: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        patchula_config::defaults()
            .expect("bundled config")
            .prs
            .branch
    });
    &PREFIX
}
/// Record that the receipts under `receipts_scope(repo, prefix)` are the
/// repository's most recent: only while the marker names `prs.branch` may
/// the next read narrow the refs walk to the prefix.
async fn mark_prefix(cache: &Cache, repository: &str, prefix: &str) -> Result<(), Error> {
    cache
        .set_state(
            &RepositoryScope {
                repository: repository.into(),
            },
            &ReceiptsPrefix {
                prefix: prefix.into(),
                saved: chrono::Utc::now().timestamp(),
            },
        )
        .await
        .map_err(Error::Cache)
}
/// `server` is the advertised URL, which the PR body carries. Only a server
/// joins the hash, so receipts written without one stay valid.
fn intent(plan: &Plan, config: &Config, recheck: Recheck, server: Option<&str>) -> String {
    let inputs = (
        "release-notes-v4",
        plan.identity(),
        plan.input_fingerprint(),
        plan.changes(),
        config,
        crate::pull_requests::recheck_hashed(recheck),
    );
    // Tooling warnings join only when present, so other receipts stay valid.
    hash(
        match (server, plan.callouts().is_empty()) {
            (None, true) => serde_json::to_string(&inputs),
            (Some(url), true) => serde_json::to_string(&(inputs, url)),
            (server, false) => serde_json::to_string(&(inputs, server, plan.callouts())),
        }
        .expect("serializable intent")
        .as_bytes(),
    )
}
/// `Receipt.pr` — the frozen comparison state. The scalars that pass
/// through verbatim stay `Value` so the serialization matches what the
/// write path stored, whatever a REST object happened to hold; the computed
/// fields are typed.
#[derive(Serialize)]
struct PrState {
    number: Value,
    title: Value,
    body_hash: String,
    head: PrStateHead,
    base: PrStateBase,
    actor: Value,
    labels: BTreeSet<String>,
    merge: bool,
}
/// `PrState.head` — `{ref, sha, repo}`.
#[derive(Serialize)]
struct PrStateHead {
    #[serde(rename = "ref")]
    r#ref: Value,
    sha: Value,
    repo: Value,
}
/// `PrState.base` — `{ref, repo}`.
#[derive(Serialize)]
struct PrStateBase {
    #[serde(rename = "ref")]
    r#ref: Value,
    repo: Value,
}
impl PrState {
    /// From the REST view the write lifecycle stores — every scalar the
    /// receipt compares passes through verbatim, as before.
    fn from_rest(pr: &model::PullRequest) -> Self {
        Self {
            number: json!(pr.number),
            title: json!(pr.title),
            body_hash: crate::pr::body::body_hash(crate::pull_requests::body(pr)),
            head: PrStateHead {
                r#ref: json!(pr.head.branch),
                sha: json!(pr.head.sha),
                repo: pr
                    .head
                    .repo
                    .as_ref()
                    .map_or(Value::Null, |r| json!(r.full_name)),
            },
            base: PrStateBase {
                r#ref: json!(pr.base.branch),
                repo: pr
                    .base
                    .repo
                    .as_ref()
                    .map_or(Value::Null, |r| json!(r.full_name)),
            },
            actor: pr.user.as_ref().map_or(Value::Null, |u| json!(u.login)),
            labels: pr.labels.iter().map(|l| l.name.clone()).collect(),
            merge: pr.auto_merge.is_some(),
        }
    }
    /// From an inventory `PullRequest` — the same fields, read typed.
    fn from_inventory(pr: &InventoryPr) -> Self {
        Self {
            number: json!(pr.number),
            title: json!(pr.title),
            body_hash: crate::pr::body::body_hash(&pr.body),
            head: PrStateHead {
                r#ref: json!(pr.head_ref_name),
                sha: json!(pr.head_ref_oid),
                repo: pr
                    .head_repository
                    .as_ref()
                    .map_or(Value::Null, |r| json!(r.name_with_owner)),
            },
            base: PrStateBase {
                r#ref: json!(pr.base_ref_name),
                repo: pr
                    .base_repository
                    .as_ref()
                    .map_or(Value::Null, |r| json!(r.name_with_owner)),
            },
            actor: pr
                .author
                .as_ref()
                .map_or(Value::Null, |a| json!(actor_login(a))),
            labels: pr.labels.as_ref().map_or_else(BTreeSet::new, |page| {
                page.items
                    .iter()
                    .flatten()
                    .map(|l| l.name.clone())
                    .collect()
            }),
            merge: pr.auto_merge_request.is_some(),
        }
    }
    fn value(&self) -> Value {
        serde_json::to_value(self).expect("serializable PR state")
    }
}
/// The REST-style login for a GraphQL `Actor`: `Actor.__typename == "Bot"`
/// carries REST's `[bot]` suffix the GraphQL login omits — stripping or
/// appending suffixes for arbitrary Users would conflate identities.
pub(crate) fn actor_login(actor: &Actor) -> String {
    if actor.typename == "Bot" && !actor.login.ends_with("[bot]") {
        format!("{}[bot]", actor.login)
    } else {
        actor.login.clone()
    }
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

/// The default branch's head as `inventory.graphql` returns it: the fields
/// the freshness check and tooling evidence read.
#[derive(Debug, Default)]
pub struct DefaultBranch {
    /// `defaultBranchRef.name`.
    pub name: Option<String>,
    /// `defaultBranchRef.target.oid`.
    pub oid: Option<String>,
    /// `target.history` read element-wise; `None` when the response carried
    /// no list, so absent and empty stay distinct.
    pub history: Option<Page<DefaultCommit>>,
    /// `defaultBranchRef.rules` as a [`Page`]; `None` when absent.
    pub rules: Option<Page<Rule>>,
    /// `refUpdateRule`, as returned.
    pub ref_update_rule: Option<RefUpdateRule>,
}
impl DefaultBranch {
    /// Decode the query's `defaultBranchRef` node.
    fn read(head: Option<DefaultBranchRef>) -> Self {
        let Some(head) = head else {
            return Self::default();
        };
        Self {
            name: Some(head.name),
            oid: head.target.as_ref().map(|t| t.oid.clone()),
            history: head
                .target
                .and_then(|t| t.history)
                .and_then(|h| h.nodes)
                .map(|nodes| Page::nodes(&nodes, true)),
            rules: head.rules,
            ref_update_rule: head.ref_update_rule,
        }
    }
}
/// Fresh control and branch state shared by lookup, dashboard and write receipts.
#[derive(Default, Debug)]
pub struct Inventory {
    /// `viewer.login` — the run's one actor.
    pub actor: String,
    /// Every `refs/heads/` ref GitHub reported, read element-wise.
    pub refs: Page<InventoryRef>,
    /// The closed PRs the receipts still reference (`nodes(ids:$closed)`).
    pub closed: Page<ClosedNode>,
    /// The repository's open issues (`states: [OPEN]`).
    pub issues: Page<Issue>,
    /// The repository's merge settings.
    pub merge_settings: crate::automerge::Settings,
    /// The PRs this actor merged last, oldest first — the cached auto-merge
    /// history, not part of the GraphQL response.
    pub merged_prs: Vec<crate::automerge::MergedPr>,
    /// The default branch's head, for the freshness check and tooling.
    pub default_branch: DefaultBranch,
    /// The `refs(query:)` the walk ran with, if it narrowed the branch
    /// list to a prefix: `Some` means `refs` may omit Count branches
    /// whose names fall outside the prefix — a rename GitHub keeps an
    /// open pull request on — so the receipts path must not settle around
    /// a receipted branch it cannot see. `None` walked every branch.
    pub ref_filter: Option<String>,
    /// Branch names whose `associatedPullRequests` or their `labels` were
    /// not fully readable. Their PR view is incomplete, so the receipts
    /// fast path must not settle from this inventory.
    pub unsettled: BTreeSet<String>,
}
impl Inventory {
    /// The open, bot-owned pull requests across all refs — projected onto
    /// the lifecycle's [`Tracked`] (see [`pull_requests::tracked_pr`]).
    ///
    /// [`pull_requests::tracked_pr`]: crate::pull_requests::tracked_pr
    /// [`Tracked`]: crate::pull_requests::Tracked
    pub fn pull_requests(&self, repo: &str) -> Vec<crate::pull_requests::Tracked> {
        self.refs
            .items
            .iter()
            .flatten()
            .flat_map(|r| r.associated_pull_requests.items.iter().flatten())
            .filter_map(crate::pull_requests::tracked_pr)
            .filter(|p| crate::pull_requests::owned_pr(&p.pr, &self.actor, repo))
            .collect()
    }
    pub async fn read(
        client: &Client,
        cache: &Cache,
        repository: &Repository,
        config: &Config,
    ) -> Result<Self, Error> {
        Self::read_documents(
            client,
            cache,
            &repository.repository,
            &config.prs.branch,
            Head::Scan {
                name: repository.default_branch.as_deref(),
                oid: repository.commit.as_deref(),
            },
            DOCUMENTS,
        )
        .await
    }
    /// The inventory read beside the scan's head check: no head is known
    /// yet, so the first page's answer anchors the per-page race checks
    /// instead. The caller must compare the result's
    /// [`DefaultBranch::name`] and [`DefaultBranch::oid`], and the
    /// `branch_prefix` it passed, with what the scan found — a difference
    /// means this read answered for another head or another config, and
    /// the inventory must be read again with [`read`](Self::read).
    pub async fn read_beside(
        client: &Client,
        cache: &Cache,
        repository: &str,
        branch_prefix: &str,
    ) -> Result<Self, Error> {
        Self::read_documents(
            client,
            cache,
            repository,
            branch_prefix,
            Head::FirstPage,
            DOCUMENTS,
        )
        .await
    }
    /// The `prs.branch` a read beside the scan may assume before the
    /// repository's config is known: the prefix the repository's most
    /// recent write receipts were saved under, or the bundled schema's
    /// default when no receipts were ever saved. The caller must still
    /// compare it with the scanned config's `prs.branch` before trusting
    /// the read.
    pub async fn assumed_prefix(cache: &Cache, repository: &str) -> Result<String, Error> {
        let remembered = cache
            .state::<ReceiptsPrefix>(&RepositoryScope {
                repository: repository.into(),
            })
            .await?
            .map(|saved| saved.prefix);
        Ok(remembered.unwrap_or_else(|| default_branch_prefix().into()))
    }
    /// A read over `documents`: the credential's remembered actor filters
    /// the issue walk; a missing or disproven hint reads unfiltered instead
    /// and the answer's `viewer.login` is stored for the next read.
    async fn read_documents(
        client: &Client,
        cache: &Cache,
        repository: &str,
        branch_prefix: &str,
        head: Head<'_>,
        documents: Documents,
    ) -> Result<Self, Error> {
        let receipts = cache
            .state::<Receipts>(&receipts_scope(repository, branch_prefix))
            .await?;
        let saved = cache
            .state::<ReceiptsPrefix>(&RepositoryScope {
                repository: repository.into(),
            })
            .await?;
        // Stored receipts prove every branch the Count wrote here shared
        // this prefix — while the marker says they are the repository's
        // most recent. A `prs.branch` flip away and back leaves the older
        // prefix's receipts in place beside newer ones, and they no
        // longer prove the walk may skip every other branch. Without
        // receipts, without the marker, or with a marker naming another
        // prefix — a first run, a cold cache, a changed prefix or a flip
        // back — the walk covers every branch as before: that run is a
        // full write whose cleanup lists every branch itself and whose
        // restore marks the prefix again.
        let ref_query = (receipts.is_some()
            && documents.ref_query
            && saved.is_some_and(|saved| saved.prefix == branch_prefix))
        .then_some(branch_prefix);
        let receipts = receipts.unwrap_or_default();
        let closed_ids: Vec<_> = receipts
            .closed
            .values()
            .map(|r| r.pr["node_id"].clone())
            .take(100)
            .collect();
        let (owner, name) = repository
            .split_once('/')
            .ok_or(Error::Refused("invalid repository".into()))?;
        let scope = ActorScope {
            credential_scope: client.credential_scope().into(),
        };
        let mut hint = cache.state::<ActorHint>(&scope).await?.map(|hint| hint.0);
        for _ in 0..2 {
            match Self::walk(
                client,
                cache,
                &scope,
                head,
                Request {
                    owner,
                    name,
                    closed_ids: &closed_ids,
                    documents: &documents,
                    actor: hint.as_deref(),
                    ref_query,
                },
            )
            .await
            {
                Ok(mut inventory) => {
                    inventory.finish(cache, repository).await?;
                    return Ok(inventory);
                }
                Err(ReadFailure::WrongActor) => hint = None,
                Err(ReadFailure::Error(error)) => return Err(error),
            }
        }
        Err(Error::Refused("inventory actor changed".into()))
    }
    /// The document walk itself: the first page asks for everything, then
    /// each request continues every connection that still has a page — a
    /// finished one is never sent again. `request.actor` is the stored
    /// login hint, if any: the answer's `viewer.login` must confirm it or
    /// the issue set cannot be trusted. `head` says what the per-page
    /// race checks compare the default branch with.
    async fn walk(
        client: &Client,
        cache: &Cache,
        scope: &ActorScope,
        head: Head<'_>,
        request: Request<'_>,
    ) -> Result<Self, ReadFailure> {
        let actor = request.actor;
        // `createdBy` takes the REST login, which is what `viewer.login`
        // reports; a null filter is no filter at all.
        let mut filter = actor.map_or(Value::Null, |login| json!({"createdBy":login}));
        let variables = |cursor: &Value, issue_cursor: &Value, filter: &Value| {
            let mut variables = json!({"owner":request.owner,"name":request.name,
                "prefix":"refs/heads/","closed":request.closed_ids,
                "cursor":cursor,"issueCursor":issue_cursor,
                "issueFilter":filter.clone()});
            // A null variable is no filter; the key stays out entirely
            // when the walk covers every branch.
            if let Some(query) = request.ref_query {
                variables["refQuery"] = json!(query);
            }
            variables
        };
        let data: InventoryData = client
            .graphql(
                request.documents.first,
                variables(&Value::Null, &Value::Null, &filter),
            )
            .await?;
        // `viewer.login` is the credential's proven actor, so store the
        // hint as soon as it is known: a walk that fails below leaves the
        // next read no colder than this one, and a repository near the
        // page budget cannot stay unfiltered forever.
        if actor != Some(data.viewer.login.as_str()) {
            cache
                .set_state(scope, &ActorHint(data.viewer.login.clone()))
                .await
                .map_err(Error::Cache)?;
        }
        if actor.is_some_and(|hint| hint != data.viewer.login) {
            return Err(ReadFailure::WrongActor);
        }
        let mut result = Self {
            actor: data.viewer.login,
            ..Self::default()
        };
        let repository_data = data.repository.unwrap_or_default();
        result.merge_settings = crate::automerge::Settings::from_graphql(&repository_data);
        let branch = DefaultBranch::read(repository_data.default_branch_ref);
        if let Head::Scan { name, oid } = head {
            Self::head_check(name, oid, branch.name.as_deref(), branch.oid.as_deref())?;
        }
        result.default_branch = branch;
        result.closed = Page::nodes(
            &data
                .nodes
                .ok_or(Error::Refused("missing closed inventory".into()))?,
            false,
        );
        let mut refs = Walk::default();
        let mut issues = Walk::default();
        refs.page(repository_data.refs.as_ref())?;
        issues.page(Some(&repository_data.issues))?;
        // A first page read without a hint that left the issues connection
        // unfinished now knows the actor: what remains is its issues, and
        // cursors do not carry across filters, so the walk restarts on
        // `createdBy` and the gathered unfiltered nodes go with it.
        if actor.is_none() && !issues.done {
            filter = json!({"createdBy": result.actor});
            issues = Walk::default();
        }
        let mut pages = 1;
        while !(refs.done && issues.done) {
            pages += 1;
            if pages > client.max_pages() {
                return Err(Error::Refused("inventory page budget exceeded".into()).into());
            }
            let document = if !refs.done && !issues.done {
                request.documents.next
            } else if !refs.done {
                request.documents.refs
            } else {
                request.documents.issues
            };
            let data: ContinuationData = client
                .graphql(document, variables(&refs.next, &issues.next, &filter))
                .await?;
            let repository_data = Self::check(&result, data)?;
            if !refs.done {
                refs.page(repository_data.refs.as_ref())?;
            }
            if !issues.done {
                issues.page(Some(&repository_data.issues))?;
            }
            if refs.nodes.len() + issues.nodes.len() > 100_000 {
                return Err(Error::Refused("inventory page budget exceeded".into()).into());
            }
        }
        // A walk that paged ends on the closing read: the fields only the
        // first page answered — `nodes(ids:$closed)`, the merge settings
        // and the default branch's rules, history and update rule — are
        // re-asked so the newest answer wins, as it did when every page
        // carried them. The same actor and head checks hold it honest.
        if pages > 1
            && let Some(last) = request.documents.last
        {
            let data: InventoryData = client
                .graphql(
                    last,
                    json!({"owner":request.owner,"name":request.name,
                        "closed":request.closed_ids}),
                )
                .await?;
            if result.actor != data.viewer.login {
                return Err(Error::Refused("inventory actor changed".into()).into());
            }
            let repository_data = data.repository.unwrap_or_default();
            result.merge_settings = crate::automerge::Settings::from_graphql(&repository_data);
            let branch = DefaultBranch::read(repository_data.default_branch_ref);
            Self::head_check(
                result.default_branch.name.as_deref(),
                result.default_branch.oid.as_deref(),
                branch.name.as_deref(),
                branch.oid.as_deref(),
            )?;
            result.default_branch = branch;
            result.closed = Page::nodes(
                &data
                    .nodes
                    .ok_or(Error::Refused("missing closed inventory".into()))?,
                false,
            );
        }
        result.refs = Page::nodes(&refs.nodes, true);
        // `refs(query:)` is GitHub's case-insensitive substring match on
        // the ref name — `a/patchula/b` still arrives — while every
        // branch the Count writes starts with the prefix, so only those
        // keep. An undecoded node stays: it could hide an in-prefix
        // branch, and `complete` already counts it.
        if let Some(prefix) = request.ref_query {
            result
                .refs
                .items
                .retain(|item| item.as_ref().is_none_or(|r| r.name.starts_with(prefix)));
            result.ref_filter = Some(prefix.into());
        }
        result.issues = Page::nodes(&issues.nodes, true);
        Ok(result)
    }
    /// The race check every page makes: the default branch's name and head
    /// OID must still be the walk's anchor — the scan's answer, or the
    /// first page's when the read ran beside the scan.
    fn head_check(
        expected_name: Option<&str>,
        expected_oid: Option<&str>,
        name: Option<&str>,
        oid: Option<&str>,
    ) -> Result<(), Error> {
        if name != expected_name || oid != expected_oid {
            return Err(Error::Raced(
                "default branch changed since extraction; rerun",
            ));
        }
        Ok(())
    }
    /// The per-page checks a continuation repeats — the actor must stay the
    /// first page's actor and the default branch must not have moved —
    /// returning the page's repository data.
    fn check(result: &Inventory, data: ContinuationData) -> Result<ContinuationRepository, Error> {
        if result.actor != data.viewer.login {
            return Err(Error::Refused("inventory actor changed".into()));
        }
        let repository_data = data.repository.unwrap_or_default();
        let head = repository_data.default_branch_ref.as_ref();
        Self::head_check(
            result.default_branch.name.as_deref(),
            result.default_branch.oid.as_deref(),
            head.map(|h| h.name.as_str()),
            head.and_then(|h| h.target.as_ref()).map(|t| t.oid.as_str()),
        )?;
        Ok(repository_data)
    }
    /// `merged_prs` and `unsettled`, read once the walk gathered every page.
    async fn finish(&mut self, cache: &Cache, repo: &str) -> Result<(), Error> {
        self.merged_prs = crate::automerge::load_history(cache, repo, &self.actor).await?;
        // Labels and associated pull requests come back only to their
        // first page; anything past it — or unreadable — is invisible
        // here, so the branch cannot settle from receipts.
        for r in self.refs.items.iter().flatten() {
            let prs = &r.associated_pull_requests;
            let partial = !prs.complete
                || prs.items.iter().any(Option::is_none)
                || prs
                    .items
                    .iter()
                    .flatten()
                    .any(|pr| pr.labels.as_ref().is_none_or(|l| !l.complete));
            if partial {
                self.unsettled.insert(r.name.clone());
            }
        }
        Ok(())
    }
}

/// `walk`'s failure: an inventory error, or a login hint the answer's
/// `viewer.login` disproves.
enum ReadFailure {
    Error(Error),
    WrongActor,
}
/// What a walk's per-page head checks compare each page's default branch
/// with.
#[derive(Clone, Copy)]
enum Head<'a> {
    /// The scan's answer — the extraction's default branch name and
    /// commit — so a page reporting another raced the run.
    Scan {
        name: Option<&'a str>,
        oid: Option<&'a str>,
    },
    /// Whatever the first page answers: the read runs beside the scan's
    /// head check, so nothing is known yet. The walk stays self-consistent
    /// and the caller compares the result with the scan's head itself.
    FirstPage,
}
impl From<Error> for ReadFailure {
    fn from(error: Error) -> Self {
        Self::Error(error)
    }
}
impl From<GithubError> for ReadFailure {
    fn from(error: GithubError) -> Self {
        Self::Error(error.into())
    }
}
/// The unchanging arguments of one walk's requests; only `actor` differs
/// between a hinted first try and the unfiltered retry after `WrongActor`.
struct Request<'a> {
    owner: &'a str,
    name: &'a str,
    closed_ids: &'a [Value],
    documents: &'a Documents,
    actor: Option<&'a str>,
    /// The branch prefix `refs(query:)` narrows the walk to when a write
    /// receipt exists for it — `None` sends no `query` argument at all.
    ref_query: Option<&'a str>,
}
/// The documents a read walks: the first page asks everything once; each
/// continuation asks every connection that still has a page. `last` is the
/// closing re-read of the fields only the first page asks, sent when the
/// walk took more than one request so the newest answer wins; `None` when
/// `first` already re-read them on every page (the legacy document).
/// `ref_query` marks the documents as declaring `$refQuery` on `refs`, so
/// a stored receipt may narrow the walk to the branch prefix.
struct Documents {
    first: &'static str,
    next: &'static str,
    refs: &'static str,
    issues: &'static str,
    last: Option<&'static str>,
    ref_query: bool,
}
const DOCUMENTS: Documents = Documents {
    first: INVENTORY_QUERY,
    next: INVENTORY_NEXT_QUERY,
    refs: INVENTORY_REFS_QUERY,
    issues: INVENTORY_ISSUES_QUERY,
    last: Some(INVENTORY_LAST_QUERY),
    ref_query: true,
};

/// The pre-split inventory document, kept so the differential test can
/// build an `Inventory` exactly the way releases before the split did.
#[cfg(feature = "testing")]
pub const LEGACY_INVENTORY_QUERY: &str = include_str!("../tests/fixtures/legacy-inventory.graphql");
/// The differential-test entry: the same walk over the pre-split document,
/// which answers both connections on every page it is sent for.
#[cfg(feature = "testing")]
pub async fn read_legacy_inventory(
    client: &Client,
    cache: &Cache,
    repository: &Repository,
    config: &Config,
) -> Result<Inventory, Error> {
    Inventory::read_documents(
        client,
        cache,
        &repository.repository,
        &config.prs.branch,
        Head::Scan {
            name: repository.default_branch.as_deref(),
            oid: repository.commit.as_deref(),
        },
        Documents {
            first: LEGACY_INVENTORY_QUERY,
            next: LEGACY_INVENTORY_QUERY,
            refs: LEGACY_INVENTORY_QUERY,
            issues: LEGACY_INVENTORY_QUERY,
            last: None,
            ref_query: false,
        },
    )
    .await
}
/// The receipt set a full write leaves behind, emptied: enough state for
/// the read to take the prefix walk without a real write. For tests and
/// benchmarks that seed a steady-state repository.
#[cfg(feature = "testing")]
pub async fn seed_receipts(cache: &Cache, repository: &str, config: &Config) -> Result<(), Error> {
    cache
        .set_state(
            &receipts_scope(repository, &config.prs.branch),
            &Receipts::default(),
        )
        .await
        .map_err(Error::Cache)?;
    mark_prefix(cache, repository, &config.prs.branch).await
}
/// One connection's walk: the last cursor sent, the cursors already seen
/// (a repeat fails rather than looping) and the nodes gathered so far.
#[derive(Default)]
struct Walk {
    done: bool,
    next: Value,
    visited: std::collections::HashSet<String>,
    nodes: Vec<Option<Value>>,
}
impl Walk {
    /// Gather one page's nodes and the cursor for the next request.
    fn page(&mut self, connection: Option<&Connection>) -> Result<(), Error> {
        let (nodes, page_info) = connection
            .and_then(|c| c.nodes.as_ref().zip(c.page_info.as_ref()))
            .ok_or(GithubError::Pagination)?;
        self.nodes.extend(nodes.iter().cloned());
        if page_info.has_next_page {
            let end = page_info
                .end_cursor
                .clone()
                .filter(|c| !c.is_empty())
                .ok_or(Error::Refused("missing inventory cursor".into()))?;
            if !self.visited.insert(end.clone()) {
                return Err(Error::Refused("repeated inventory cursor".into()));
            }
            self.next = json!(end);
        } else {
            self.done = true;
        }
        Ok(())
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
) -> Result<RestoreOutcome, Error> {
    let stored = cache
        .state::<Receipts>(&receipts_scope(&repository.repository, &config.prs.branch))
        .await?;
    let proven = stored.is_some();
    let pending = plans
        .iter()
        .any(|p| p.action().status == BranchStatus::Planned);
    let receipts = match stored {
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
    // An unreadable ref node hides a branch and its pull requests: what the
    // walk cannot name it must not settle around.
    if !refs.complete {
        return Ok(not_restored(repository, plans, "incomplete-refs"));
    }
    // The filtered walk holds only names under the prefix, so a branch the
    // receipts still name being absent means it was renamed out of the
    // prefix or deleted — GitHub's rename keeps its open pull request,
    // whose rechecks and dashboard row the settled run would silently
    // drop. Unless a plan rewrites that branch this run — its
    // `missing-head` fails the restore on its own — take the full write
    // path: its REST pull listing and cleanup's own branch list see every
    // branch, exactly as an unfiltered run's would.
    if inventory.ref_filter.is_some()
        && receipts.writes.keys().any(|branch| {
            !refs.items.iter().flatten().any(|r| r.name == *branch)
                && !plans.iter().any(|plan| {
                    plan.action().status == BranchStatus::Planned && plan.action().branch == *branch
                })
        })
    {
        return Ok(not_restored(repository, plans, "receipted-branch-missing"));
    }
    let desired: BTreeSet<_> = plans.iter().map(|p| p.action().branch.as_str()).collect();
    let obsolete = refs.items.iter().flatten().any(|r| {
        let name = r.name.as_str();
        Some(name) != repository.default_branch.as_deref()
            && !desired.contains(name)
            && matches!(&r.target, Some(RefTarget::Commit(c))
                if c.author.as_ref().and_then(|a| a.user.as_ref()).is_some_and(|u| u.login == actor)
                    && (c.committer.as_ref().and_then(|a| a.user.as_ref())
                        .is_some_and(|u| u.login == actor || u.login == "web-flow"))
                    && c.signature.as_ref().is_some_and(|s| s.is_valid)
                    && (name.starts_with(&config.prs.branch)
                        || crate::trailers::version(&c.message)))
    });
    let mut restored = 0;
    for plan in plans
        .iter_mut()
        .filter(|p| p.action().status == BranchStatus::Planned)
    {
        let result = if plan.retry() {
            Err("checkbox-recheck")
        } else if let Some(receipt) = receipts.closed.get(&plan.action().branch) {
            if receipt.actor != actor {
                Err("actor-mismatch")
            } else if receipt.intent != intent(plan, config, recheck, server) {
                Err("intent-changed")
            } else if !closed.items.iter().flatten().any(|pr| match pr {
                ClosedNode::PullRequest(pr) => {
                    receipt.pr["node_id"].as_str() == Some(pr.id.as_str())
                        && pr.state == PullRequestState::CLOSED
                        && !pr.merged
                        && crate::pr::body::body_hash(&pr.body)
                            == crate::pr::body::body_hash(receipt.pr["body"].as_str().unwrap_or(""))
                        && pr.author.as_ref().map(actor_login).as_deref() == Some(actor)
                        && pr.head_repository.as_ref().is_some_and(|r| {
                            crate::pull_requests::same_repository(
                                &r.name_with_owner,
                                &repository.repository,
                            )
                        })
                        && pr.base_repository.as_ref().is_some_and(|r| {
                            crate::pull_requests::same_repository(
                                &r.name_with_owner,
                                &repository.repository,
                            )
                        })
                }
                _ => false,
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
        && inventory.refs.complete
        && inventory.closed.complete
        && inventory.unsettled.is_empty()
        && plans
            .iter()
            .all(|p| p.action().status != BranchStatus::Planned);
    tracing::debug!(target: "patchula_update::receipts", repo = %repository.repository,
        restored, complete, obsolete, unsettled = inventory.unsettled.len(), "write receipts restored");
    Ok(if complete {
        // An unfiltered walk just verified every claim these receipts
        // make against live data. When the walk ran unfiltered because
        // the prefix marker was absent or stale — a `prs.branch` flip
        // back, or receipts saved before the marker existed — these are
        // again the repository's most recent receipts: mark them, so the
        // next read may narrow the refs walk. A failed write only costs
        // the next run its narrowed walk.
        if proven
            && inventory.ref_filter.is_none()
            && let Err(error) = mark_prefix(cache, &repository.repository, &config.prs.branch).await
        {
            tracing::warn!(target: "patchula_update::receipts", repo = %repository.repository,
                %error, "receipts prefix marker not saved");
        }
        RestoreOutcome::Complete
    } else {
        RestoreOutcome::Partial
    })
}

fn restore_write(
    receipts: &Receipts,
    refs: &Page<InventoryRef>,
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
        .items
        .iter()
        .flatten()
        .find(|r| r.name == plan.action().branch)
        .ok_or("missing-head")?;
    let Some(RefTarget::Commit(commit)) = &r.target else {
        return Err("head-oid-changed");
    };
    if commit.oid != receipt.commit {
        return Err("head-oid-changed");
    }
    if commit.tree.oid != receipt.tree {
        return Err("tree-oid-changed");
    }
    if commit.signature.as_ref().is_none_or(|s| !s.is_valid) {
        return Err("signature-invalid");
    }
    let prs = &r.associated_pull_requests;
    if prs.items.len() != 1 || !prs.complete {
        return Err("pr-state-changed");
    }
    let node = prs.items[0].as_ref().ok_or("pr-state-incomplete")?;
    let pr = crate::pull_requests::tracked_pr(node).ok_or("pr-state-incomplete")?;
    if pr.pr.user.as_ref().map(|u| u.login.as_str()) != Some(actor) {
        return Err("actor-mismatch");
    }
    if pr.pr.head.sha != receipt.commit || PrState::from_inventory(node).value() != receipt.pr {
        return Err("pr-state-changed");
    }
    if receipt.base != plan.action().base_commit
        && (config.prs.rebase != Rebase::Conflicts || plan.has_lock_jobs())
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
) -> Result<(), Error> {
    let scope = receipts_scope(&repository.repository, &config.prs.branch);
    let Some(mut receipts) = cache.state::<Receipts>(&scope).await? else {
        return Ok(());
    };
    for plan in plans {
        let Some(pr) = plan.restored_pr() else {
            continue;
        };
        if plan.action().status == BranchStatus::Merged {
            receipts.writes.remove(&plan.action().branch);
        } else if let Some(receipt) = receipts.writes.get_mut(&plan.action().branch) {
            receipt.pr = PrState::from_rest(&pr.pr).value();
        }
    }
    cache.set_state(&scope, &receipts).await?;
    mark_prefix(cache, &repository.repository, &config.prs.branch).await
}

pub async fn save_receipts(
    cache: &Cache,
    repository: &Repository,
    config: &Config,
    plans: &[Plan],
    lifecycle: &Lifecycle,
) -> Result<(), Error> {
    let mut receipts = Receipts::default();
    for plan in plans {
        if plan.held(&crate::plan::Hold::ClosedByOwner)
            && let Some(pr) = lifecycle.closed_for(plan)
        {
            let Some(actor) = pr.pr.user.as_ref().map(|u| u.login.as_str()) else {
                fallback(repository, plan, "missing-pr-author");
                continue;
            };
            receipts.closed.insert(
                plan.action().branch.clone(),
                ClosedReceipt {
                    actor: actor.into(),
                    intent: intent(plan, config, lifecycle.recheck, lifecycle.server.as_deref()),
                    pr: json!({"node_id":pr.pr.node_id,"body":pr.pr.body}),
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
        let (Some(commit), Some(tree)) = (plan.action().commit.as_ref(), plan.tree()) else {
            continue;
        };
        let Some(pr) = plan.restored_pr().or_else(|| {
            lifecycle
                .prs
                .iter()
                .find(|pr| pr.pr.state == "open" && pr.pr.head.branch == plan.action().branch)
        }) else {
            continue;
        };
        let Some(actor) = pr.pr.user.as_ref().map(|u| u.login.as_str()) else {
            fallback(repository, plan, "missing-pr-author");
            continue;
        };
        if pr.pr.head.sha != *commit {
            fallback(repository, plan, "pr-head-oid-changed");
            continue;
        }
        receipts.writes.insert(
            plan.action().branch.clone(),
            Receipt {
                actor: actor.into(),
                commit: commit.clone(),
                tree: tree.to_owned(),
                base: plan.action().base_commit.clone(),
                intent: intent(plan, config, lifecycle.recheck, lifecycle.server.as_deref()),
                pr: PrState::from_rest(&pr.pr).value(),
                fingerprint: plan.fingerprint().to_owned(),
                files: plan.action().files.clone(),
            },
        );
    }
    cache
        .set_state(
            &receipts_scope(&repository.repository, &config.prs.branch),
            &receipts,
        )
        .await?;
    mark_prefix(cache, &repository.repository, &config.prs.branch).await?;
    tracing::debug!(target: "patchula_update::receipts", repo = %repository.repository,
        count = receipts.writes.len() + receipts.closed.len(), writes = receipts.writes.len(),
        closed = receipts.closed.len(), "write receipts saved");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_state_golden() {
        let pr = serde_json::from_value::<model::PullRequest>(serde_json::json!({
            "number": 17,
            "node_id": "PR_node17",
            "html_url": "https://github.com/acme/repo/pull/17",
            "title": "chore(deps): bump serde from 1.0.0 to 1.0.1",
            "body": "Bumps serde.\n\nSigned-off-by: patchula[bot] <bot@patchula>",
            "state": "open",
            "head": { "ref": "patchula/serde-1.0.1", "sha": "abc123", "repo": { "full_name": "acme/repo" } },
            "base": { "ref": "main", "repo": { "full_name": "acme/repo" } },
            "user": { "login": "patchula[bot]" },
            "labels": [{ "name": "automerge" }, { "name": "dependencies" }],
            "auto_merge": { "enabled_at": "2025-01-01T00:00:00Z" },
        }))
        .unwrap();
        // Frozen receipt bytes — captured from the Value-based `pr_state`
        // before the typed-inventory refactor.
        assert_eq!(
            serde_json::to_string(&PrState::from_rest(&pr).value()).unwrap(),
            "{\"actor\":\"patchula[bot]\",\"base\":{\"ref\":\"main\",\"repo\":\"acme/repo\"},\"body_hash\":\"e7a9832c1be74db14193558e3b03bb92d1c607068599b5ebe1aca13225191acf\",\"head\":{\"ref\":\"patchula/serde-1.0.1\",\"repo\":\"acme/repo\",\"sha\":\"abc123\"},\"labels\":[\"automerge\",\"dependencies\"],\"merge\":true,\"number\":17,\"title\":\"chore(deps): bump serde from 1.0.0 to 1.0.1\"}"
        );
        // The inventory path builds the identical bytes.
        let inventory_pr = serde_json::from_value::<InventoryPr>(serde_json::json!({
            "number": 17,
            "id": "PR_node17",
            "url": "https://github.com/acme/repo/pull/17",
            "title": "chore(deps): bump serde from 1.0.0 to 1.0.1",
            "body": "Bumps serde.\n\nSigned-off-by: patchula[bot] <bot@patchula>",
            "headRefName": "patchula/serde-1.0.1",
            "headRefOid": "abc123",
            "baseRefName": "main",
            "author": { "__typename": "User", "login": "patchula[bot]" },
            "headRepository": { "nameWithOwner": "acme/repo" },
            "baseRepository": { "nameWithOwner": "acme/repo" },
            "labels": { "pageInfo": { "hasNextPage": false, "endCursor": null },
                "nodes": [{ "name": "automerge" }, { "name": "dependencies" }] },
            "autoMergeRequest": { "enabledAt": "2025-01-01T00:00:00Z" },
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_string(&PrState::from_inventory(&inventory_pr).value()).unwrap(),
            serde_json::to_string(&PrState::from_rest(&pr).value()).unwrap()
        );
    }

    #[test]
    fn bot_actors_are_marked_and_humans_pass_through() {
        let actor = |typename: &str, login: &str| Actor {
            typename: typename.into(),
            login: login.into(),
        };
        assert_eq!(actor_login(&actor("Bot", "hubot")), "hubot[bot]");
        assert_eq!(
            actor_login(&actor("Bot", "dependabot[bot]")),
            "dependabot[bot]"
        );
        assert_eq!(actor_login(&actor("User", "octocat")), "octocat");
        assert_eq!(actor_login(&actor("User", "hubot[bot]")), "hubot[bot]");
    }
}
