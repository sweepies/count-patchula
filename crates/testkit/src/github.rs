//! A stateful in-memory GitHub for offline tests: no sockets or network.
//!
//! [`FakeGitHub`] is the shared extraction of the write-path suites' fakes.
//! A repository's refs, commits, trees, blobs, pull requests, issues,
//! checks and merge settings live as REST-shaped JSON — GitHub's terms,
//! not the client's typed model — so the fake stays an independent oracle
//! for what the code asks and writes. Object ids are counters (`commit-0`,
//! `tree-1`), because tests pin them. One server holds several
//! repositories: [`FakeGitHub::new`] names the one the test drives;
//! [`FakeGitHub::add_repository`] adds more (upstream packages the
//! datasources read, or extra installed repositories for discovery).
//!
//! Two layers sit on that state:
//!
//! - **Scripted faults** script against a [`Matcher`] through
//!   [`FakeGitHub::on`]: [`On::respond`] answers without applying the
//!   request, [`On::then_fail`] applies it then fails the answer, and
//!   [`On::mutate`] changes state at the request's point.
//! - **Server properties** are typed [`Settings`]: page sizes, unsigned
//!   commits, read lag, truncated trees and latency.
//!
//! Requests are recorded for assertions: [`FakeGitHub::requests`] returns
//! `(method, path)` pairs, [`FakeGitHub::writes`] `(path, body)` and
//! [`FakeGitHub::log`] `(method, path, body)`. Every request also lands on
//! the thread's [`crate::transcripts`] log, and the fake enforces the
//! check: dropping a `FakeGitHub` whose transcript never ran through
//! [`crate::transcripts::check`] panics, so a test cannot silently skip
//! it.
//!
//! Hosts that are not GitHub — registries, the versions host, OSV — go
//! through [`Hosts`]: it routes `api.github.com` to the fake and every
//! other host to a per-test scripted route, recording every request so
//! host mix-ins still produce a checked transcript.

use crate::transcripts;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use patchula_http::{Exchange, Response, Transport};
use reqwest::{Method, Request, StatusCode};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// A content validator for the ref and tag listings, which GitHub etags as
/// it does any changed representation.
fn etag(nodes: &[Value]) -> String {
    Sha256::digest(serde_json::to_vec(nodes).unwrap())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn response(status: u16, body: Value) -> Response {
    Response {
        status: StatusCode::from_u16(status).unwrap(),
        headers: Default::default(),
        body: serde_json::to_vec(&body).unwrap(),
        stale: None,
    }
}

/// A JSON answer for a scripted [`Hosts`] route.
pub fn json(status: u16, body: Value) -> Response {
    response(status, body)
}

/// A stored commit as GitHub's REST schemas serve it: `git-commit` requires
/// `sha`, `message` and `verification`, and its content object requires
/// `message` — terse seeds name only the fields a test steers, so the server
/// fills the rest the way GitHub always does. `tree` stays seeded: a read of
/// a commit without one fails the way a missing tree always did.
fn commit_answer(sha: &str, commit: &Value) -> Value {
    let mut answer = commit.clone();
    let object = answer.as_object_mut().unwrap();
    object.entry("sha").or_insert_with(|| json!(sha));
    object.entry("message").or_insert_with(|| json!(""));
    object
        .entry("verification")
        .or_insert_with(|| json!({"verified":true}));
    answer
}

/// The `commit` schema's `author`/`committer` accounts for a stored commit:
/// GitHub maps them to users — `actor`/`committer_login` steering fields —
/// with the bot's own authorship as the default.
fn commit_author(commit: &Value, key: &str, default: &str) -> Value {
    json!({"login":commit.get(key).and_then(Value::as_str).unwrap_or(default)})
}

/// The git blob sha for `content` — the same formula the client proves.
fn blob_sha(content: &str) -> String {
    use sha1::Digest;
    let mut digest = sha1::Sha1::new();
    digest.update(format!("blob {}\0", content.len()));
    digest.update(content);
    digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn graphql_author(pr: &Value) -> Value {
    if let Some(actor) = pr.get("graphql_author") {
        return actor.clone();
    }
    let login = pr["user"]["login"].as_str().unwrap();
    if let Some(bot) = login.strip_suffix("[bot]") {
        json!({"__typename":"Bot","login":bot})
    } else {
        json!({"__typename":"User","login":login})
    }
}

/// The REST login an issue `author` node implies for `filterBy.createdBy`:
/// a `Bot`'s GraphQL `login` gains the `[bot]` suffix it omits, as the
/// client's `actor_login` reads it; other typenames answer their `login`
/// verbatim and a missing or malformed author matches nothing.
fn issue_author_login(issue: &Value) -> Option<String> {
    let author = &issue["author"];
    let login = author["login"].as_str()?;
    Some(
        if author["__typename"] == "Bot" && !login.ends_with("[bot]") {
            format!("{login}[bot]")
        } else {
            login.to_owned()
        },
    )
}

/// The `__typename` a user object reports: logins ending in `[bot]` answer
/// `Bot`, others `User`.
fn viewer_typename(login: &str) -> &'static str {
    if login.ends_with("[bot]") {
        "Bot"
    } else {
        "User"
    }
}

/// `node` cloned with its `__typename` stamped, for seeded nodes the fake
/// stores in another surface's shape — the seed stays untouched, the
/// served node declares its type.
fn with_typename(node: &Value, typename: &str) -> Value {
    let mut node = node.clone();
    node.as_object_mut()
        .unwrap()
        .insert("__typename".into(), json!(typename));
    node
}

/// The `PullRequestState` a stored PR answers on GraphQL: a merged pull
/// is `MERGED`, never `CLOSED` — `CLOSED` is "closed without being
/// merged" (`crates/github/graphql/schema.docs.graphql`,
/// `PullRequestState`).
fn pull_state(pr: &Value) -> &'static str {
    if pr["state"] == "open" {
        "OPEN"
    } else if !pr["merged_at"].is_null() {
        "MERGED"
    } else {
        "CLOSED"
    }
}

/// A stored PR's `created_at` as a sortable instant; a seed without one
/// sorts oldest.
fn pull_created(pr: &Value) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(pr["created_at"].as_str()?)
        .ok()
        .map(|at| at.timestamp_millis())
}

/// The `Repository` a PR's `head.repo`/`base.repo` names, or null for a
/// repository GitHub no longer holds — a deleted fork's head reads null.
fn pull_repo(repo: &Value) -> Value {
    if repo.is_null() {
        Value::Null
    } else {
        json!({"__typename":"Repository","nameWithOwner":repo["full_name"]})
    }
}

/// A `PullRequest` node for the GraphQL connections: the union of fields
/// the `nodes`, `pullRequests` and `associatedPullRequests` answers
/// carry, built from the stored REST object and stamped with
/// `__typename` so the pruner's inline-fragment matching sees the node's
/// type.
fn pull_node(pr: &Value, state: &State) -> Value {
    // `PullRequest.title` and `.body` are `String!`: GitHub answers the
    // empty string for a missing body, never null.
    let title = pr["title"].as_str().unwrap_or_default();
    let body = pr["body"].as_str().unwrap_or_default();
    json!({"__typename":"PullRequest",
    "number":pr["number"],"id":pr["node_id"],"url":pr["html_url"],
    "title":title,"body":body,
    "state":pull_state(pr),
    "merged":!pr["merged_at"].is_null(),
    "createdAt":pr["created_at"],
    "headRefName":pr["head"]["ref"],"headRefOid":pr["head"]["sha"],
    "baseRefName":pr["base"]["ref"],"author":graphql_author(pr),
    "headRepository":pull_repo(&pr["head"]["repo"]),
    "baseRepository":pull_repo(&pr["base"]["repo"]),
    "labels":{"__typename":"LabelConnection","nodes":pr["labels"],
        "pageInfo":{"__typename":"PageInfo",
            "hasNextPage":state.settings.label_next_page == pr["number"].as_u64()}},
    "autoMergeRequest":if pr["auto_merge"].is_null() {
        Value::Null
    } else {
        with_typename(&pr["auto_merge"], "AutoMergeRequest")
    }})
}

/// Server properties: knobs that change what the server is, not what it does
/// to one request.
#[derive(Default)]
pub struct Settings {
    /// `pulls` listing page size; `None` means the query's `per_page`
    /// (GitHub's default is 30).
    pub pr_page_size: Option<usize>,
    /// Inventory `refs` connection page size.
    pub ref_page_size: Option<usize>,
    /// New commits report unverified signatures.
    pub unsigned: bool,
    /// Tree listings report `truncated: true`.
    pub truncated: bool,
    /// Synthetic latency added to every answer.
    pub latency: Duration,
    /// PR head reads stay behind ref updates for this many reads — GitHub's
    /// replication lag.
    pub pr_lag_reads: usize,
    /// The pull number whose `labels` connection reports a second page.
    pub label_next_page: Option<u64>,
    /// The branch whose `associatedPullRequests` reports a second page.
    pub pr_next_page: Option<String>,
}

/// The merge surfaces a repository answers, identical on REST
/// (`allow_*`, `delete_branch_on_merge`) and GraphQL (`*Allowed`,
/// `deleteBranchOnMerge`). GitHub's defaults: every merge method on,
/// auto-merge and delete-branch-on-merge off.
#[derive(Clone)]
pub struct Merge {
    /// `autoMergeAllowed` / `allow_auto_merge`.
    pub auto_merge: bool,
    /// `squashMergeAllowed` / `allow_squash_merge`.
    pub squash: bool,
    /// `mergeCommitAllowed` / `allow_merge_commit`.
    pub merge_commit: bool,
    /// `rebaseMergeAllowed` / `allow_rebase_merge`.
    pub rebase: bool,
    /// `deleteBranchOnMerge` / `delete_branch_on_merge`; a merged head's
    /// ref is deleted with the merge.
    pub delete_branch: bool,
}
impl Default for Merge {
    fn default() -> Self {
        Self {
            auto_merge: false,
            squash: true,
            merge_commit: true,
            rebase: true,
            delete_branch: false,
        }
    }
}

/// One repository the server holds: refs, commits, trees, blobs, pull
/// requests, issues, check contexts, release nodes and settings, all as
/// REST-shaped JSON in GitHub's terms.
pub struct Repository {
    /// `owner/name`.
    pub name: String,
    /// REST `node_id`, GraphQL `id`, the `updateRefs` `repositoryId`.
    pub node_id: String,
    /// `user/repos` and `installation/repositories` list repositories the
    /// token sees; upstreams seeded only for reads stay off them.
    pub installed: bool,
    /// REST `private`, GraphQL `isPrivate` and the discovery listings'
    /// `private`.
    pub private: bool,
    /// Every read answers 404 — the repository was deleted after the
    /// installation listed it, and the listings lag the deletion.
    pub deleted: bool,
    /// The default branch's name.
    pub default_branch: String,
    /// Head sha of the default branch.
    pub base: String,
    /// `refs/{namespace}` → object sha. Deletions record the zero sha.
    pub refs: BTreeMap<String, String>,
    /// `git/trees/{sha}` answers: each tree's `tree` array.
    pub trees: BTreeMap<String, Value>,
    /// Commits by sha; extra `actor`/`committer_login` fields steer
    /// authorship.
    pub commits: BTreeMap<String, Value>,
    /// Blob contents by sha, for `git/blobs/{sha}` and `object(oid:)` reads.
    pub blobs: BTreeMap<String, String>,
    /// Pull requests by number, REST PR objects.
    pub prs: BTreeMap<u64, Value>,
    /// Issues by number, in the inventory's node shape.
    pub issues: BTreeMap<u64, Value>,
    /// `statusCheckRollup` contexts check reads report.
    pub checks: Vec<Value>,
    /// `releases` connection nodes for release-catalog reads.
    pub releases: Vec<Value>,
    /// Merge settings on both surfaces.
    pub merge: Merge,
    /// A `defaultBranchRef` answer used verbatim — history, rules and
    /// refUpdateRule — replacing the synthesized one.
    pub default_branch_ref: Option<Value>,
    /// Commits `compare` reports ahead of a head.
    pub history: Vec<String>,
    /// Heads `compare` reports as merged into the base.
    pub merged_heads: BTreeSet<String>,
    /// The upstream release's body text, served by `releases/tags/{tag}`
    /// and the `release` GraphQL field.
    pub release_note_body: Option<String>,
    /// The `contents` listing, when the repository serves one.
    pub contents: Option<Value>,
    /// `releases/tags/{tag}` serves bodies at all.
    pub release_tags: bool,
    /// The versions host's `releases.json` document.
    pub mirror_releases: Value,
    /// The versions host's `tags.json` document.
    pub mirror_tags: Value,
    /// Heads applied after `settings.pr_lag_reads` more reads.
    pending_heads: BTreeMap<u64, Value>,
}
impl Repository {
    /// Move the default branch's head to `sha`, cloning the current head's
    /// commit when `sha` is unknown so commit and tree reads still resolve.
    pub fn move_base(&mut self, sha: &str) {
        if !self.commits.contains_key(sha) {
            let mut commit = self.commits[&self.base].clone();
            commit["sha"] = json!(sha);
            self.commits.insert(sha.into(), commit);
        }
        self.refs
            .insert(format!("refs/heads/{}", self.default_branch), sha.into());
        self.base = sha.into();
    }

    fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            node_id: format!("{name}-id"),
            installed: false,
            private: false,
            deleted: false,
            default_branch: "main".into(),
            base: String::new(),
            refs: BTreeMap::new(),
            trees: BTreeMap::new(),
            commits: BTreeMap::new(),
            blobs: BTreeMap::new(),
            prs: BTreeMap::new(),
            issues: BTreeMap::new(),
            checks: Vec::new(),
            releases: Vec::new(),
            merge: Merge::default(),
            default_branch_ref: None,
            history: Vec::new(),
            merged_heads: BTreeSet::new(),
            release_note_body: None,
            contents: None,
            release_tags: false,
            mirror_releases: json!({"releases":[],"truncated":false}),
            mirror_tags: json!({"tags":[],"truncated":false}),
            pending_heads: BTreeMap::new(),
        }
    }
}

/// What one route served: how many requests it answered and the response
/// bytes it returned — the counts a listing measurement reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Traffic {
    /// Requests answered on the route.
    pub requests: usize,
    /// Response body bytes the route served.
    pub bytes: usize,
}

/// The server's state: REST-shaped JSON in GitHub's terms — an independent
/// oracle, never the client's typed model.
pub struct State {
    /// Repositories by `owner/name`: the one the test drives plus
    /// upstreams and extra installed repositories.
    pub repositories: BTreeMap<String, Repository>,
    /// The acting login — `viewer{login}`, new pull requests' `user` and
    /// new issues' `author`.
    pub actor: String,
    /// Installation ids `app/installations` and `{repo,org}/installation`
    /// report.
    pub installations: Vec<u64>,
    /// Minted installation-token counter for `access_tokens`.
    minted: usize,
    /// `(method, path)` of every request, in arrival order.
    pub requests: Vec<(String, String)>,
    /// `(path, body)` of every mutation request, in arrival order.
    pub writes: Vec<(String, Value)>,
    /// ReleaseNotes operation bodies, in arrival order.
    pub release_queries: Vec<Value>,
    /// Per route — `"GET /repos/{repo}/pulls?state=all"` — how many
    /// requests the server answered and how many body bytes it served,
    /// since the state was made or the map last cleared.
    pub traffic: BTreeMap<String, Traffic>,
    /// Server properties.
    pub settings: Settings,
    /// `(method, path, body)` of every request, in arrival order.
    log: Vec<(String, String, Value)>,
    /// The repository `FakeGitHub::new` named.
    primary: String,
}

impl State {
    fn new() -> Self {
        let mut repositories = BTreeMap::new();
        for (name, contents, releases, tags, release_tags) in [
            (
                "nodejs/node",
                json!([]),
                json!({"releases":[],"truncated":false}),
                json!({"tags":[],"truncated":false}),
                false,
            ),
            (
                "actions/checkout",
                json!({"tree":[],"truncated":false}),
                json!({"releases":[{"tag":"20.0.0"},{"tag":"22.0.0"}],"truncated":false}),
                json!({"tags":[{"tag":"20.0.0"},{"tag":"22.0.0"}],"truncated":false}),
                true,
            ),
        ] {
            let mut upstream = Repository::new(name);
            upstream.contents = Some(contents);
            upstream.mirror_releases = releases;
            upstream.mirror_tags = tags;
            upstream.release_tags = release_tags;
            repositories.insert(name.to_owned(), upstream);
        }
        Self {
            repositories,
            actor: "patchula[bot]".into(),
            installations: vec![10],
            minted: 0,
            requests: Vec::new(),
            writes: Vec::new(),
            release_queries: Vec::new(),
            traffic: BTreeMap::new(),
            settings: Settings::default(),
            log: Vec::new(),
            primary: String::new(),
        }
    }
    /// The repository `FakeGitHub::new` named — the one the test drives.
    pub fn repo(&self) -> &Repository {
        &self.repositories[&self.primary]
    }
    /// The primary repository, mutable.
    pub fn repo_mut(&mut self) -> &mut Repository {
        self.repositories.get_mut(&self.primary).unwrap()
    }
}

/// What a matched request carried, handed to [`On::mutate`] closures.
pub struct Hit {
    /// The request's method.
    pub method: Method,
    /// The request's path, without host or query.
    pub path: String,
    /// The `owner/name` repository the request addresses, REST or GraphQL.
    pub repo: Option<String>,
    /// The pull number of a `pulls/{n}` route.
    pub pull: Option<u64>,
}

/// Which requests a scripted fault matches.
#[derive(Clone)]
pub enum Matcher {
    /// `GET /repos/{repo}/pulls/{number}`; `None` matches every pull read.
    PullRead {
        /// The repository, `owner/name`.
        repo: String,
        /// A specific pull, or `None` for all pulls.
        number: Option<u64>,
    },
    /// `POST /repos/{repo}/pulls` — pull creation.
    PullCreate {
        /// The repository, `owner/name`.
        repo: String,
    },
    /// `PUT /repos/{repo}/pulls/{number}/merge`; `None` matches every merge.
    PullMerge {
        /// The repository, `owner/name`.
        repo: String,
        /// A specific pull, or `None` for all merges.
        number: Option<u64>,
    },
    /// `GET /repos/{repo}` — repository metadata.
    RepoRead {
        /// The repository, `owner/name`.
        repo: String,
    },
    /// `PATCH /repos/{repo}/issues/{number}`; `None` matches every issue
    /// patch.
    IssuePatch {
        /// The repository, `owner/name`.
        repo: String,
        /// A specific issue, or `None` for all issues.
        number: Option<u64>,
    },
    /// A REST route by method and exact path, without query.
    Route {
        /// The request's method.
        method: Method,
        /// The request's path, without host or query.
        path: String,
    },
    /// A GraphQL document that names this field unaliased, at any depth —
    /// `"updateRefs"` for the mutation, `"statusCheckRollup"` for the
    /// checks query, `"associatedPullRequests"` for the write inventory.
    Graphql(&'static str),
    /// An `updateRefs` mutation that deletes a ref (`afterOid` zero).
    RefDelete,
}

type Mutation = Box<dyn Fn(&mut State, &Hit) + Send + Sync>;

enum Action {
    /// Answer without running the handler — the request never happened.
    Respond(u16, Value),
    /// Run the handler, then answer with this status on its body.
    ThenFail(u16),
    /// Change state at the request's point, then answer normally.
    Mutate(Mutation),
}

struct Rule {
    matcher: Matcher,
    after: usize,
    times: Option<usize>,
    action: Action,
}

impl Rule {
    /// `true` when this match is spent on the `after` window or when `times`
    /// is exhausted; otherwise counts the match down and fires.
    fn consume(&mut self) -> bool {
        if self.after > 0 {
            self.after -= 1;
            return false;
        }
        match &mut self.times {
            Some(0) => false,
            Some(times) => {
                *times -= 1;
                true
            }
            None => true,
        }
    }
}

/// The builder [`FakeGitHub::on`] returns: finish with [`On::respond`],
/// [`On::then_fail`] or [`On::mutate`].
#[must_use = "a fault rule does nothing until `respond`, `then_fail` or `mutate` arms it"]
pub struct On<'a> {
    fake: &'a FakeGitHub,
    matcher: Matcher,
    after: usize,
    times: Option<usize>,
}

impl On<'_> {
    /// The rule fires at most `times` matches; unlimited when unset.
    pub fn times(mut self, times: usize) -> Self {
        self.times = Some(times);
        self
    }
    /// The rule skips its first `after` matches.
    pub fn after(mut self, after: usize) -> Self {
        self.after = after;
        self
    }
    /// Answer `status`/`body` without applying the request.
    pub fn respond(self, status: u16, body: Value) {
        self.push(Action::Respond(status, body));
    }
    /// Apply the request, then answer `status` on the handler's body.
    pub fn then_fail(self, status: u16) {
        self.push(Action::ThenFail(status));
    }
    /// Run `change` on the server's state at the request's point.
    pub fn mutate(self, change: impl Fn(&mut State, &Hit) + Send + Sync + 'static) {
        self.push(Action::Mutate(Box::new(change)));
    }
    fn push(self, action: Action) {
        self.fake.rules.lock().unwrap().push(Rule {
            matcher: self.matcher,
            after: self.after,
            times: self.times,
            action,
        });
    }
}

/// A stateful in-memory GitHub: REST and GraphQL endpoints over several
/// repositories, scripted faults, request recording and transcript
/// enforcement. Construct with [`FakeGitHub::new`], wrap in `Arc` and hand
/// to the client's `with_transport`; tests that also need other hosts wrap
/// it in [`Hosts`].
pub struct FakeGitHub {
    /// `owner/name` of the primary repository — kept outside the state
    /// lock so fault helpers can be armed while a test holds
    /// [`FakeGitHub::state`].
    repo: String,
    state: Mutex<State>,
    rules: Mutex<Vec<Rule>>,
    /// Set by `transcripts::check`; unset at drop means the test skipped it.
    transcript_checked: Arc<AtomicBool>,
}

/// A GraphQL argument: a variable lookup or a literal as JSON — enum
/// names, ints, lists and `orderBy`-style objects all land here.
enum Arg {
    Var(String),
    Lit(Value),
}

/// A GraphQL field, distilled to owned facts — routing needs no AST
/// borrows.
struct GqlField {
    /// The response key: the alias or the field name.
    key: String,
    name: String,
    /// `(argument name, value)` pairs.
    args: Vec<(String, Arg)>,
    /// Selections below this field: fields and inline fragments.
    selections: Vec<GqlSelection>,
}

/// A selection inside a set: a field, or an inline fragment whose fields
/// apply when the answer's `__typename` matches its condition.
enum GqlSelection {
    Field(GqlField),
    Fragment {
        /// The `... on T` type condition; `None` is a bare `... { }`.
        on: Option<String>,
        selections: Vec<GqlSelection>,
    },
}
impl GqlSelection {
    /// Every unaliased field name in this selection's subtree.
    fn names(&self) -> BTreeSet<String> {
        match self {
            GqlSelection::Field(field) => field.names(),
            GqlSelection::Fragment { selections, .. } => {
                selections.iter().flat_map(GqlSelection::names).collect()
            }
        }
    }
}

/// The fields `selections` asks for on `typename`: direct fields plus the
/// fields of matching inline fragments, for resolvers that iterate the
/// selection to build their answer.
fn gather<'a>(selections: &'a [GqlSelection], typename: &str, out: &mut Vec<&'a GqlField>) {
    for selection in selections {
        match selection {
            GqlSelection::Field(field) => out.push(field),
            GqlSelection::Fragment { on, selections }
                if on.as_deref().is_none_or(|on| on == typename) =>
            {
                gather(selections, typename, out);
            }
            GqlSelection::Fragment { .. } => {}
        }
    }
}

/// Project `answer` — a resolver's whole object — onto `selections`, the
/// way GraphQL prunes a full value to the selection set: a field answers
/// under its response key (the alias), an inline fragment contributes its
/// selections when the object's `__typename` matches its condition, and a
/// selected field the resolver did not produce stays absent — not null.
fn prune(answer: &Value, selections: &[GqlSelection]) -> Value {
    match answer {
        Value::Object(object) => {
            let mut pruned = Map::new();
            prune_fields(object, selections, &mut pruned);
            Value::Object(pruned)
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| prune(item, selections)).collect())
        }
        _ => answer.clone(),
    }
}
fn prune_fields(
    answer: &Map<String, Value>,
    selections: &[GqlSelection],
    pruned: &mut Map<String, Value>,
) {
    for selection in selections {
        match selection {
            GqlSelection::Field(field) => {
                // A selection's arguments make each answer its own, so
                // resolvers key by response key; nested objects they build
                // keep field names, where an alias finds the named field.
                if let Some(value) = answer
                    .get(field.key.as_str())
                    .or_else(|| answer.get(field.name.as_str()))
                {
                    pruned.insert(field.key.clone(), prune(value, &field.selections));
                }
            }
            GqlSelection::Fragment { on, selections } => {
                // A node the fake forgot to type would silently lose every
                // fragment field and read as "no data"; fail loudly instead.
                let applies = on.as_deref().is_none_or(|condition| {
                    let typename = answer
                        .get("__typename")
                        .and_then(Value::as_str)
                        .unwrap_or_else(|| {
                            panic!(
                                "the fake answered a node without __typename under `... on {condition}`: {}",
                                Value::Object(answer.clone())
                            )
                        });
                    typename == condition
                });
                if applies {
                    prune_fields(answer, selections, pruned);
                }
            }
        }
    }
}

impl GqlField {
    /// This field's `name` argument resolved against the operation's
    /// variables.
    fn arg<'a>(&'a self, name: &str, body: &'a Value) -> Option<&'a str> {
        self.args
            .iter()
            .find(|(arg, _)| arg == name)
            .and_then(|(_, value)| match value {
                Arg::Var(var) => body["variables"][var.as_str()].as_str(),
                Arg::Lit(text) => text.as_str(),
            })
    }
    /// This field's `name` argument resolved to its JSON value, for
    /// arguments that are not strings — lists, ints, input objects. A
    /// null answer (unset variable, literal null) is no argument.
    fn arg_json(&self, name: &str, body: &Value) -> Option<Value> {
        self.args
            .iter()
            .find(|(arg, _)| arg == name)
            .map(|(_, value)| match value {
                Arg::Var(var) => body["variables"][var.as_str()].clone(),
                Arg::Lit(value) => value.clone(),
            })
            .filter(|value| !value.is_null())
    }
    /// Every unaliased field name in this field's subtree, its own name
    /// included when unaliased.
    fn names(&self) -> BTreeSet<String> {
        let mut names: BTreeSet<String> = self
            .selections
            .iter()
            .flat_map(GqlSelection::names)
            .collect();
        if self.key == self.name {
            names.insert(self.name.clone());
        }
        names
    }
    /// The fields this level selects on `typename`: direct fields plus the
    /// fields of matching inline fragments.
    fn fields(&self, typename: &str) -> Vec<&GqlField> {
        let mut fields = Vec::new();
        gather(&self.selections, typename, &mut fields);
        fields
    }
}

/// A literal GraphQL argument as JSON: strings, enums and booleans are
/// their values, ints and floats their numbers, lists and input objects
/// their JSON shapes. A variable nested inside a literal is null — the
/// fake resolves variables only at the top level.
fn arg_literal<'a>(value: &graphql_parser::query::Value<'a, &'a str>) -> Value {
    use graphql_parser::query::Value as ArgValue;
    match value {
        ArgValue::Variable(_) | ArgValue::Null => Value::Null,
        ArgValue::Int(number) => number.as_i64().map_or(Value::Null, |n| json!(n)),
        ArgValue::Float(number) => json!(number),
        ArgValue::String(text) => json!(text),
        ArgValue::Boolean(flag) => json!(flag),
        ArgValue::Enum(name) => json!(name),
        ArgValue::List(items) => Value::Array(items.iter().map(arg_literal).collect()),
        ArgValue::Object(map) => Value::Object(
            map.iter()
                .map(|(name, value)| ((*name).to_owned(), arg_literal(value)))
                .collect(),
        ),
    }
}

/// One GraphQL operation's kind and root selections.
struct GqlOp {
    mutation: bool,
    selections: Vec<GqlSelection>,
}

/// The route facts request matching and the write log need, parsed once per
/// request.
struct Ctx {
    method: Method,
    path: String,
    repo: Option<String>,
    /// The `{n}` of a `pulls/{n}` route.
    pull: Option<u64>,
    /// `GET /repos/{repo}/pulls/{n}` — its `mutate` rules fire after read lag.
    pull_read: bool,
    /// `PATCH /repos/{repo}/issues/{n}`.
    issue_patch: Option<u64>,
    /// `PUT /repos/{repo}/pulls/{n}/merge`.
    pull_merge: Option<u64>,
    /// The request's GraphQL operations are mutations.
    mutation: bool,
    /// Every unaliased field name in the GraphQL document.
    names: BTreeSet<String>,
    /// An `updateRefs` mutation carrying a deletion.
    ref_deletion: bool,
    ops: Vec<GqlOp>,
}

/// Distill a selection set to owned [`GqlSelection`]s: fields carry their
/// response key, arguments and subtree; inline fragments keep their type
/// condition for [`prune`].
fn selections<'a>(set: &'a graphql_parser::query::SelectionSet<'a, &'a str>) -> Vec<GqlSelection> {
    use graphql_parser::query::{Selection, TypeCondition, Value as ArgValue};
    set.items
        .iter()
        .map(|selection| match selection {
            Selection::Field(field) => GqlSelection::Field(GqlField {
                key: field.alias.unwrap_or(field.name).to_owned(),
                name: field.name.to_owned(),
                args: field
                    .arguments
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.to_string(),
                            match value {
                                ArgValue::Variable(var) => Arg::Var((*var).to_owned()),
                                other => Arg::Lit(arg_literal(other)),
                            },
                        )
                    })
                    .collect(),
                selections: selections(&field.selection_set),
            }),
            // `... on Commit` keeps its type condition: resolvers build the
            // whole answer, and the pruner keeps the fragment's fields only
            // on a matching `__typename`.
            Selection::InlineFragment(fragment) => GqlSelection::Fragment {
                on: fragment
                    .type_condition
                    .as_ref()
                    .map(|TypeCondition::On(name)| (*name).to_owned()),
                selections: selections(&fragment.selection_set),
            },
            Selection::FragmentSpread(spread) => panic!(
                "the fake does not model named fragment spreads (`...{}`); inline the fragment",
                spread.fragment_name
            ),
        })
        .collect()
}

impl Ctx {
    fn new(request: &Request, path: &str, body: &Value) -> Self {
        use graphql_parser::query::{Definition, OperationDefinition, parse_query};
        let method = request.method().clone();
        let mut repo = None;
        let mut pull = None;
        let mut pull_read = false;
        let mut issue_patch = None;
        let mut pull_merge = None;
        if let Some(rest) = path.strip_prefix("/repos/")
            && let Some((owner, rest)) = rest.split_once('/')
        {
            let (name, rest) = rest.split_once('/').unwrap_or((rest, ""));
            repo = Some(format!("{owner}/{name}"));
            if let Some(number) = rest.strip_prefix("pulls/") {
                if let Some(merge) = number.strip_suffix("/merge") {
                    pull_merge = merge.parse().ok();
                } else {
                    pull = number.parse().ok();
                    pull_read = method == Method::GET && pull.is_some();
                }
            } else if let Some(number) = rest.strip_prefix("issues/")
                && !number.ends_with("/comments")
                && method == Method::PATCH
            {
                issue_patch = number.parse().ok();
            }
        }
        let mut names = BTreeSet::new();
        let mut mutation = false;
        let mut ref_deletion = false;
        let mut ops = Vec::new();
        if path == "/graphql" {
            let doc = parse_query::<&str>(body["query"].as_str().unwrap_or("-"))
                .unwrap_or_else(|error| panic!("unparseable GraphQL query: {error}"));
            for def in &doc.definitions {
                let (set, is_mutation) = match def {
                    Definition::Operation(OperationDefinition::Query(operation)) => {
                        (&operation.selection_set, false)
                    }
                    Definition::Operation(OperationDefinition::Mutation(operation)) => {
                        (&operation.selection_set, true)
                    }
                    Definition::Operation(OperationDefinition::SelectionSet(set)) => (set, false),
                    _ => panic!("unmodeled GraphQL definition"),
                };
                let selections = selections(set);
                mutation |= is_mutation;
                let mut fields = Vec::new();
                gather(
                    &selections,
                    if is_mutation { "Mutation" } else { "Query" },
                    &mut fields,
                );
                for field in fields {
                    names.extend(field.names());
                    if is_mutation && field.name == "updateRefs" {
                        ref_deletion = body["variables"]["input"]["refUpdates"]
                            .as_array()
                            .is_some_and(|refs| {
                                refs.iter().any(|r| {
                                    r["afterOid"] == "0000000000000000000000000000000000000000"
                                })
                            });
                    }
                    if repo.is_none()
                        && field.name == "repository"
                        && let (Some(owner), Some(name)) =
                            (field.arg("owner", body), field.arg("name", body))
                    {
                        repo = Some(format!("{owner}/{name}"));
                    }
                }
                ops.push(GqlOp {
                    mutation: is_mutation,
                    selections,
                });
            }
        }
        Self {
            method,
            path: path.to_owned(),
            repo,
            pull,
            pull_read,
            issue_patch,
            pull_merge,
            mutation,
            names,
            ref_deletion,
            ops,
        }
    }
    /// Writes land on the write log: GraphQL mutations, and any non-GET
    /// REST request.
    fn is_write(&self) -> bool {
        (self.path == "/graphql" && self.mutation)
            || (self.path != "/graphql" && self.method != Method::GET)
    }
    fn matches(&self, matcher: &Matcher) -> bool {
        match matcher {
            Matcher::PullRead { repo, number } => {
                self.pull_read
                    && self.repo.as_deref() == Some(repo.as_str())
                    && number.is_none_or(|n| self.pull == Some(n))
            }
            Matcher::PullCreate { repo } => {
                self.method == Method::POST && self.path == format!("/repos/{repo}/pulls")
            }
            Matcher::PullMerge { repo, number } => {
                self.pull_merge.is_some()
                    && self.repo.as_deref() == Some(repo.as_str())
                    && number.is_none_or(|n| self.pull_merge == Some(n))
            }
            Matcher::RepoRead { repo } => {
                self.method == Method::GET && self.path == format!("/repos/{repo}")
            }
            Matcher::IssuePatch { repo, number } => {
                self.issue_patch.is_some()
                    && self.repo.as_deref() == Some(repo.as_str())
                    && number.is_none_or(|n| self.issue_patch == Some(n))
            }
            Matcher::Route { method, path } => self.method == *method && self.path == *path,
            Matcher::Graphql(field) => self.names.contains(*field),
            Matcher::RefDelete => self.ref_deletion,
        }
    }
}

/// A root field's answer: a `data` entry, or an `errors` answer that
/// replaces the whole response.
enum FieldAnswer {
    Data(Value),
    Errors(Value),
}

impl FakeGitHub {
    /// A GitHub with one repository, `repo` (`"owner/name"`), on default
    /// branch `main`; it is the primary repository [`State::repo`]
    /// returns and the only one the discovery listings report installed.
    pub fn new(repo: &str) -> Self {
        let mut state = State::new();
        let mut primary = Repository::new(repo);
        primary.node_id = "repo-id".into();
        primary.installed = true;
        state.repositories.insert(repo.into(), primary);
        state.primary = repo.into();
        let this = Self {
            repo: repo.into(),
            state: Mutex::new(state),
            rules: Mutex::new(Vec::new()),
            transcript_checked: Arc::new(AtomicBool::new(false)),
        };
        transcripts::watch(&this.transcript_checked);
        this
    }
    /// Add a repository GitHub holds — an upstream the datasources read
    /// or, with `installed`, one the token sees.
    pub fn add_repository(&self, repo: &str, installed: bool) {
        let mut repository = Repository::new(repo);
        repository.installed = installed;
        self.state().repositories.insert(repo.into(), repository);
    }
    /// Seed the primary repository: `branch` points at commit `base` whose
    /// tree `"{base}-tree"` holds `entries` (REST tree entries).
    pub fn seed(&self, branch: &str, base: &str, entries: Vec<Value>) {
        let repo = self.repo.clone();
        self.seed_on(&repo, branch, base, entries);
    }
    /// Seed the named repository the way [`FakeGitHub::seed`] seeds the
    /// primary one.
    fn seed_on(&self, name: &str, branch: &str, base: &str, entries: Vec<Value>) {
        let mut state = self.state();
        let repo = state.repositories.get_mut(name).expect("seeded repository");
        repo.default_branch = branch.into();
        repo.base = base.into();
        repo.refs
            .insert(format!("refs/heads/{branch}"), base.into());
        repo.trees.insert(format!("{base}-tree"), json!(entries));
        repo.commits.insert(
            base.into(),
            json!({"sha":base,"tree":{"sha":format!("{base}-tree")}}),
        );
    }
    /// Seed the primary repository from file contents: every file becomes
    /// a blob (`git/blobs/{sha}` and `object(oid:)` reads answer its text)
    /// and a `100644` tree entry; `extra` entries join the tree verbatim.
    pub fn seed_files(
        &self,
        branch: &str,
        base: &str,
        files: &BTreeMap<String, String>,
        extra: Vec<Value>,
    ) {
        let repo = self.repo.clone();
        self.seed_files_on(&repo, branch, base, files, extra);
    }
    /// Seed the named repository the way [`FakeGitHub::seed_files`] seeds
    /// the primary one.
    pub fn seed_files_on(
        &self,
        name: &str,
        branch: &str,
        base: &str,
        files: &BTreeMap<String, String>,
        extra: Vec<Value>,
    ) {
        let mut entries: Vec<Value> = files
            .iter()
            .map(|(path, content)| {
                json!({"path":path,"sha":blob_sha(content),"mode":"100644","type":"blob"})
            })
            .collect();
        entries.extend(extra);
        self.seed_on(name, branch, base, entries);
        let mut state = self.state();
        let repo = state.repositories.get_mut(name).expect("seeded repository");
        for content in files.values() {
            repo.blobs.insert(blob_sha(content), content.clone());
        }
    }
    /// Seed `total` historical pull requests on the primary repository —
    /// for the listing measurement and for tests that need a large
    /// history. The mix is deterministic: every tenth pull is open, a
    /// third of the rest is merged, every seventh is a foreign author's
    /// (`octocat`) on a `topic/history-N` head; the actor's pulls sit on
    /// `patchula/history-N`. `created_at` steps an hour back per pull
    /// from now, so `sort=created` orders them and per-hour reads see a
    /// real spread. Numbering continues past the pulls already present,
    /// and only the open pulls' head branches exist.
    pub fn seed_pulls(&self, total: u64) {
        let mut state = self.state();
        let actor = state.actor.clone();
        let repo = state.repo_mut();
        let name = repo.name.clone();
        let base = repo.default_branch.clone();
        let first = repo.prs.keys().max().copied().unwrap_or(0);
        let now = chrono::Utc::now();
        repo.trees
            .entry("seed-tree".into())
            .or_insert_with(|| json!([]));
        for i in 1..=total {
            let number = first + i;
            let open = i % 10 == 0;
            let merged = !open && i % 3 == 0;
            let foreign = i % 7 == 0;
            let created = now - chrono::Duration::hours((total - i) as i64);
            let (login, head) = if foreign {
                ("octocat", format!("topic/history-{i}"))
            } else {
                (actor.as_str(), format!("patchula/history-{i}"))
            };
            let sha = format!("history-{number}");
            if open {
                repo.refs.insert(format!("refs/heads/{head}"), sha.clone());
                repo.commits.insert(
                    sha.clone(),
                    json!({"sha":sha,"tree":{"sha":"seed-tree"},
                           "message":"seeded history","actor":login}),
                );
            }
            repo.prs.insert(
                number,
                json!({"number":number,"node_id":format!("pr-{number}"),
                    "html_url":format!("https://github.com/{name}/pull/{number}"),
                    "state":if open {"open"} else {"closed"},
                    "title":format!("history {number}"),"body":"",
                    "head":{"ref":head,"sha":sha,"repo":{"full_name":name}},
                    "base":{"ref":base,"repo":{"full_name":name}},
                    "user":{"login":login},"labels":[],"auto_merge":null,
                    "created_at":created.to_rfc3339(),
                    "merged_at":if merged {
                        json!((created + chrono::Duration::hours(1)).to_rfc3339())
                    } else {
                        Value::Null
                    },
                    "draft":false,"mergeable":true,"mergeable_state":"clean"}),
            );
        }
    }
    /// The locked server state: arrange what the server holds and read what
    /// it recorded, in the same shapes the old fakes exposed.
    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }
    /// `(method, path)` for every request, in arrival order.
    pub fn requests(&self) -> Vec<(String, String)> {
        self.state().requests.clone()
    }
    /// `(path, body)` for every mutation request, in arrival order.
    pub fn writes(&self) -> Vec<(String, Value)> {
        self.state().writes.clone()
    }
    /// `(method, path, body)` for every request, in arrival order.
    pub fn log(&self) -> Vec<(String, String, Value)> {
        self.state().log.clone()
    }
    /// ReleaseNotes operation bodies, in arrival order.
    pub fn release_queries(&self) -> Vec<Value> {
        self.state().release_queries.clone()
    }
    /// Requests answered and response bytes served, keyed by
    /// `"METHOD path?query"` — the route counts a listing measurement
    /// reports. Clear through `state().traffic` to measure one run.
    pub fn traffic(&self) -> BTreeMap<String, Traffic> {
        self.state().traffic.clone()
    }
    /// Script a fault against requests matching `matcher`; finish with
    /// [`On::respond`], [`On::then_fail`] or [`On::mutate`].
    pub fn on(&self, matcher: Matcher) -> On<'_> {
        On {
            fake: self,
            matcher,
            after: 0,
            times: None,
        }
    }
    /// The next `POST .../pulls` applies, then answers 500 — GitHub never
    /// told the client whether the PR happened.
    pub fn pull_create_fails(&self) {
        self.on(Matcher::PullCreate {
            repo: self.repo.clone(),
        })
        .times(1)
        .then_fail(500);
    }
    /// `GET .../pulls/{number}` answers 500 without touching state.
    pub fn pull_read_fails(&self, number: u64) {
        self.on(Matcher::PullRead {
            repo: self.repo.clone(),
            number: Some(number),
        })
        .respond(500, json!({}));
    }
    /// `GET .../pulls` answers 500 — the lifecycle listing is down.
    pub fn pulls_list_fails(&self) {
        self.on(Matcher::Route {
            method: Method::GET,
            path: format!("/repos/{}/pulls", self.repo),
        })
        .respond(500, json!({"message":"pulls boom"}));
    }
    /// `PUT .../pulls/{n}/merge` answers `{"merged":false}` — GitHub
    /// declines the merge without telling the client why.
    pub fn pull_merge_refused(&self) {
        self.on(Matcher::PullMerge {
            repo: self.repo.clone(),
            number: None,
        })
        .respond(200, json!({"merged":false,"sha":"merged-base"}));
    }
    /// `updateRefs` never satisfies the lease: GraphQL errors, refs
    /// unchanged.
    pub fn update_refs_fail(&self) {
        self.on(Matcher::Graphql("updateRefs"))
            .respond(200, json!({"errors":[{"message":"reference changed"}]}));
    }
    /// `updateRefs` mutations that delete a ref answer GraphQL errors with
    /// `message`; branch writes still succeed.
    pub fn ref_delete_fails(&self, message: &str) {
        self.on(Matcher::RefDelete)
            .respond(200, json!({"errors":[{"message":message}]}));
    }
    /// `enablePullRequestAutoMerge` answers `UNPROCESSABLE` with `message`,
    /// as GitHub does for a PR it finds mergeable already.
    pub fn auto_merge_refused(&self, message: &str) {
        self.on(Matcher::Graphql("enablePullRequestAutoMerge"))
            .respond(
                200,
                json!({"data":{"enablePullRequestAutoMerge":null},
                       "errors":[{"type":"UNPROCESSABLE","message":message}]}),
            );
    }
    /// The write inventory answers 500 — the scan's one big read is down.
    pub fn inventory_fails(&self) {
        self.on(Matcher::Graphql("associatedPullRequests"))
            .respond(500, json!({"message":"inventory boom"}));
    }
    /// Dashboard issue writes — create and patch — answer 422.
    pub fn issue_writes_fail(&self) {
        self.on(Matcher::Route {
            method: Method::POST,
            path: format!("/repos/{}/issues", self.repo),
        })
        .respond(422, json!({"message":"issue boom"}));
        self.on(Matcher::IssuePatch {
            repo: self.repo.clone(),
            number: None,
        })
        .respond(422, json!({"message":"issue boom"}));
    }
    /// `GET /repos/{repo}` answers 200 with an empty object.
    pub fn repo_read_malformed(&self) {
        self.on(Matcher::RepoRead {
            repo: self.repo.clone(),
        })
        .respond(200, json!({}));
    }
    /// Every pull read finds it closed — its owner closed it mid-run.
    pub fn close_pulls_on_read(&self) {
        self.on(Matcher::PullRead {
            repo: self.repo.clone(),
            number: None,
        })
        .mutate(|state, hit| {
            state
                .repositories
                .get_mut(hit.repo.as_deref().unwrap())
                .unwrap()
                .prs
                .get_mut(&hit.pull.unwrap())
                .unwrap()["state"] = json!("closed");
        });
    }
    /// Pulls read open for `reads` reads, then read back closed.
    pub fn close_pulls_after_reads(&self, reads: usize) {
        self.on(Matcher::PullRead {
            repo: self.repo.clone(),
            number: None,
        })
        .after(reads)
        .mutate(|state, hit| {
            state
                .repositories
                .get_mut(hit.repo.as_deref().unwrap())
                .unwrap()
                .prs
                .get_mut(&hit.pull.unwrap())
                .unwrap()["state"] = json!("closed");
        });
    }
    /// Every pull read reports `sha` as its head — a foreign push.
    pub fn override_pull_head(&self, sha: impl Into<String>) {
        let sha = sha.into();
        self.on(Matcher::PullRead {
            repo: self.repo.clone(),
            number: None,
        })
        .mutate(move |state, hit| {
            state
                .repositories
                .get_mut(hit.repo.as_deref().unwrap())
                .unwrap()
                .prs
                .get_mut(&hit.pull.unwrap())
                .unwrap()["head"]["sha"] = json!(sha);
        });
    }
    /// The checks read finds `sha` on the first pull's head — a human push
    /// landed between the pull read and the checks read.
    pub fn checks_move_head(&self, sha: &str) {
        let sha = sha.to_owned();
        self.on(Matcher::Graphql("statusCheckRollup"))
            .times(1)
            .mutate(move |state, hit| {
                let repo = state
                    .repositories
                    .get_mut(hit.repo.as_deref().unwrap())
                    .unwrap();
                repo.prs.values_mut().next().unwrap()["head"]["sha"] = json!(sha);
            });
    }
    /// Fire `mutate` rules that match `ctx`. Pull reads call this after read
    /// lag, at the point the request sees state; every other route calls it
    /// before its handler.
    fn fire_mutates(&self, ctx: &Ctx, state: &mut State) {
        let mut rules = self.rules.lock().unwrap();
        for rule in rules.iter_mut() {
            if !ctx.matches(&rule.matcher) || !matches!(rule.action, Action::Mutate(_)) {
                continue;
            }
            if !rule.consume() {
                continue;
            }
            let Action::Mutate(change) = &rule.action else {
                continue;
            };
            change(
                state,
                &Hit {
                    method: ctx.method.clone(),
                    path: ctx.path.clone(),
                    repo: ctx.repo.clone(),
                    pull: ctx.pull,
                },
            );
        }
    }
    /// Every answer the resolvers build is the whole object; `prune` then
    /// projects it onto the operation's selection set, so a field the query
    /// does not ask for — or a fragment on another `__typename` — never
    /// reaches the client.
    fn graphql(&self, ctx: &Ctx, body: &Value, state: &mut State) -> Response {
        let mut data = Map::new();
        for op in &ctx.ops {
            let mut fields = Vec::new();
            gather(
                &op.selections,
                if op.mutation { "Mutation" } else { "Query" },
                &mut fields,
            );
            for field in fields {
                let answer = if op.mutation {
                    self.graphql_mutation(&field.name, body, state)
                } else {
                    self.graphql_query(field, body, state)
                };
                match answer {
                    FieldAnswer::Data(value) => {
                        data.insert(field.key.clone(), prune(&value, &field.selections));
                    }
                    FieldAnswer::Errors(errors) => {
                        return response(200, json!({"errors":errors}));
                    }
                }
            }
        }
        response(200, json!({"data":data}))
    }
    fn graphql_query(&self, field: &GqlField, body: &Value, state: &mut State) -> FieldAnswer {
        if field.name == "repository" {
            let name = field
                .arg("owner", body)
                .zip(field.arg("name", body))
                .map(|(owner, name)| format!("{owner}/{name}"));
            if name.is_some_and(|name| {
                state
                    .repositories
                    .get(&name)
                    .is_some_and(|repo| repo.deleted)
            }) {
                return FieldAnswer::Errors(json!([{"type":"NOT_FOUND",
                            "message":"Could not resolve to a Repository."}]));
            }
        }
        FieldAnswer::Data(match field.name.as_str() {
            "viewer" => json!({"__typename":viewer_typename(&state.actor),"login":state.actor}),
            "nodes" => self.closed_prs(body, state),
            "repository" => self.repository(field, body, state),
            name => panic!("unmodeled GraphQL root field {name}"),
        })
    }
    fn closed_prs(&self, body: &Value, state: &State) -> Value {
        let nodes: Vec<_> = state
            .repositories
            .values()
            .flat_map(|repo| repo.prs.values())
            .filter(|p| {
                body["variables"]["closed"]
                    .as_array()
                    .unwrap()
                    .contains(&p["node_id"])
            })
            .map(|p| pull_node(p, state))
            .collect();
        json!(nodes)
    }
    fn repository(&self, field: &GqlField, body: &Value, state: &mut State) -> Value {
        let Some(name) = field
            .arg("owner", body)
            .zip(field.arg("name", body))
            .map(|(owner, name)| format!("{owner}/{name}"))
        else {
            panic!("repository field without owner/name arguments");
        };
        if !state.repositories.contains_key(&name) {
            panic!("unknown repository {name}");
        }
        let repo = &state.repositories[&name];
        let mut answer = Map::new();
        answer.insert("__typename".into(), json!("Repository"));
        let mut release_logged = false;
        for child in field.fields("Repository") {
            let key = child.key.clone();
            let value = match child.name.as_str() {
                "id" => json!(repo.node_id),
                "isPrivate" => json!(repo.private),
                "autoMergeAllowed" => json!(repo.merge.auto_merge),
                "squashMergeAllowed" => json!(repo.merge.squash),
                "mergeCommitAllowed" => json!(repo.merge.merge_commit),
                "rebaseMergeAllowed" => json!(repo.merge.rebase),
                "deleteBranchOnMerge" => json!(repo.merge.delete_branch),
                "defaultBranchRef" => repo
                    .default_branch_ref
                    .clone()
                    .filter(|_| {
                        child.names().iter().any(|name| {
                            ["rules", "refUpdateRule", "history", "requiresStatusChecks"]
                                .contains(&name.as_str())
                        })
                    })
                    .unwrap_or_else(|| {
                        let oid = repo
                            .refs
                            .get(&format!("refs/heads/{}", repo.default_branch))
                            .cloned()
                            .unwrap_or_else(|| repo.base.clone());
                        json!({"__typename":"Ref","name":repo.default_branch,
                           "target":{"__typename":"Commit","oid":oid,
                                     "tree":{"__typename":"Tree",
                                         "oid":repo.commits.get(&oid)
                                             .map(|c| c["tree"]["sha"].clone())
                                             .unwrap_or(Value::Null)}}})
                    }),
                "issues" => self.issues_connection(child, repo, body),
                "refs" => self.refs_connection(child, repo, body, state),
                "pullRequests" => self.pull_requests(child, repo, body, state),
                "release" => {
                    if !release_logged {
                        state.release_queries.push(body.clone());
                        release_logged = true;
                    }
                    let text = repo
                        .release_note_body
                        .clone()
                        .unwrap_or_else(|| "Release details 🦀\n".repeat(10_000));
                    json!({"__typename":"Release","updatedAt":blob_sha(&text),
                           "description":text,"isDraft":false,"isPrerelease":false})
                }
                "releases" => json!({"__typename":"ReleaseConnection",
                        "nodes":repo.releases.iter()
                            .map(|node| with_typename(node, "Release")).collect::<Vec<_>>(),
                        "pageInfo":{"__typename":"PageInfo",
                            "hasNextPage":false,"endCursor":null}}),
                "object" => self.object_field(child, repo, body),
                name => panic!("unmodeled repository field {name}"),
            };
            answer.insert(key, value);
        }
        Value::Object(answer)
    }
    /// `repository { object(oid:|expression:) }`: blobs by content,
    /// commits as `{"oid","__typename":"Commit"}`, and the
    /// `statusCheckRollup` contexts a checks read asks for. An expression
    /// resolves through refs; an oid resolves blobs first, then reads as a
    /// commit.
    fn object_field(&self, field: &GqlField, repo: &Repository, body: &Value) -> Value {
        let names = field.names();
        if let Some(expression) = field.arg("expression", body) {
            return repo
                .refs
                .get(expression)
                .filter(|oid| repo.commits.contains_key(*oid))
                .map(|oid| json!({"oid":oid,"__typename":"Commit"}))
                .unwrap_or(Value::Null);
        }
        let Some(oid) = field.arg("oid", body) else {
            panic!("object field without oid or expression");
        };
        if names.contains("text") {
            if let Some(text) = repo.blobs.get(oid) {
                return json!({"__typename":"Blob","text":text,
                              "isBinary":false,"isTruncated":false});
            }
            if !repo.commits.contains_key(oid) {
                return Value::Null;
            }
        }
        let mut object = json!({"oid":oid,"__typename":"Commit"});
        if names.contains("statusCheckRollup") {
            let offset = body["variables"]["cursor"]
                .as_str()
                .and_then(|c| c.parse::<usize>().ok())
                .unwrap_or(0);
            let next = offset + 100;
            object["statusCheckRollup"] = json!({"__typename":"StatusCheckRollup","contexts":{
                "__typename":"StatusCheckRollupContextConnection",
                "nodes":repo.checks.iter().skip(offset).take(100).cloned().collect::<Vec<_>>(),
                "pageInfo":{"__typename":"PageInfo","hasNextPage":next < repo.checks.len(),
                            "endCursor":next.to_string()}}});
        }
        object
    }
    /// `repository { pullRequests }` — the `PullRequestConnection` the
    /// schema types (`schema.docs.graphql`, `Repository.pullRequests`).
    /// Modeled arguments: `headRefName` (exact head ref filter),
    /// `states` (`OPEN`/`CLOSED`/`MERGED`; `CLOSED` is closed-unmerged),
    /// `first`/`after` (forward paging over an index cursor, as
    /// `refs_connection` pages) and `orderBy` (`CREATED_AT` only — the
    /// sole `IssueOrderField` the fake models — `ASC` or `DESC`; the
    /// connection's natural order is `DESC`). `labels`, `baseRefName`,
    /// `before`, `last` and any other argument are unmodeled and panic,
    /// as does an `orderBy.field` that is not `CREATED_AT`.
    fn pull_requests(
        &self,
        field: &GqlField,
        repo: &Repository,
        body: &Value,
        state: &State,
    ) -> Value {
        for (name, _) in &field.args {
            assert!(
                matches!(
                    name.as_str(),
                    "headRefName" | "states" | "first" | "after" | "orderBy"
                ),
                "unmodeled pullRequests argument {name}"
            );
        }
        // A null or unset variable leaves a nullable argument out; a
        // non-string value is a GitHub validation error, never no filter.
        let string_arg = |name: &str| {
            field.arg_json(name, body).map(|value| {
                value
                    .as_str()
                    .unwrap_or_else(|| panic!("pullRequests `{name}` is not a string: {value}"))
                    .to_owned()
            })
        };
        let head = string_arg("headRefName");
        let states: Option<BTreeSet<String>> = field.arg_json("states", body).map(|states| {
            states
                .as_array()
                .unwrap_or_else(|| panic!("pullRequests `states` is not a list: {states}"))
                .iter()
                .map(|state| {
                    let name = state.as_str().unwrap_or_else(|| {
                        panic!("pullRequests `states` element is not a name: {state}")
                    });
                    // `PullRequestState`: anything else is a validation error.
                    assert!(
                        matches!(name, "OPEN" | "CLOSED" | "MERGED"),
                        "pullRequests `states` has no value {name:?}"
                    );
                    name.to_owned()
                })
                .collect()
        });
        // GitHub refuses a connection without `first`/`last`
        // (MISSING_PAGINATION_BOUNDARIES) and a `first` outside 1-100.
        let first = field
            .arg_json("first", body)
            .map(|first| {
                first
                    .as_u64()
                    .unwrap_or_else(|| panic!("pullRequests `first` is not a count: {first}"))
                    as usize
            })
            .unwrap_or_else(|| panic!("pullRequests without `first`"));
        assert!(
            (1..=100).contains(&first),
            "pullRequests `first` {first} is outside 1-100"
        );
        let after = string_arg("after")
            .map(|cursor| {
                cursor.parse::<usize>().unwrap_or_else(|_| {
                    panic!("pullRequests `after` {cursor:?} is not a fake cursor")
                })
            })
            .unwrap_or(0);
        // `IssueOrder` requires both `field` and `direction`, and GitHub
        // documents no order without `orderBy`, so the fake invents none.
        let order = field
            .arg_json("orderBy", body)
            .unwrap_or_else(|| panic!("pullRequests without `orderBy`"));
        let order = order
            .as_object()
            .unwrap_or_else(|| panic!("pullRequests `orderBy` is not an IssueOrder: {order}"));
        assert!(
            order.keys().all(|key| key == "field" || key == "direction"),
            "pullRequests `orderBy` has an unknown key: {order:?}"
        );
        let order_field = order.get("field").and_then(Value::as_str);
        assert_eq!(
            order_field,
            Some("CREATED_AT"),
            "unmodeled pullRequests `orderBy.field`"
        );
        let descending = match order.get("direction").and_then(Value::as_str) {
            Some("DESC") => true,
            Some("ASC") => false,
            direction => panic!("unmodeled pullRequests `orderBy.direction` {direction:?}"),
        };
        let mut prs: Vec<Value> = repo
            .prs
            .values()
            .filter(|pr| {
                head.as_deref().is_none_or(|head| pr["head"]["ref"] == head)
                    && states
                        .as_ref()
                        .is_none_or(|states| states.contains(pull_state(pr)))
            })
            .cloned()
            .collect();
        prs.sort_by_key(|pr| (pull_created(pr), pr["number"].as_u64().unwrap_or(0)));
        if descending {
            prs.reverse();
        }
        let nodes: Vec<Value> = prs
            .iter()
            .skip(after)
            .take(first)
            .map(|pr| pull_node(pr, state))
            .collect();
        let served = nodes.len();
        let more = after + served < prs.len();
        json!({"__typename":"PullRequestConnection","nodes":nodes,"totalCount":prs.len(),
        "pageInfo":{"__typename":"PageInfo","hasNextPage":more,
            "endCursor":if served > 0 {
                json!((after + served).to_string())
            } else {
                Value::Null
            }}})
    }
    /// `repository { issues }` — the `IssueConnection` the schema types
    /// (`schema.docs.graphql`, `Repository.issues`). Modeled arguments:
    /// `states` (`OPEN`/`CLOSED`), `filterBy` (`createdBy` only — the REST
    /// login, so a `Bot` node matches its `[bot]`-suffixed name) and
    /// `first`/`after` (forward paging over an index cursor, as
    /// `pull_requests` pages). `labels`, `orderBy`, `before`, `last` and
    /// any other argument are unmodeled and panic, as does a `filterBy`
    /// key that is not `createdBy`.
    fn issues_connection(&self, field: &GqlField, repo: &Repository, body: &Value) -> Value {
        for (name, _) in &field.args {
            assert!(
                matches!(name.as_str(), "states" | "filterBy" | "first" | "after"),
                "unmodeled issues argument {name}"
            );
        }
        let states: Option<BTreeSet<String>> = field.arg_json("states", body).map(|states| {
            states
                .as_array()
                .unwrap_or_else(|| panic!("issues `states` is not a list: {states}"))
                .iter()
                .map(|state| {
                    let name = state.as_str().unwrap_or_else(|| {
                        panic!("issues `states` element is not a name: {state}")
                    });
                    // `IssueState`: anything else is a validation error.
                    assert!(
                        matches!(name, "OPEN" | "CLOSED"),
                        "issues `states` has no value {name:?}"
                    );
                    name.to_owned()
                })
                .collect()
        });
        let created_by = field.arg_json("filterBy", body).map(|filter| {
            let filter = filter
                .as_object()
                .unwrap_or_else(|| panic!("issues `filterBy` is not an IssueFilters: {filter}"));
            for key in filter.keys() {
                assert_eq!(key, "createdBy", "unmodeled issues `filterBy` key {key}");
            }
            filter["createdBy"]
                .as_str()
                .unwrap_or_else(|| panic!("issues `filterBy.createdBy` is not a name: {filter:?}"))
                .to_owned()
        });
        // GitHub refuses a connection without `first`/`last` and a `first`
        // outside 1-100.
        let first = field
            .arg_json("first", body)
            .map(|first| {
                first
                    .as_u64()
                    .unwrap_or_else(|| panic!("issues `first` is not a count: {first}"))
                    as usize
            })
            .unwrap_or_else(|| panic!("issues without `first`"));
        assert!(
            (1..=100).contains(&first),
            "issues `first` {first} is outside 1-100"
        );
        let after = field
            .arg("after", body)
            .map(|cursor| {
                cursor
                    .parse::<usize>()
                    .unwrap_or_else(|_| panic!("issues `after` {cursor:?} is not a fake cursor"))
            })
            .unwrap_or(0);
        let issues: Vec<&Value> = repo
            .issues
            .values()
            .filter(|issue| {
                states.as_ref().is_none_or(|states| {
                    states.contains(issue["state"].as_str().unwrap_or_default())
                }) && created_by.as_ref().is_none_or(|login| {
                    issue_author_login(issue).as_deref() == Some(login.as_str())
                })
            })
            .collect();
        let nodes: Vec<Value> = issues
            .iter()
            .skip(after)
            .take(first)
            .map(|issue| with_typename(issue, "Issue"))
            .collect();
        let served = nodes.len();
        let more = after + served < issues.len();
        json!({"__typename":"IssueConnection","nodes":nodes,
        "pageInfo":{"__typename":"PageInfo","hasNextPage":more,
            "endCursor":if served > 0 {
                json!((after + served).to_string())
            } else {
                Value::Null
            }}})
    }
    /// `repository { refs }` — the `RefConnection` the schema types
    /// (`schema.docs.graphql`, `Repository.refs`). Modeled arguments:
    /// `refPrefix` (`String!`; GitHub requires the trailing `/`),
    /// `query` (a case-insensitive substring filter on the ref's short
    /// name — measured: `ENOVATE` matched `macabu/renovate-...` in
    /// grafana/grafana) and `first`/`after` (forward paging over an
    /// index cursor, as `issues_connection` pages). `orderBy`,
    /// `direction`, `before`, `last` and any other argument are
    /// unmodeled and panic.
    fn refs_connection(
        &self,
        field: &GqlField,
        repo: &Repository,
        body: &Value,
        state: &State,
    ) -> Value {
        for (name, _) in &field.args {
            assert!(
                matches!(name.as_str(), "refPrefix" | "query" | "first" | "after"),
                "unmodeled refs argument {name}"
            );
        }
        let prefix = field
            .arg("refPrefix", body)
            .unwrap_or_else(|| panic!("refs without `refPrefix`"));
        assert!(
            prefix.ends_with('/'),
            "refs `refPrefix` {prefix:?} must end in `/`"
        );
        let query = field.arg("query", body).map(str::to_lowercase);
        // GitHub refuses a connection without `first`/`last` and a
        // `first` outside 1-100.
        let first = field
            .arg_json("first", body)
            .map(|first| {
                first
                    .as_u64()
                    .unwrap_or_else(|| panic!("refs `first` is not a count: {first}"))
                    as usize
            })
            .unwrap_or_else(|| panic!("refs without `first`"));
        assert!(
            (1..=100).contains(&first),
            "refs `first` {first} is outside 1-100"
        );
        let after = field
            .arg("after", body)
            .map(|cursor| {
                cursor
                    .parse::<usize>()
                    .unwrap_or_else(|_| panic!("refs `after` {cursor:?} is not a fake cursor"))
            })
            .unwrap_or(0);
        let nodes: Vec<_> = repo
            .refs
            .iter()
            .filter_map(|(name, oid)| Some((name.strip_prefix(prefix)?, oid)))
            .filter(|(name, _)| {
                query
                    .as_ref()
                    .is_none_or(|query| name.to_lowercase().contains(query))
            })
            .map(|(name, oid)| {
                let commit = &repo.commits[oid];
                let prs: Vec<_> = repo
                    .prs
                    .values()
                    .filter(|p| p["state"] == "open" && p["head"]["ref"] == name)
                    .map(|p| pull_node(p, state))
                    .collect();
                json!({"__typename":"Ref","name":name,
                       "target":{"__typename":"Commit","oid":oid,
                          "tree":{"__typename":"Tree","oid":commit["tree"]["sha"]},
                          // `Commit.message` is `String!`.
                          "message":commit["message"].as_str().unwrap_or_default(),
                          "author":{"__typename":"GitActor","user":{"login":commit.get("actor").and_then(Value::as_str).unwrap_or("patchula[bot]")}},
                          "committer":{"__typename":"GitActor","user":{"login":"web-flow"}},
                          // `Commit.signature` is the `GitSignature`
                          // interface; `GpgSignature` carries `isValid`.
                          "signature":{"__typename":"GpgSignature","isValid":!state.settings.unsigned}},
                       "associatedPullRequests":{"__typename":"PullRequestConnection","nodes":prs,
                          "pageInfo":{"__typename":"PageInfo","hasNextPage":state.settings.pr_next_page.as_deref() == Some(name)}}})
            })
            .collect();
        // `ref_page_size` simulates GitHub serving a shorter page than
        // `first` asked for, never a longer one.
        let size = state.settings.ref_page_size.unwrap_or(first).min(first);
        let more = after + size < nodes.len();
        let nodes: Vec<_> = nodes.into_iter().skip(after).take(size).collect();
        json!({"__typename":"RefConnection","nodes":nodes,
               "pageInfo":{"__typename":"PageInfo","hasNextPage":more,"endCursor":(after+size).to_string()}})
    }
    fn graphql_mutation(&self, name: &str, body: &Value, state: &mut State) -> FieldAnswer {
        match name {
            "updateRefs" => self.update_refs(body, state),
            "enablePullRequestAutoMerge" | "disablePullRequestAutoMerge" => {
                let enabled = name.starts_with("enable");
                let input = &body["variables"]["input"];
                let id = input["pullRequestId"].as_str().unwrap_or_default();
                let Some(pr) = state
                    .repositories
                    .values_mut()
                    .find_map(|repo| repo.prs.values_mut().find(|p| p["node_id"] == id))
                else {
                    panic!("auto-merge on unknown pull request {id}");
                };
                if !input["expectedHeadOid"].is_null()
                    && input["expectedHeadOid"] != pr["head"]["sha"]
                {
                    return FieldAnswer::Errors(
                        json!([{"message":"expectedHeadOid does not match the pull request head"}]),
                    );
                }
                pr["auto_merge"] = if enabled {
                    json!({"enabledAt":chrono::Utc::now().to_rfc3339()})
                } else {
                    Value::Null
                };
                let payload = if enabled {
                    "EnablePullRequestAutoMergePayload"
                } else {
                    "DisablePullRequestAutoMergePayload"
                };
                FieldAnswer::Data(json!({"__typename":payload,
                    "pullRequest":{"__typename":"PullRequest","id":id}}))
            }
            _ => panic!("unmodeled GraphQL mutation {name}"),
        }
    }
    fn update_refs(&self, body: &Value, state: &mut State) -> FieldAnswer {
        let input = &body["variables"]["input"];
        let Some(repo) = state
            .repositories
            .values_mut()
            .find(|repo| repo.node_id == input["repositoryId"])
        else {
            panic!("updateRefs to unknown repository {}", input["repositoryId"])
        };
        let updates = input["refUpdates"].as_array().unwrap();
        let zero = "0000000000000000000000000000000000000000";
        let valid = updates.iter().all(|r| {
            repo.refs
                .get(r["name"].as_str().unwrap())
                .map_or(zero, String::as_str)
                == r["beforeOid"].as_str().unwrap()
        });
        if !valid {
            return FieldAnswer::Errors(json!([{"message":"reference changed"}]));
        }
        for r in updates {
            if r["afterOid"] == zero {
                repo.refs.remove(r["name"].as_str().unwrap());
            } else {
                repo.refs.insert(
                    r["name"].as_str().unwrap().into(),
                    r["afterOid"].as_str().unwrap().into(),
                );
            }
        }
        let lag = state.settings.pr_lag_reads > 0;
        let mut pending = BTreeMap::new();
        for pr in repo.prs.values_mut() {
            for r in updates {
                if r["name"] == format!("refs/heads/{}", pr["head"]["ref"].as_str().unwrap()) {
                    if lag {
                        pending.insert(pr["number"].as_u64().unwrap(), r["afterOid"].clone());
                    } else {
                        pr["head"]["sha"].clone_from(&r["afterOid"]);
                    }
                }
            }
        }
        repo.pending_heads.extend(pending);
        FieldAnswer::Data(json!({"__typename":"UpdateRefsPayload","clientMutationId":null}))
    }
    fn create_pull(&self, body: &Value, repo: &mut Repository, actor: &str) -> Response {
        let number = repo.prs.len() as u64 + 1;
        let name = repo.name.clone();
        let sha = repo.refs[&format!("refs/heads/{}", body["head"].as_str().unwrap())].clone();
        let mut pr = json!({"number":number,"node_id":format!("pr-{number}"),"html_url":format!("https://github.com/{name}/pull/{number}"),"state":"open","title":body["title"],"body":body["body"],"head":{"ref":body["head"],"sha":sha,"repo":{"full_name":name}},"base":{"ref":body["base"],"repo":{"full_name":name}},"user":{"login":actor},"labels":[],"auto_merge":null,"merged_at":null,"created_at":chrono::Utc::now().to_rfc3339(),"draft":false,"mergeable":true,"mergeable_state":"clean"});
        pr["head"]["label"] = json!("demo:branch");
        pr["head"]["repo"]["updated_at"] = json!("2026-10-15T00:00:00Z");
        pr["base"]["sha"] = json!(repo.base);
        pr["base"]["repo"]["size"] = json!(42);
        repo.prs.insert(number, pr.clone());
        response(201, pr)
    }
    fn create_issue(&self, body: &Value, repo: &mut Repository, actor: &str) -> Response {
        let number = 100 + repo.issues.len() as u64;
        let author = if let Some(bot) = actor.strip_suffix("[bot]") {
            json!({"__typename":"Bot","login":bot})
        } else {
            json!({"__typename":"User","login":actor})
        };
        let issue = json!({"number":number,"id":format!("dashboard-{number}"),"title":body["title"],"body":body["body"],"state":"OPEN","author":author});
        repo.issues.insert(number, issue.clone());
        response(201, issue)
    }
    fn create_tree(&self, body: &Value, repo: &mut Repository) -> Response {
        let mut tree = repo.trees[body["base_tree"].as_str().unwrap()].clone();
        for original in body["tree"].as_array().unwrap() {
            let mut entry = original.clone();
            if let Some(content) = entry["content"].as_str().map(str::to_owned) {
                let sha = blob_sha(&content);
                entry["sha"] = json!(sha);
                repo.blobs.insert(sha, content);
                entry.as_object_mut().unwrap().remove("content");
            }
            tree.as_array_mut()
                .unwrap()
                .retain(|e| e["path"] != entry["path"]);
            if !(entry.get("sha").is_some_and(Value::is_null) && entry.get("content").is_none()) {
                tree.as_array_mut().unwrap().push(entry.clone());
            }
        }
        let sha = format!("tree-{}", repo.trees.len());
        repo.trees.insert(sha.clone(), tree);
        response(201, json!({"sha":sha}))
    }
    fn create_commit(&self, body: &Value, repo: &mut Repository, unsigned: bool) -> Response {
        assert!(
            body.get("author").is_none()
                && body.get("committer").is_none()
                && body.get("signature").is_none()
        );
        assert_eq!(body["parents"].as_array().unwrap().len(), 1);
        assert_eq!(body["parents"][0], repo.base);
        let sha = format!("commit-{}", repo.commits.len());
        let commit = json!({"sha":sha,"tree":{"sha":body["tree"]},"message":body["message"],"parents":[{"sha":body["parents"][0]}],"verification":{"verified":!unsigned}});
        repo.commits.insert(sha.clone(), commit.clone());
        response(201, commit)
    }
    fn patch_pull(&self, rest: &str, body: &Value, repo: &mut Repository) -> Response {
        let n: u64 = rest.rsplit('/').next().unwrap().parse().unwrap();
        let pr = repo.prs.get_mut(&n).unwrap();
        for (key, value) in body.as_object().unwrap() {
            if key == "base" {
                pr["base"]["ref"].clone_from(value);
            } else {
                pr[key].clone_from(value);
            }
        }
        response(200, pr.clone())
    }
    fn patch_issue(&self, n: u64, body: &Value, repo: &mut Repository) -> Response {
        let issue = repo.issues.get_mut(&n).unwrap();
        if let Some(text) = body.get("body") {
            issue["body"].clone_from(text);
        }
        if body["state"] == "closed" {
            issue["state"] = json!("CLOSED");
        }
        response(200, issue.clone())
    }
    fn merge_pull(&self, n: u64, body: &Value, repo: &mut Repository, actor: &str) -> Response {
        let pr = repo.prs.get_mut(&n).unwrap();
        assert_eq!(body["sha"], pr["head"]["sha"], "merging a stale head");
        assert_eq!(pr["user"]["login"], actor, "merging a foreign pull request");
        assert_eq!(
            pr["mergeable_state"], "clean",
            "merging a pull request GitHub does not find clean"
        );
        let head = pr["head"]["ref"].as_str().unwrap().to_owned();
        pr["state"] = json!("closed");
        pr["merged_at"] = json!(chrono::Utc::now().to_rfc3339());
        if repo.merge.delete_branch {
            repo.refs.remove(&format!("refs/heads/{head}"));
        }
        response(200, json!({"merged":true,"sha":"merged-base"}))
    }
    fn put_labels(&self, rest: &str, body: &Value, repo: &mut Repository) -> Response {
        let n: u64 = rest.split('/').nth(1).unwrap().parse().unwrap();
        let labels: Vec<_> = body["labels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| json!({"name":l}))
            .collect();
        repo.prs.get_mut(&n).unwrap()["labels"] = json!(labels);
        response(200, json!(labels))
    }
    /// `GET /repos/{repo}/pulls`, per GitHub's REST docs
    /// (<https://docs.github.com/en/rest/pulls/pulls#list-pull-requests>):
    /// `state` is `open` (the default), `closed` or `all` — `closed`
    /// covers merged pulls, whose REST state is `closed`; `sort` is
    /// `created` (the default, and the only sort the fake models);
    /// `direction` is `desc` (the `created` default) or `asc`;
    /// `per_page` defaults to 30 and caps at 100; `page` defaults to 1.
    /// The `head`/`base` filters and every other parameter are unmodeled
    /// and panic, so a caller the fake cannot answer truthfully fails
    /// loudly instead of reading a plausible wrong listing.
    fn list_pulls(
        &self,
        request: &Request,
        repo: &Repository,
        page_size: Option<usize>,
    ) -> Response {
        let mut state = "open".to_owned();
        let mut descending = true;
        let mut per_page = 30_usize;
        let mut page = 1_usize;
        for (key, value) in request.url().query_pairs() {
            match key.as_ref() {
                "state" => match value.as_ref() {
                    "open" | "closed" | "all" => state = value.into_owned(),
                    other => panic!("unmodeled pulls `state` {other:?}"),
                },
                "sort" => assert_eq!(value, "created", "unmodeled pulls `sort`"),
                "direction" => match value.as_ref() {
                    "desc" => descending = true,
                    "asc" => descending = false,
                    other => panic!("unmodeled pulls `direction` {other:?}"),
                },
                "per_page" => {
                    per_page = value
                        .parse::<usize>()
                        .ok()
                        .filter(|size| *size > 0)
                        .unwrap_or_else(|| panic!("unmodeled pulls `per_page` {value:?}"));
                }
                "page" => {
                    page = value
                        .parse::<usize>()
                        .ok()
                        .filter(|page| *page > 0)
                        .unwrap_or_else(|| panic!("unmodeled pulls `page` {value:?}"));
                }
                key => panic!("unmodeled pulls query parameter {key:?}"),
            }
        }
        let size = page_size.unwrap_or(per_page.min(100));
        let mut prs: Vec<Value> = repo
            .prs
            .values()
            .filter(|pr| match state.as_str() {
                "open" => pr["state"] == "open",
                "closed" => pr["state"] == "closed",
                _ => true,
            })
            .cloned()
            .collect();
        // `sort=created`: GitHub orders by `created_at`; the pull number
        // breaks ties the way it does for same-second creations.
        prs.sort_by_key(|pr| (pull_created(pr), pr["number"].as_u64().unwrap_or(0)));
        if descending {
            prs.reverse();
        }
        let total = prs.len();
        let prs: Vec<Value> = prs.into_iter().skip((page - 1) * size).take(size).collect();
        let mut reply = response(200, json!(prs));
        if page * size < total {
            // GitHub's `next` link repeats the request's whole query with
            // only `page` advanced; the client follows it verbatim.
            let mut next = request.url().clone();
            let query: Vec<(String, String)> = request
                .url()
                .query_pairs()
                .filter(|(key, _)| key != "page")
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .chain([("page".to_owned(), (page + 1).to_string())])
                .collect();
            next.query_pairs_mut().clear().extend_pairs(query);
            reply
                .headers
                .insert("link", format!("<{next}>; rel=\"next\"").parse().unwrap());
        }
        reply
    }
    fn pull_read(&self, ctx: &Ctx, full: &str, state: &mut State) -> Response {
        let n = ctx.pull.unwrap();
        if state.settings.pr_lag_reads > 0 {
            state.settings.pr_lag_reads -= 1;
        } else {
            let head = state
                .repositories
                .get_mut(full)
                .unwrap()
                .pending_heads
                .remove(&n);
            if let Some(head) = head {
                state
                    .repositories
                    .get_mut(full)
                    .unwrap()
                    .prs
                    .get_mut(&n)
                    .unwrap()["head"]["sha"] = head;
            }
        }
        // `mutate` rules fire after read lag: the request sees state at its
        // own point.
        self.fire_mutates(ctx, state);
        response(200, state.repositories[full].prs[&n].clone())
    }
    /// The App surface: installation listing, installation lookup and
    /// per-installation access tokens.
    fn app_routes(&self, path: &str, method: &Method, state: &mut State) -> Option<Response> {
        if path == "/app/installations" && *method == Method::GET {
            let installations: Vec<_> = state
                .installations
                .iter()
                .map(|id| json!({"id":id,"suspended_at":null}))
                .collect();
            return Some(response(200, json!(installations)));
        }
        if let Some(id) = path
            .strip_prefix("/app/installations/")
            .and_then(|rest| rest.strip_suffix("/access_tokens"))
            && *method == Method::POST
        {
            let _: u64 = id.parse().unwrap();
            state.minted += 1;
            let expires = chrono::Utc::now() + chrono::Duration::hours(1);
            return Some(response(
                201,
                json!({"token":format!("installation-token-{}",state.minted),
                       "expires_at":expires.to_rfc3339()}),
            ));
        }
        if let Some(spec) = path
            .strip_prefix("/repos/")
            .and_then(|rest| rest.strip_suffix("/installation"))
        {
            let found = state.repositories.contains_key(spec)
                && state.repositories[spec].installed
                && !state.repositories[spec].deleted;
            return Some(if found && let Some(id) = state.installations.first() {
                response(200, json!({"id":id}))
            } else {
                response(404, json!({"message":"Not Found"}))
            });
        }
        if let Some(org) = path
            .strip_prefix("/orgs/")
            .and_then(|rest| rest.strip_suffix("/installation"))
        {
            return Some(
                if !org.is_empty()
                    && let Some(id) = state.installations.first()
                {
                    response(200, json!({"id":id}))
                } else {
                    response(404, json!({"message":"Not Found"}))
                },
            );
        }
        None
    }
    /// The repository discovery listings: `user/repos` for token callers
    /// and `installation/repositories` for installations.
    fn discovery(&self, path: &str, state: &State) -> Option<Response> {
        if let Some(query) = path.strip_prefix("/user/repos") {
            let _ = query;
            let repos: Vec<_> = state
                .repositories
                .values()
                .filter(|repo| repo.installed)
                .map(|repo| json!({"full_name":repo.name,"private":repo.private}))
                .collect();
            return Some(response(200, json!(repos)));
        }
        if path.starts_with("/installation/repositories") {
            let repos: Vec<_> = state
                .repositories
                .values()
                .filter(|repo| repo.installed)
                .map(|repo| json!({"full_name":repo.name,"private":repo.private}))
                .collect();
            return Some(response(
                200,
                json!({"repositories":repos,"total_count":repos.len()}),
            ));
        }
        None
    }
    fn dispatch(&self, request: &Request, body: &Value, ctx: &Ctx, state: &mut State) -> Response {
        let path = ctx.path.as_str();
        if path == "/graphql" {
            return self.graphql(ctx, body, state);
        }
        if let Some(reply) = self.app_routes(path, &ctx.method, state) {
            return reply;
        }
        if ctx.method == Method::GET
            && let Some(reply) = self.discovery(path, state)
        {
            return reply;
        }
        if let Some(doc) = path.strip_prefix("/data/github/") {
            let Some((repo, kind)) = doc.rsplit_once('/') else {
                panic!("unexpected read {path}")
            };
            let Some(upstream) = state.repositories.get(repo) else {
                panic!("unexpected read {path}")
            };
            return response(
                200,
                match kind {
                    "releases.json" => upstream.mirror_releases.clone(),
                    "tags.json" => upstream.mirror_tags.clone(),
                    _ => panic!("unexpected read {path}"),
                },
            );
        }
        let Some(rest) = path.strip_prefix("/repos/") else {
            panic!("unexpected read {path}")
        };
        let (owner, rest) = rest.split_once('/').unwrap();
        let (name, rest) = rest.split_once('/').unwrap_or((rest, ""));
        let full = format!("{owner}/{name}");
        if !state.repositories.contains_key(&full) {
            panic!("unexpected read {path}")
        }
        if state.repositories[&full].deleted {
            return response(404, json!({"message":"Not Found"}));
        }
        // Pull reads take the whole state — read lag and `mutate` rules run
        // at the request's point.
        if ctx.method == Method::GET && rest.starts_with("pulls/") {
            return self.pull_read(ctx, &full, state);
        }
        let page_size = state.settings.pr_page_size;
        let truncated = state.settings.truncated;
        let unsigned = state.settings.unsigned;
        let actor = state.actor.clone();
        let repo = state.repositories.get_mut(&full).unwrap();
        if ctx.method == Method::POST {
            match rest {
                "pulls" => self.create_pull(body, repo, &actor),
                "issues" => self.create_issue(body, repo, &actor),
                rest if rest.ends_with("/comments") && rest.starts_with("issues/") => {
                    response(201, json!({"id":1,"body":body["body"]}))
                }
                "git/blobs" => {
                    assert_eq!(body["encoding"], "utf-8");
                    let content = body["content"].as_str().unwrap();
                    let sha = blob_sha(content);
                    repo.blobs.insert(sha.clone(), content.to_owned());
                    response(201, json!({"sha":sha}))
                }
                "git/trees" => self.create_tree(body, repo),
                "git/commits" => self.create_commit(body, repo, unsigned),
                _ => panic!("unexpected write {path}"),
            }
        } else if ctx.method == Method::PUT && rest.ends_with("/merge") {
            let n: u64 = rest
                .trim_start_matches("pulls/")
                .trim_end_matches("/merge")
                .parse()
                .unwrap();
            self.merge_pull(n, body, repo, &actor)
        } else if ctx.method == Method::PATCH && rest.starts_with("pulls/") {
            self.patch_pull(rest, body, repo)
        } else if ctx.method == Method::PATCH && rest.starts_with("issues/") {
            self.patch_issue(ctx.issue_patch.unwrap(), body, repo)
        } else if ctx.method == Method::PUT && rest.ends_with("/labels") {
            self.put_labels(rest, body, repo)
        } else if ctx.method == Method::GET {
            if rest == "pulls" {
                self.list_pulls(request, repo, page_size)
            } else if rest == "branches" {
                let branches: Vec<_> = repo
                    .refs
                    .iter()
                    .filter_map(|(n, sha)| {
                        n.strip_prefix("refs/heads/")
                            .map(|name| json!({"name":name,"commit":{"sha":sha}}))
                    })
                    .collect();
                response(200, json!(branches))
            } else if rest == format!("branches/{}", repo.default_branch) {
                let oid = repo.refs[&format!("refs/heads/{}", repo.default_branch)].clone();
                let commit = repo.commits.get(&oid).cloned().unwrap_or(json!({}));
                response(
                    200,
                    json!({"name":repo.default_branch,
                           "commit":{"sha":oid,"commit":commit_answer(&oid, &commit)}}),
                )
            } else if let Some(reference) = rest.strip_prefix("commits/") {
                let oid = repo
                    .refs
                    .get(&format!("refs/heads/{reference}"))
                    .or_else(|| repo.refs.get(&format!("refs/tags/{reference}")))
                    .map_or(reference, String::as_str);
                match repo.commits.get(oid) {
                    Some(commit) => response(
                        200,
                        json!({"sha":oid,
                        "commit":commit_answer(oid, commit),
                        "author":commit_author(commit, "actor", "patchula[bot]"),
                        "committer":commit_author(commit, "committer_login", "web-flow")}),
                    ),
                    None => response(404, json!({"message":"not found"})),
                }
            } else if rest.is_empty() {
                response(
                    200,
                    json!({"node_id":repo.node_id,"default_branch":repo.default_branch,
                           "private":repo.private,
                           "allow_auto_merge":repo.merge.auto_merge,
                           "allow_squash_merge":repo.merge.squash,
                           "allow_merge_commit":repo.merge.merge_commit,
                           "allow_rebase_merge":repo.merge.rebase,
                           "delete_branch_on_merge":repo.merge.delete_branch}),
                )
            } else if let Some(branch) = rest.strip_prefix("git/ref/heads/") {
                // url::Url keeps percent-encoded slashes in a path segment.
                let branch = branch.replace("%2F", "/");
                match repo.refs.get(&format!("refs/heads/{branch}")) {
                    Some(sha) => response(200, json!({"object":{"sha":sha}})),
                    None => response(404, json!({})),
                }
            } else if let Some(sha) = rest.strip_prefix("git/commits/") {
                match repo.commits.get(sha) {
                    Some(commit) => response(200, commit_answer(sha, commit)),
                    None => response(404, json!({"message":"not found"})),
                }
            } else if let Some(sha) = rest.strip_prefix("git/trees/") {
                match repo.trees.get(sha) {
                    Some(tree) => response(200, json!({"tree":tree,"truncated":truncated})),
                    None => response(404, json!({"message":"not found"})),
                }
            } else if let Some(sha) = rest.strip_prefix("git/blobs/") {
                match repo.blobs.get(sha) {
                    Some(content) => response(
                        200,
                        json!({"encoding":"base64","content":STANDARD.encode(content)}),
                    ),
                    None => response(404, json!({"message":"not found"})),
                }
            } else if let Some(matching) = rest.strip_prefix("git/matching-refs/") {
                let prefix = format!("refs/{matching}");
                let refs: Vec<_> = repo
                    .refs
                    .iter()
                    .filter(|(name, _)| name.starts_with(&prefix))
                    .map(|(name, oid)| json!({"ref":name,"object":{"sha":oid,"type":"commit"}}))
                    .collect();
                let etag = format!("tags-{}", etag(&refs));
                if request
                    .headers()
                    .get("if-none-match")
                    .is_some_and(|value| value == &etag)
                {
                    return response(304, Value::Null);
                }
                let mut reply = response(200, json!(refs));
                reply.headers.insert("etag", etag.parse().unwrap());
                reply
            } else if rest == "tags" {
                let tags: Vec<_> = repo
                    .refs
                    .iter()
                    .filter_map(|(name, oid)| {
                        name.strip_prefix("refs/tags/")
                            .map(|tag| json!({"name":tag,"commit":{"sha":oid}}))
                    })
                    .collect();
                let etag = format!("tags-{}", etag(&tags));
                if request
                    .headers()
                    .get("if-none-match")
                    .is_some_and(|value| value == &etag)
                {
                    return response(304, Value::Null);
                }
                let mut reply = response(200, json!(tags));
                reply.headers.insert("etag", etag.parse().unwrap());
                reply
            } else if rest == "contents" {
                match &repo.contents {
                    Some(contents) => response(200, contents.clone()),
                    None => response(404, json!({"message":"not found"})),
                }
            } else if rest == "releases" {
                let nodes: Vec<_> = repo
                    .releases
                    .iter()
                    .map(|node| {
                        json!({"tag_name":node["tagName"],"published_at":node["publishedAt"],
                               "draft":node["isDraft"],"prerelease":node["isPrerelease"]})
                    })
                    .collect();
                response(200, json!(nodes))
            } else if let Some(_tag) = rest.strip_prefix("releases/tags/") {
                if !repo.release_tags {
                    response(404, json!({"message":"not found"}))
                } else {
                    let etag = repo.release_note_body.as_ref().map_or_else(
                        || "\"release-one\"".into(),
                        |text| format!("\"{}\"", blob_sha(text)),
                    );
                    let mut reply = if request
                        .headers()
                        .get("if-none-match")
                        .is_some_and(|value| value == etag.as_str())
                    {
                        response(304, Value::Null)
                    } else {
                        response(
                            200,
                            json!({"body":repo.release_note_body.clone().unwrap_or_else(|| {
                                "Release details 🦀\n".repeat(10_000)
                            })}),
                        )
                    };
                    reply.headers.insert("etag", etag.parse().unwrap());
                    reply
                }
            } else if rest.starts_with("compare/unknown...") {
                response(404, json!({"message":"Not Found"}))
            } else if let Some(spec) = rest.strip_prefix("compare/") {
                let sha = spec.split("...").nth(1).unwrap().split('?').next().unwrap();
                let history: Vec<_> = repo
                    .history
                    .iter()
                    .map(String::as_str)
                    .chain(std::iter::once(sha))
                    .collect();
                let commits: Vec<_> = if spec.starts_with(&format!("{}...", repo.base))
                    && repo.merged_heads.contains(sha)
                {
                    Vec::new()
                } else {
                    history
                        .iter()
                        .map(|sha| {
                            let commit = &repo.commits[*sha];
                            json!({"sha":sha,"author":commit_author(commit, "actor", "patchula[bot]"),"committer":commit_author(commit, "committer_login", "web-flow"),"commit":commit_answer(sha, commit)})
                        })
                        .collect()
                };
                response(
                    200,
                    json!({"total_commits":commits.len(),"commits":commits}),
                )
            } else {
                panic!("unexpected read {path}")
            }
        } else {
            panic!("unexpected write {path}")
        }
    }
}

impl Transport for FakeGitHub {
    fn send(&self, request: Request) -> Exchange<'_> {
        if request.url().host_str() == Some("api.github.com") {
            assert!(
                request.headers()["authorization"].is_sensitive(),
                "github requests keep their bearer token out of logs"
            );
        } else {
            assert!(
                !request.headers().contains_key("authorization"),
                "authorization never leaves api.github.com"
            );
        }
        let path = request.url().path().to_owned();
        let body: Value = request
            .body()
            .and_then(|b| b.as_bytes())
            .and_then(|b| serde_json::from_slice(b).ok())
            .unwrap_or(Value::Null);
        let mut state = self.state.lock().unwrap();
        let latency = state.settings.latency;
        state
            .requests
            .push((request.method().to_string(), path.clone()));
        state
            .log
            .push((request.method().to_string(), path.clone(), body.clone()));
        transcripts::record(&request, &body);
        let ctx = Ctx::new(&request, &path, &body);
        if ctx.is_write() {
            state.writes.push((path.clone(), body.clone()));
        }
        // `respond` answers before the handler — the request never happened.
        // `then_fail` arms now and swaps the status after it.
        let mut answer = None;
        let mut then_fail = None;
        {
            let mut rules = self.rules.lock().unwrap();
            for rule in rules.iter_mut() {
                if !ctx.matches(&rule.matcher) {
                    continue;
                }
                if matches!(rule.action, Action::Mutate(_)) {
                    // `mutate` fires at the route's own point, inside the
                    // handler.
                    continue;
                }
                if !rule.consume() {
                    continue;
                }
                match &rule.action {
                    Action::Respond(status, body) => {
                        answer = Some((*status, body.clone()));
                        break;
                    }
                    Action::ThenFail(status) => then_fail = Some(*status),
                    Action::Mutate(_) => {}
                }
            }
        }
        let reply = match answer {
            Some((status, body)) => response(status, body),
            None => {
                if !ctx.pull_read {
                    self.fire_mutates(&ctx, &mut state);
                }
                let mut reply = self.dispatch(&request, &body, &ctx, &mut state);
                if let Some(status) = then_fail {
                    reply.status = StatusCode::from_u16(status).unwrap();
                }
                reply
            }
        };
        let route = format!(
            "{} {}{}",
            request.method(),
            path,
            request
                .url()
                .query()
                .map(|query| format!("?{query}"))
                .unwrap_or_default()
        );
        let route = state.traffic.entry(route).or_default();
        route.requests += 1;
        route.bytes += reply.body.len();
        Box::pin(async move {
            if !latency.is_zero() {
                tokio::time::sleep(latency).await;
            }
            Ok(reply)
        })
    }
}

impl Drop for FakeGitHub {
    fn drop(&mut self) {
        if !self.transcript_checked.load(Ordering::Relaxed) && !std::thread::panicking() {
            panic!(
                "`{}` dropped a FakeGitHub without checking its request transcript",
                std::thread::current().name().unwrap_or("<unnamed>")
            );
        }
    }
}

type Script = dyn Fn(&Request, &Value) -> Response + Send + Sync;

/// A host router: `api.github.com` requests reach the [`FakeGitHub`];
/// every other host needs a per-test scripted route, so suites can compose
/// GitHub with registries, the versions host or OSV. Every request joins
/// the router's log — GitHub's share records inside the fake again — and
/// scripted hosts join the checked transcript like GitHub's requests.
pub struct Hosts {
    github: Arc<FakeGitHub>,
    routes: Mutex<BTreeMap<String, Arc<Script>>>,
    log: Mutex<Vec<(String, String, Value)>>,
    /// The scripted routes' share of the latency `set_latency` charges;
    /// the fake's own answers charge `settings.latency`, which
    /// `set_latency` writes too, so one knob times a whole run.
    latency: Mutex<Duration>,
}

impl Hosts {
    /// A router fronting `github`.
    pub fn new(github: Arc<FakeGitHub>) -> Self {
        Self {
            github,
            routes: Mutex::new(BTreeMap::new()),
            log: Mutex::new(Vec::new()),
            latency: Mutex::new(Duration::ZERO),
        }
    }
    /// Charge every request `latency`, slept on Tokio's paused clock:
    /// GitHub's answers inside the fake and the scripted routes' alike.
    /// A run's virtual elapsed time divided by this latency is its depth —
    /// how many round trips lie on its critical path.
    pub fn set_latency(&self, latency: Duration) {
        self.github.state().settings.latency = latency;
        *self.latency.lock().unwrap() = latency;
    }
    /// The GitHub the router fronts — state and fault rules as usual.
    pub fn github(&self) -> &FakeGitHub {
        &self.github
    }
    /// Route every request for `host` through `answer`: the request and
    /// its decoded JSON body in, the response out.
    pub fn script(
        &self,
        host: &str,
        answer: impl Fn(&Request, &Value) -> Response + Send + Sync + 'static,
    ) {
        self.routes
            .lock()
            .unwrap()
            .insert(host.into(), Arc::new(answer));
    }
    /// `(method, path, body)` for every request the router answered, in
    /// arrival order, GitHub and scripted hosts together.
    pub fn log(&self) -> Vec<(String, String, Value)> {
        self.log.lock().unwrap().clone()
    }
    /// Forget every recorded request.
    pub fn clear(&self) {
        self.log.lock().unwrap().clear();
    }
}

impl Transport for Hosts {
    fn send(&self, request: Request) -> Exchange<'_> {
        let host = request.url().host_str().unwrap_or_default().to_owned();
        let path = request.url().path().to_owned();
        let body: Value = request
            .body()
            .and_then(|b| b.as_bytes())
            .and_then(|b| serde_json::from_slice(b).ok())
            .unwrap_or(Value::Null);
        self.log
            .lock()
            .unwrap()
            .push((request.method().to_string(), path.clone(), body.clone()));
        if host == "api.github.com" {
            return self.github.send(request);
        }
        assert!(
            !request.headers().contains_key("authorization"),
            "authorization never leaves api.github.com"
        );
        transcripts::record(&request, &body);
        let route = self
            .routes
            .lock()
            .unwrap()
            .get(&host)
            .cloned()
            .unwrap_or_else(|| panic!("unscripted host {host} {path}"));
        let latency = *self.latency.lock().unwrap();
        let reply = route(&request, &body);
        Box::pin(async move {
            if !latency.is_zero() {
                tokio::time::sleep(latency).await;
            }
            Ok(reply)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphql_parser::query::{Definition, OperationDefinition, parse_query};

    /// The operation's root selections, parsed the way `Ctx::new` does.
    fn select(query: &str) -> Vec<GqlSelection> {
        let doc = parse_query::<&str>(query).unwrap();
        selections(match &doc.definitions[0] {
            Definition::Operation(OperationDefinition::Query(op)) => &op.selection_set,
            Definition::Operation(OperationDefinition::SelectionSet(set)) => set,
            _ => panic!("test queries only"),
        })
    }

    /// The subtree of a query's single root field.
    fn below(query: &str) -> Vec<GqlSelection> {
        let mut root = select(query);
        let Some(GqlSelection::Field(field)) = root.pop() else {
            panic!("one root field");
        };
        field.selections
    }

    #[test]
    fn unselected_fields_leave_the_answer() {
        let answer = json!({"name":"main","protected":true});
        assert_eq!(prune(&answer, &select("{ name }")), json!({"name":"main"}));
        // An object with nothing selected prunes to nothing.
        assert_eq!(prune(&answer, &[]), json!({}));
    }

    #[test]
    fn a_field_answers_under_its_alias() {
        let answer = json!({"name":"main","target":{"oid":"abc","extra":1}});
        let pruned = prune(
            &answer,
            &select("{ renamed: name target { oid } leaf: target { oid } }"),
        );
        assert_eq!(pruned["renamed"], "main");
        assert_eq!(pruned["target"], json!({"oid":"abc"}));
        assert_eq!(pruned["leaf"], json!({"oid":"abc"}));
        // The alias still reads the named field; the schema name stays absent.
        assert!(pruned.get("name").is_none());
    }

    #[test]
    fn aliased_siblings_each_find_their_own_answer() {
        // Resolvers answering per selection (different arguments) key by the
        // response key; pruning must not cross them.
        let answer = json!({"first":{"text":"one"},"second":{"text":"two"}});
        assert_eq!(
            prune(
                &answer,
                &select("{ first: object { text } second: object { text } }"),
            ),
            json!({"first":{"text":"one"},"second":{"text":"two"}})
        );
    }

    #[test]
    fn inline_fragments_apply_only_on_the_matching_typename() {
        let selections =
            select("{ __typename ... on Commit { oid } ... on Blob { text isBinary } }");
        let commit = json!({"__typename":"Commit","oid":"abc","text":"x"});
        assert_eq!(
            prune(&commit, &selections),
            json!({"__typename":"Commit","oid":"abc"})
        );
        // A fragment whose condition does not match contributes nothing:
        // the Blob keeps `text`, loses `oid`, and the unproduced `isBinary`
        // stays absent.
        let blob = json!({"__typename":"Blob","text":"x","oid":"ignored"});
        assert_eq!(
            prune(&blob, &selections),
            json!({"__typename":"Blob","text":"x"})
        );
    }

    /// A node the fake forgot to type would lose every fragment field and
    /// read as "no data", so the pruner refuses it instead.
    #[test]
    #[should_panic(expected = "without __typename")]
    fn an_untyped_node_under_a_fragment_fails_loudly() {
        let selections = select("{ ... on Commit { oid } }");
        prune(&json!({"oid":"abc"}), &selections);
    }

    #[test]
    fn connections_prune_every_node_and_page_info() {
        let answer = json!({
            "nodes":[{"name":"a","sha":1},{"name":"b","sha":2}],
            "pageInfo":{"hasNextPage":true,"endCursor":"2"},
            "totalCount":2
        });
        assert_eq!(
            prune(
                &answer,
                &below("{ connection { nodes { name } pageInfo { hasNextPage } } }"),
            ),
            json!({"nodes":[{"name":"a"},{"name":"b"}],"pageInfo":{"hasNextPage":true}})
        );
    }

    #[test]
    fn a_selected_field_the_resolver_did_not_produce_stays_absent() {
        let answer = json!({"oid":"abc"});
        assert_eq!(
            prune(&answer, &select("{ oid missing tree { oid } }")),
            json!({"oid":"abc"})
        );
        // A null the resolver produced stays null — absent is not null.
        assert_eq!(
            prune(
                &json!({"autoMergeRequest":null}),
                &select("{ autoMergeRequest { enabledAt } }"),
            ),
            json!({"autoMergeRequest":null})
        );
    }

    /// A request the fake will answer: GitHub requests carry a sensitive
    /// bearer token; `body` becomes the JSON request body.
    fn request(method: Method, path: &str, body: Option<Value>) -> Request {
        let mut request = Request::new(
            method,
            format!("https://api.github.com{path}").parse().unwrap(),
        );
        let mut token = reqwest::header::HeaderValue::from_static("token");
        token.set_sensitive(true);
        request
            .headers_mut()
            .insert(reqwest::header::AUTHORIZATION, token);
        if let Some(body) = body {
            *request.body_mut() = Some(serde_json::to_vec(&body).unwrap().into());
        }
        request
    }

    /// A GraphQL POST body: `query` plus `variables`.
    fn graphql(query: &str, variables: Value) -> Request {
        request(
            Method::POST,
            "/graphql",
            Some(json!({"query":query,"variables":variables})),
        )
    }

    /// The pull numbers a `pulls` listing page answers, in order.
    async fn listed(github: &FakeGitHub, query: &str) -> (Vec<u64>, Response) {
        let reply = github
            .send(request(
                Method::GET,
                &format!("/repos/acme/app/pulls{query}"),
                None,
            ))
            .await
            .unwrap();
        let prs: Vec<Value> = serde_json::from_slice(&reply.body).unwrap();
        let numbers = prs
            .iter()
            .map(|pr| pr["number"].as_u64().unwrap())
            .collect();
        (numbers, reply)
    }

    /// A stored REST pull for seeding: `created` is an RFC 3339 instant,
    /// `merged` stamps `merged_at` an hour on.
    fn stored_pr(number: u64, head: &str, login: &str, state: &str, created: &str) -> Value {
        json!({"number":number,"node_id":format!("pr-{number}"),
            "html_url":format!("https://github.com/acme/app/pull/{number}"),
            "state":state,"title":format!("pull {number}"),"body":"",
            "head":{"ref":head,"sha":format!("sha-{number}"),"repo":{"full_name":"acme/app"}},
            "base":{"ref":"main","repo":{"full_name":"acme/app"}},
            "user":{"login":login},"labels":[],"auto_merge":null,
            "created_at":created,
            "merged_at":Value::Null,
            "draft":false,"mergeable":true,"mergeable_state":"clean"})
    }

    /// `state=closed` covers merged pulls (their REST state is `closed`);
    /// the default is `open`, per GitHub's REST docs.
    #[tokio::test]
    async fn the_pulls_listing_filters_by_state() {
        let github = FakeGitHub::new("acme/app");
        {
            let mut state = github.state();
            let repo = state.repo_mut();
            repo.prs.insert(
                1,
                stored_pr(
                    1,
                    "patchula/a",
                    "patchula[bot]",
                    "open",
                    "2026-01-03T00:00:00Z",
                ),
            );
            repo.prs.insert(
                2,
                stored_pr(
                    2,
                    "patchula/b",
                    "patchula[bot]",
                    "closed",
                    "2026-01-02T00:00:00Z",
                ),
            );
            let mut merged = stored_pr(
                3,
                "patchula/c",
                "patchula[bot]",
                "closed",
                "2026-01-01T00:00:00Z",
            );
            merged["merged_at"] = json!("2026-01-01T01:00:00Z");
            repo.prs.insert(3, merged);
        }
        assert_eq!(listed(&github, "").await.0, [1]);
        assert_eq!(listed(&github, "?state=open").await.0, [1]);
        assert_eq!(listed(&github, "?state=closed").await.0, [2, 3]);
        assert_eq!(listed(&github, "?state=all").await.0, [1, 2, 3]);
        github.transcript_checked.store(true, Ordering::Relaxed);
    }

    /// `sort=created&direction=desc` is newest first; `asc` reverses;
    /// `per_page` sizes the page and the `Link` header walks it.
    #[tokio::test]
    async fn the_pulls_listing_orders_by_created_and_pages() {
        let github = FakeGitHub::new("acme/app");
        {
            let mut state = github.state();
            let repo = state.repo_mut();
            // Numbers deliberately disagree with creation order.
            repo.prs.insert(
                1,
                stored_pr(1, "b", "patchula[bot]", "closed", "2026-01-03T00:00:00Z"),
            );
            repo.prs.insert(
                2,
                stored_pr(2, "b", "patchula[bot]", "closed", "2026-01-01T00:00:00Z"),
            );
            repo.prs.insert(
                3,
                stored_pr(3, "b", "patchula[bot]", "closed", "2026-01-02T00:00:00Z"),
            );
        }
        let (page, reply) =
            listed(&github, "?state=all&sort=created&direction=desc&per_page=2").await;
        assert_eq!(page, [1, 3]);
        let link = reply.headers["link"].to_str().unwrap();
        assert!(
            link.contains("?state=all&sort=created&direction=desc&per_page=2&page=2>"),
            "{link}"
        );
        assert_eq!(listed(&github, "?state=all&page=2&per_page=2").await.0, [2]);
        assert_eq!(
            listed(&github, "?state=all&direction=asc").await.0,
            [2, 3, 1]
        );
        github.transcript_checked.store(true, Ordering::Relaxed);
    }

    /// A parameter the fake does not model — here GitHub's `head` filter —
    /// must not silently serve an unfiltered listing.
    #[test]
    #[should_panic(expected = "unmodeled pulls query parameter \"head\"")]
    fn an_unmodeled_pulls_parameter_panics() {
        let github = FakeGitHub::new("acme/app");
        drop(github.send(request(
            Method::GET,
            "/repos/acme/app/pulls?head=acme:patchula%2Fa",
            None,
        )));
    }

    /// `sort` values other than `created` are unmodeled too.
    #[test]
    #[should_panic(expected = "unmodeled pulls `sort`")]
    fn an_unmodeled_pulls_sort_panics() {
        let github = FakeGitHub::new("acme/app");
        drop(github.send(request(
            Method::GET,
            "/repos/acme/app/pulls?sort=updated",
            None,
        )));
    }

    const CLOSED_PULLS: &str =
        "query ClosedPulls($owner: String!, $name: String!, $head: String!, $after: String) {
        repository(owner: $owner, name: $name) {
            found: pullRequests(headRefName: $head, states: [CLOSED], first: 2, after: $after,
                                orderBy: {field: CREATED_AT, direction: DESC}) {
                pageInfo { hasNextPage endCursor }
                nodes { id number url title body state merged createdAt
                        headRefName headRefOid baseRefName
                        author { __typename login }
                        headRepository { nameWithOwner } baseRepository { nameWithOwner } }
            }
        }
    }";

    async fn closed_pulls(github: &FakeGitHub, head: &str, after: Value) -> Value {
        let reply = github
            .send(graphql(
                CLOSED_PULLS,
                json!({
                    "owner":"acme","name":"app","head":head,"after":after,
                }),
            ))
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&reply.body).unwrap();
        body["data"]["repository"]["found"].clone()
    }

    /// Several aliased `pullRequests` fields in one document each get
    /// their own filter; `states: [CLOSED]` means closed-unmerged, so a
    /// merged pull and another branch's pull stay out; `first`/`after`
    /// page the branch's pulls.
    #[tokio::test]
    async fn pull_requests_filters_by_branch_state_and_pages() {
        let github = FakeGitHub::new("acme/app");
        {
            let mut state = github.state();
            let repo = state.repo_mut();
            repo.prs.insert(
                1,
                stored_pr(
                    1,
                    "patchula/a",
                    "patchula[bot]",
                    "closed",
                    "2026-01-01T00:00:00Z",
                ),
            );
            repo.prs.insert(
                2,
                stored_pr(
                    2,
                    "patchula/a",
                    "patchula[bot]",
                    "closed",
                    "2026-01-03T00:00:00Z",
                ),
            );
            repo.prs.insert(
                3,
                stored_pr(
                    3,
                    "patchula/a",
                    "patchula[bot]",
                    "closed",
                    "2026-01-02T00:00:00Z",
                ),
            );
            let mut merged = stored_pr(
                4,
                "patchula/a",
                "patchula[bot]",
                "closed",
                "2026-01-04T00:00:00Z",
            );
            merged["merged_at"] = json!("2026-01-04T01:00:00Z");
            repo.prs.insert(4, merged);
            repo.prs.insert(
                5,
                stored_pr(
                    5,
                    "patchula/b",
                    "patchula[bot]",
                    "closed",
                    "2026-01-05T00:00:00Z",
                ),
            );
            repo.prs.insert(
                6,
                stored_pr(
                    6,
                    "patchula/a",
                    "patchula[bot]",
                    "open",
                    "2026-01-06T00:00:00Z",
                ),
            );
        }
        let first = closed_pulls(&github, "patchula/a", Value::Null).await;
        // Newest closed-unmerged first; the merged and the open pull stay out.
        assert_eq!(
            first["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n["number"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [2, 3]
        );
        assert_eq!(first["pageInfo"]["hasNextPage"], true);
        let cursor = first["pageInfo"]["endCursor"].as_str().unwrap().to_owned();
        let node = &first["nodes"][0];
        assert_eq!(node["state"], "CLOSED");
        assert_eq!(node["merged"], false);
        assert_eq!(node["createdAt"], "2026-01-03T00:00:00Z");
        assert_eq!(node["id"], "pr-2");
        assert_eq!(node["headRefName"], "patchula/a");
        assert_eq!(
            node["author"],
            json!({"__typename":"Bot","login":"patchula"})
        );
        assert_eq!(node["headRepository"]["nameWithOwner"], "acme/app");
        assert_eq!(node["baseRepository"]["nameWithOwner"], "acme/app");
        let second = closed_pulls(&github, "patchula/a", json!(cursor)).await;
        assert_eq!(
            second["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n["number"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [1]
        );
        assert_eq!(second["pageInfo"]["hasNextPage"], false);
        // A second alias in the same document filters independently.
        let reply = github
            .send(graphql(
                "query($owner: String!, $name: String!) {
                    repository(owner: $owner, name: $name) {
                        a: pullRequests(headRefName: \"patchula/a\", states: [MERGED], first: 10, orderBy: {field: CREATED_AT, direction: DESC}) { nodes { number state } }
                        b: pullRequests(headRefName: \"patchula/b\", states: [CLOSED], first: 10, orderBy: {field: CREATED_AT, direction: DESC}) { nodes { number state } }
                    }
                }",
                json!({"owner":"acme","name":"app"}),
            ))
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&reply.body).unwrap();
        assert_eq!(body["data"]["repository"]["a"]["nodes"][0]["number"], 4);
        assert_eq!(
            body["data"]["repository"]["a"]["nodes"][0]["state"],
            "MERGED"
        );
        assert_eq!(body["data"]["repository"]["b"]["nodes"][0]["number"], 5);
        github.transcript_checked.store(true, Ordering::Relaxed);
    }

    /// A pull whose head fork is gone answers `headRepository: null`, as
    /// GitHub does; the base repository still resolves.
    #[tokio::test]
    async fn pull_requests_reports_null_for_a_deleted_fork() {
        let github = FakeGitHub::new("acme/app");
        {
            let mut state = github.state();
            let mut pr = stored_pr(
                1,
                "patchula/a",
                "patchula[bot]",
                "closed",
                "2026-01-01T00:00:00Z",
            );
            pr["head"]["repo"] = Value::Null;
            state.repo_mut().prs.insert(1, pr);
        }
        let reply = github
            .send(graphql(
                "query($owner: String!, $name: String!) {
                    repository(owner: $owner, name: $name) {
                        pullRequests(headRefName: \"patchula/a\", states: [CLOSED], first: 10, orderBy: {field: CREATED_AT, direction: DESC}) {
                            nodes { headRepository { nameWithOwner } baseRepository { nameWithOwner } }
                        }
                    }
                }",
                json!({"owner":"acme","name":"app"}),
            ))
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&reply.body).unwrap();
        let node = &body["data"]["repository"]["pullRequests"]["nodes"][0];
        assert_eq!(node["headRepository"], Value::Null);
        assert_eq!(node["baseRepository"]["nameWithOwner"], "acme/app");
        github.transcript_checked.store(true, Ordering::Relaxed);
    }

    /// The connection's answer prunes to the selection: unselected node
    /// and pageInfo fields stay absent, and an argument the fake does not
    /// model is refused.
    #[tokio::test]
    async fn pull_requests_prunes_to_the_selection() {
        let github = FakeGitHub::new("acme/app");
        github.state().repo_mut().prs.insert(
            1,
            stored_pr(
                1,
                "patchula/a",
                "patchula[bot]",
                "closed",
                "2026-01-01T00:00:00Z",
            ),
        );
        let reply = github
            .send(graphql(
                "query($owner: String!, $name: String!) {
                    repository(owner: $owner, name: $name) {
                        pullRequests(first: 10, orderBy: {field: CREATED_AT, direction: DESC}) {
                            nodes { number }
                            pageInfo { hasNextPage }
                        }
                    }
                }",
                json!({"owner":"acme","name":"app"}),
            ))
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&reply.body).unwrap();
        let connection = &body["data"]["repository"]["pullRequests"];
        assert_eq!(connection["nodes"], json!([{"number":1}]));
        assert_eq!(connection["pageInfo"], json!({"hasNextPage":false}));
        github.transcript_checked.store(true, Ordering::Relaxed);
    }

    /// `baseRefName` is a schema argument the fake does not model.
    #[test]
    #[should_panic(expected = "unmodeled pullRequests argument baseRefName")]
    fn an_unmodeled_pull_requests_argument_panics() {
        let github = FakeGitHub::new("acme/app");
        drop(github.send(graphql(
            "query { repository(owner: \"acme\", name: \"app\") {
                pullRequests(baseRefName: \"main\") { nodes { number } } } }",
            json!({}),
        )));
    }

    fn pull_requests_with(arguments: &str) -> Request {
        graphql(
            &format!(
                "query {{ repository(owner: \"acme\", name: \"app\") {{
                    pullRequests({arguments}) {{ nodes {{ number }} }} }} }}"
            ),
            json!({}),
        )
    }

    /// GitHub refuses a connection without `first`/`last`.
    #[test]
    #[should_panic(expected = "pullRequests without `first`")]
    fn pull_requests_without_first_panics() {
        let github = FakeGitHub::new("acme/app");
        drop(github.send(pull_requests_with(
            "orderBy: {field: CREATED_AT, direction: DESC}",
        )));
    }

    /// GitHub refuses a `first` above 100.
    #[test]
    #[should_panic(expected = "outside 1-100")]
    fn pull_requests_with_too_large_a_page_panics() {
        let github = FakeGitHub::new("acme/app");
        drop(github.send(pull_requests_with(
            "first: 101, orderBy: {field: CREATED_AT, direction: DESC}",
        )));
    }

    /// A `PullRequestState` GitHub does not have is a validation error,
    /// never an empty answer.
    #[test]
    #[should_panic(expected = "has no value \"CLOSE\"")]
    fn pull_requests_with_an_unknown_state_panics() {
        let github = FakeGitHub::new("acme/app");
        drop(github.send(pull_requests_with(
            "states: [CLOSE], first: 10, orderBy: {field: CREATED_AT, direction: DESC}",
        )));
    }

    /// GitHub documents no order without `orderBy`, so the fake invents none.
    #[test]
    #[should_panic(expected = "pullRequests without `orderBy`")]
    fn pull_requests_without_an_order_panics() {
        let github = FakeGitHub::new("acme/app");
        drop(github.send(pull_requests_with("first: 10")));
    }

    /// `IssueOrder.direction` is required.
    #[test]
    #[should_panic(expected = "unmodeled pullRequests `orderBy.direction`")]
    fn pull_requests_with_a_partial_order_panics() {
        let github = FakeGitHub::new("acme/app");
        drop(github.send(pull_requests_with(
            "first: 10, orderBy: {field: CREATED_AT}",
        )));
    }

    /// The `next` link repeats the whole query, so later pages keep the
    /// caller's page size and direction.
    #[tokio::test]
    async fn the_pulls_next_link_keeps_the_query() {
        let github = FakeGitHub::new("acme/app");
        github.seed_pulls(150);
        let reply = github
            .send(request(
                Method::GET,
                "/repos/acme/app/pulls?state=all&sort=created&direction=asc&per_page=100",
                None,
            ))
            .await
            .unwrap();
        let link = reply.headers["link"].to_str().unwrap().to_owned();
        assert_eq!(
            link,
            "<https://api.github.com/repos/acme/app/pulls?state=all&sort=created&direction=asc&per_page=100&page=2>; rel=\"next\""
        );
        github.transcript_checked.store(true, Ordering::Relaxed);
    }

    /// `traffic` counts requests by route and the response bytes served.
    #[tokio::test]
    async fn traffic_counts_requests_and_response_bytes() {
        let github = FakeGitHub::new("acme/app");
        github.state().repo_mut().prs.insert(
            1,
            stored_pr(
                1,
                "patchula/a",
                "patchula[bot]",
                "open",
                "2026-01-01T00:00:00Z",
            ),
        );
        github.state().traffic.clear();
        listed(&github, "?state=all").await;
        listed(&github, "?state=open").await;
        listed(&github, "?state=open").await;
        let traffic = github.traffic();
        let open = &traffic["GET /repos/acme/app/pulls?state=open"];
        assert_eq!(open.requests, 2);
        assert_eq!(
            open.bytes,
            traffic["GET /repos/acme/app/pulls?state=all"].bytes * 2
        );
        assert!(open.bytes > 0);
        github.transcript_checked.store(true, Ordering::Relaxed);
    }

    /// `seed_pulls` fills a repository's history: open, closed and merged
    /// pulls the listings then filter and page for real.
    #[tokio::test]
    async fn seeded_pulls_fill_the_listings() {
        let github = FakeGitHub::new("acme/app");
        github.seed_pulls(100);
        {
            let state = github.state();
            let repo = state.repo();
            assert_eq!(repo.prs.len(), 100);
            let open = repo.prs.values().filter(|pr| pr["state"] == "open").count();
            let merged = repo
                .prs
                .values()
                .filter(|pr| !pr["merged_at"].is_null())
                .count();
            assert_eq!(open, 10);
            assert_eq!(merged, 30);
        }
        // The newest hundred closed-or-open pulls fit the default page.
        let (page, reply) = listed(&github, "?state=all").await;
        assert_eq!(page.len(), 30);
        assert!(reply.headers.get("link").is_some());
        github.transcript_checked.store(true, Ordering::Relaxed);
    }

    /// The installation listings lag a deletion: a deleted repository
    /// still lists, but every REST read answers 404 and GraphQL answers
    /// NOT_FOUND. `private` rides the listings and the repository read.
    #[tokio::test]
    async fn a_deleted_repository_lists_but_never_reads() {
        let github = FakeGitHub::new("acme/app");
        github
            .state
            .lock()
            .unwrap()
            .repositories
            .get_mut("acme/app")
            .unwrap()
            .deleted = true;
        let listed = github
            .send(request(Method::GET, "/installation/repositories", None))
            .await
            .unwrap();
        assert_eq!(listed.status, StatusCode::OK);
        let body: Value = serde_json::from_slice(&listed.body).unwrap();
        assert_eq!(
            body["repositories"],
            json!([{"full_name":"acme/app","private":false}])
        );
        for path in [
            "/repos/acme/app",
            "/repos/acme/app/pulls",
            "/repos/acme/app/branches",
            "/repos/acme/app/git/ref/heads/main",
        ] {
            let reply = github.send(request(Method::GET, path, None)).await.unwrap();
            assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
        }
        let reply = github
            .send(graphql(
                "query { repository(owner:\"acme\", name:\"app\") { id } }",
                json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(reply.status, StatusCode::OK);
        let body: Value = serde_json::from_slice(&reply.body).unwrap();
        assert_eq!(body["errors"][0]["type"], "NOT_FOUND");
        // This is the fake's own test: nothing to check a transcript against.
        github.transcript_checked.store(true, Ordering::Relaxed);
    }
}
