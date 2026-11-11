//! A stateful in-memory GitHub for offline tests: no sockets or network.
//!
//! [`FakeGitHub`] is the shared extraction of the write-path suites' fakes.
//! A repository's refs, commits, trees and pull requests live as REST-shaped
//! JSON — GitHub's terms, not the client's typed model — so the fake stays an
//! independent oracle for what the code asks and writes. Object ids are
//! counters (`commit-0`, `tree-1`), because tests pin them.
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
//! [`FakeGitHub::log`] `(method, path, body)`. Every request also lands on the
//! thread's [`crate::transcripts`] log, and the fake enforces the check:
//! dropping a `FakeGitHub` whose transcript never ran through
//! [`crate::transcripts::check`] panics, so a test cannot silently skip it.

use crate::transcripts;
use patchula_http::{Exchange, Response, Transport};
use reqwest::{Method, Request, StatusCode};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

fn response(status: u16, body: Value) -> Response {
    Response {
        status: StatusCode::from_u16(status).unwrap(),
        headers: Default::default(),
        body: serde_json::to_vec(&body).unwrap(),
        stale: None,
    }
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

/// Server properties: knobs that change what the server is, not what it does
/// to one request.
#[derive(Default)]
pub struct Settings {
    /// `pulls` listing page size; `None` means GitHub's 100.
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

/// A canned upstream repository: contents listings, the mise-versions mirror
/// documents, and whether `releases/tags/{tag}` serves bodies.
struct Upstream {
    contents: Value,
    releases: Value,
    tags: Value,
    release_tags: bool,
}

/// The server's repository state: REST-shaped JSON in GitHub's terms — an
/// independent oracle, never the client's typed model.
pub struct State {
    repo_id: String,
    default_branch: String,
    actor: String,
    /// Head sha of the default branch.
    pub base: String,
    /// `refs/{namespace}` → object sha. Deletions record the zero sha.
    pub refs: BTreeMap<String, String>,
    /// `git/trees/{sha}` answers: each tree's `tree` array.
    pub trees: BTreeMap<String, Value>,
    /// Commits by sha; extra `actor`/`committer_login` fields steer
    /// authorship.
    pub commits: BTreeMap<String, Value>,
    /// Pull requests by number, REST PR objects.
    pub prs: BTreeMap<u64, Value>,
    /// Commits `compare` reports ahead of a head.
    pub history: Vec<String>,
    /// Heads `compare` reports as merged into the base.
    pub merged_heads: BTreeSet<String>,
    /// The upstream release's body text.
    pub release_note_body: Option<String>,
    /// `(method, path)` of every request, in arrival order.
    pub requests: Vec<(String, String)>,
    /// `(path, body)` of every mutation request, in arrival order.
    pub writes: Vec<(String, Value)>,
    /// ReleaseNotes operation bodies, in arrival order.
    pub release_queries: Vec<Value>,
    /// Server properties.
    pub settings: Settings,
    /// `(method, path, body)` of every request, in arrival order.
    log: Vec<(String, String, Value)>,
    /// Heads applied after `settings.pr_lag_reads` more reads.
    pending_heads: BTreeMap<u64, Value>,
    upstreams: BTreeMap<String, Upstream>,
}

impl State {
    fn new() -> Self {
        Self {
            repo_id: "repo-id".into(),
            default_branch: "main".into(),
            actor: "patchula[bot]".into(),
            base: String::new(),
            refs: BTreeMap::new(),
            trees: BTreeMap::new(),
            commits: BTreeMap::new(),
            prs: BTreeMap::new(),
            history: Vec::new(),
            merged_heads: BTreeSet::new(),
            release_note_body: None,
            requests: Vec::new(),
            writes: Vec::new(),
            release_queries: Vec::new(),
            settings: Settings::default(),
            log: Vec::new(),
            pending_heads: BTreeMap::new(),
            upstreams: BTreeMap::from([
                (
                    "nodejs/node".into(),
                    Upstream {
                        contents: json!([]),
                        releases: json!({"releases":[],"truncated":false}),
                        tags: json!({"tags":[],"truncated":false}),
                        release_tags: false,
                    },
                ),
                (
                    "actions/checkout".into(),
                    Upstream {
                        contents: json!({"tree":[],"truncated":false}),
                        releases: json!({"releases":[{"tag":"20.0.0"},{"tag":"22.0.0"}],"truncated":false}),
                        tags: json!({"tags":[{"tag":"20.0.0"},{"tag":"22.0.0"}],"truncated":false}),
                        release_tags: true,
                    },
                ),
            ]),
        }
    }
}

/// What a matched request carried, handed to [`On::mutate`] closures.
pub struct Hit {
    /// The request's method.
    pub method: Method,
    /// The request's path, without host or query.
    pub path: String,
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
    /// `GET /repos/{repo}` — repository metadata.
    RepoRead {
        /// The repository, `owner/name`.
        repo: String,
    },
    /// A GraphQL operation root field, e.g. `"updateRefs"`.
    Graphql(&'static str),
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

/// A stateful in-memory GitHub: one repository's REST and GraphQL endpoints,
/// scripted faults, request recording and transcript enforcement. Construct
/// with [`FakeGitHub::new`], wrap in `Arc` and hand to the client's
/// `with_transport`.
pub struct FakeGitHub {
    /// `owner/name` — kept outside the state lock so fault helpers can be
    /// armed while a test holds [`FakeGitHub::state`].
    repo: String,
    state: Mutex<State>,
    rules: Mutex<Vec<Rule>>,
    /// Set by `transcripts::check`; unset at drop means the test skipped it.
    transcript_checked: Arc<AtomicBool>,
}

/// A GraphQL root field, distilled to owned facts — routing needs no AST
/// borrows.
struct GqlField {
    /// The response key: the alias or the field name.
    key: String,
    name: String,
    /// A `repository` field's own selection, as `(key, name, asks for
    /// description)` — `release` bodies check the last.
    children: Vec<(String, String, bool)>,
}

/// One GraphQL operation's kind and root fields.
struct GqlOp {
    mutation: bool,
    fields: Vec<GqlField>,
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
    /// The request's GraphQL operations are mutations.
    mutation: bool,
    /// Root field names of the GraphQL operations.
    graphql: Vec<String>,
    ops: Vec<GqlOp>,
}

impl Ctx {
    fn new(request: &Request, path: &str, body: &Value) -> Self {
        use graphql_parser::query::{
            Definition, OperationDefinition, Selection, SelectionSet, parse_query,
        };
        fn fields<'a>(set: &'a SelectionSet<'a, &'a str>) -> Vec<GqlField> {
            set.items
                .iter()
                .map(|selection| {
                    let Selection::Field(field) = selection else {
                        panic!("unmodeled GraphQL selection")
                    };
                    GqlField {
                        key: field.alias.unwrap_or(field.name).to_owned(),
                        name: field.name.to_owned(),
                        children: field
                            .selection_set
                            .items
                            .iter()
                            .filter_map(|child| {
                                let Selection::Field(child) = child else {
                                    // `... on Commit` and friends sit under
                                    // fields whose answers the fake builds
                                    // from state; routing reads names only.
                                    return None;
                                };
                                Some((
                                    child.alias.unwrap_or(child.name).to_owned(),
                                    child.name.to_owned(),
                                    child.selection_set.items.iter().any(|s| {
                                        matches!(s, Selection::Field(f) if f.name == "description")
                                    }),
                                ))
                            })
                            .collect(),
                    }
                })
                .collect()
        }
        let method = request.method().clone();
        let mut repo = None;
        let mut pull = None;
        let mut pull_read = false;
        if let Some(rest) = path.strip_prefix("/repos/")
            && let Some((owner, rest)) = rest.split_once('/')
        {
            let (name, rest) = rest.split_once('/').unwrap_or((rest, ""));
            repo = Some(format!("{owner}/{name}"));
            if let Some(number) = rest.strip_prefix("pulls/") {
                pull = number.parse().ok();
                pull_read = method == Method::GET;
            }
        }
        let mut graphql = Vec::new();
        let mut mutation = false;
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
                let fields = fields(set);
                mutation |= is_mutation;
                graphql.extend(fields.iter().map(|f| f.name.clone()));
                ops.push(GqlOp {
                    mutation: is_mutation,
                    fields,
                });
            }
        }
        Self {
            method,
            path: path.to_owned(),
            repo,
            pull,
            pull_read,
            mutation,
            graphql,
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
            Matcher::RepoRead { repo } => {
                self.method == Method::GET && self.path == format!("/repos/{repo}")
            }
            Matcher::Graphql(field) => self.graphql.iter().any(|name| name == *field),
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
    /// branch `main`.
    pub fn new(repo: &str) -> Self {
        let this = Self {
            repo: repo.into(),
            state: Mutex::new(State::new()),
            rules: Mutex::new(Vec::new()),
            transcript_checked: Arc::new(AtomicBool::new(false)),
        };
        transcripts::watch(&this.transcript_checked);
        this
    }
    /// Seed the repository: `branch` points at commit `base` whose tree
    /// `"{base}-tree"` holds `entries` (REST tree entries).
    pub fn seed(&self, branch: &str, base: &str, entries: Vec<Value>) {
        let mut state = self.state();
        state.default_branch = branch.into();
        state.base = base.into();
        state
            .refs
            .insert(format!("refs/heads/{branch}"), base.into());
        state.trees.insert(format!("{base}-tree"), json!(entries));
        state.commits.insert(
            base.into(),
            json!({"sha":base,"tree":{"sha":format!("{base}-tree")}}),
        );
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
    /// `updateRefs` never satisfies the lease: GraphQL errors, refs
    /// unchanged.
    pub fn update_refs_fail(&self) {
        self.on(Matcher::Graphql("updateRefs"))
            .respond(200, json!({"errors":[{"message":"reference changed"}]}));
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
            state.prs.get_mut(&hit.pull.unwrap()).unwrap()["state"] = json!("closed");
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
            state.prs.get_mut(&hit.pull.unwrap()).unwrap()["state"] = json!("closed");
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
            state.prs.get_mut(&hit.pull.unwrap()).unwrap()["head"]["sha"] = json!(sha);
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
                    pull: ctx.pull,
                },
            );
        }
    }
    fn graphql(&self, ctx: &Ctx, body: &Value, state: &mut State) -> Response {
        let mut data = Map::new();
        for op in &ctx.ops {
            for field in &op.fields {
                let answer = if op.mutation {
                    self.graphql_mutation(&field.name, body, state)
                } else {
                    self.graphql_query(field, body, state)
                };
                match answer {
                    FieldAnswer::Data(value) => {
                        data.insert(field.key.clone(), value);
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
        FieldAnswer::Data(match field.name.as_str() {
            "viewer" => json!({"login":state.actor}),
            "nodes" => self.closed_prs(body, state),
            "repository" => self.repository(field, body, state),
            name => panic!("unmodeled GraphQL root field {name}"),
        })
    }
    fn closed_prs(&self, body: &Value, state: &State) -> Value {
        let nodes: Vec<_> = state
            .prs
            .values()
            .filter(|p| {
                body["variables"]["closed"]
                    .as_array()
                    .unwrap()
                    .contains(&p["node_id"])
            })
            .map(|p| {
                json!({"id":p["node_id"],
                       "state":if p["state"] == "open" {"OPEN"} else {"CLOSED"},
                       "merged":!p["merged_at"].is_null(),
                       "body":p["body"],"author":graphql_author(p),
                       "headRepository":{"nameWithOwner":p["head"]["repo"]["full_name"]},
                       "baseRepository":{"nameWithOwner":p["base"]["repo"]["full_name"]}})
            })
            .collect();
        json!(nodes)
    }
    fn repository(&self, field: &GqlField, body: &Value, state: &mut State) -> Value {
        let mut repo = Map::new();
        let mut release_logged = false;
        for (key, name, with_description) in &field.children {
            match name.as_str() {
                "defaultBranchRef" => {
                    repo.insert(
                        key.clone(),
                        json!({"name":state.default_branch,"target":{"oid":state.base}}),
                    );
                }
                "issues" => {
                    repo.insert(
                        key.clone(),
                        json!({"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}),
                    );
                }
                "refs" => {
                    repo.insert(key.clone(), self.refs_connection(body, state));
                }
                "release" => {
                    if !release_logged {
                        state.release_queries.push(body.clone());
                        release_logged = true;
                    }
                    let text = state
                        .release_note_body
                        .clone()
                        .unwrap_or_else(|| "Release details 🦀\n".repeat(10_000));
                    repo.insert(
                        key.clone(),
                        if *with_description {
                            json!({"updatedAt":blob_sha(&text),"body":text,"draft":false,"prerelease":false})
                        } else {
                            json!({"updatedAt":blob_sha(&text)})
                        },
                    );
                }
                // Server settings the suites never read through GraphQL; the
                // old fake's answer carried none.
                "id"
                | "autoMergeAllowed"
                | "squashMergeAllowed"
                | "mergeCommitAllowed"
                | "rebaseMergeAllowed"
                | "deleteBranchOnMerge" => {}
                name => panic!("unmodeled repository field {name}"),
            }
        }
        Value::Object(repo)
    }
    fn refs_connection(&self, body: &Value, state: &State) -> Value {
        let nodes: Vec<_> = state
            .refs
            .iter()
            .map(|(name, oid)| {
                let name = name.strip_prefix("refs/heads/").unwrap();
                let commit = &state.commits[oid];
                let prs: Vec<_> = state
                    .prs
                    .values()
                    .filter(|p| p["state"] == "open" && p["head"]["ref"] == name)
                    .map(|p| {
                        json!({"number":p["number"],"id":p["node_id"],"url":p["html_url"],"title":p["title"],"body":p["body"],
                               "headRefName":p["head"]["ref"],"headRefOid":p["head"]["sha"],"baseRefName":p["base"]["ref"],"author":graphql_author(p),
                               "headRepository":{"nameWithOwner":p["head"]["repo"]["full_name"]},"baseRepository":{"nameWithOwner":p["base"]["repo"]["full_name"]},
                               "labels":{"nodes":p["labels"],"pageInfo":{"hasNextPage":state.settings.label_next_page == p["number"].as_u64()}},"autoMergeRequest":p["auto_merge"]})
                    })
                    .collect();
                json!({"name":name,"target":{"oid":oid,"tree":{"oid":commit["tree"]["sha"]},"message":commit["message"],
                       "author":{"user":{"login":commit.get("actor").and_then(Value::as_str).unwrap_or("patchula[bot]")}},
                       "committer":{"user":{"login":"web-flow"}},"signature":{"isValid":!state.settings.unsigned}},
                       "associatedPullRequests":{"nodes":prs,"pageInfo":{"hasNextPage":state.settings.pr_next_page.as_deref() == Some(name)}}})
            })
            .collect();
        let offset = body["variables"]["cursor"]
            .as_str()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        let size = state.settings.ref_page_size.unwrap_or(100);
        let more = offset + size < nodes.len();
        let nodes: Vec<_> = nodes.into_iter().skip(offset).take(size).collect();
        json!({"nodes":nodes,"pageInfo":{"hasNextPage":more,"endCursor":(offset+size).to_string()}})
    }
    fn graphql_mutation(&self, name: &str, body: &Value, state: &mut State) -> FieldAnswer {
        match name {
            "updateRefs" => self.update_refs(body, state),
            "enablePullRequestAutoMerge" | "disablePullRequestAutoMerge" => {
                let enabled = name.starts_with("enable");
                for pr in state.prs.values_mut() {
                    pr["auto_merge"] = if enabled {
                        json!({"enabled":true})
                    } else {
                        Value::Null
                    };
                }
                FieldAnswer::Data(json!({"pullRequest":{"id":"pr-id"}}))
            }
            _ => panic!("unmodeled GraphQL mutation {name}"),
        }
    }
    fn update_refs(&self, body: &Value, state: &mut State) -> FieldAnswer {
        let input = &body["variables"]["input"];
        assert_eq!(input["repositoryId"], state.repo_id);
        let updates = input["refUpdates"].as_array().unwrap();
        let zero = "0000000000000000000000000000000000000000";
        let valid = updates.iter().all(|r| {
            state
                .refs
                .get(r["name"].as_str().unwrap())
                .map_or(zero, String::as_str)
                == r["beforeOid"].as_str().unwrap()
        });
        if !valid {
            return FieldAnswer::Errors(json!([{"message":"reference changed"}]));
        }
        for r in updates {
            state.refs.insert(
                r["name"].as_str().unwrap().into(),
                r["afterOid"].as_str().unwrap().into(),
            );
        }
        let lag = state.settings.pr_lag_reads > 0;
        let mut pending = BTreeMap::new();
        for pr in state.prs.values_mut() {
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
        state.pending_heads.extend(pending);
        FieldAnswer::Data(json!({"clientMutationId":null}))
    }
    fn create_pull(&self, body: &Value, state: &mut State) -> Response {
        let number = state.prs.len() as u64 + 1;
        let repo = &self.repo;
        let sha = state.refs[&format!("refs/heads/{}", body["head"].as_str().unwrap())].clone();
        let mut pr = json!({"number":number,"node_id":format!("pr-{number}"),"html_url":format!("https://github.com/{repo}/pull/{number}"),"state":"open","title":body["title"],"body":body["body"],"head":{"ref":body["head"],"sha":sha,"repo":{"full_name":repo}},"base":{"ref":body["base"],"repo":{"full_name":repo}},"user":{"login":"patchula[bot]"},"labels":[],"auto_merge":null,"merged_at":null,"created_at":chrono::Utc::now().to_rfc3339()});
        pr["head"]["label"] = json!("demo:branch");
        pr["head"]["repo"]["updated_at"] = json!("2026-10-15T00:00:00Z");
        pr["base"]["sha"] = json!(state.base);
        pr["base"]["repo"]["size"] = json!(42);
        state.prs.insert(number, pr.clone());
        response(201, pr)
    }
    fn create_tree(&self, body: &Value, state: &mut State) -> Response {
        let mut tree = state.trees[body["base_tree"].as_str().unwrap()].clone();
        for original in body["tree"].as_array().unwrap() {
            let mut entry = original.clone();
            if let Some(content) = entry["content"].as_str() {
                entry["sha"] = json!(blob_sha(content));
                entry.as_object_mut().unwrap().remove("content");
            }
            tree.as_array_mut()
                .unwrap()
                .retain(|e| e["path"] != entry["path"]);
            if !(entry.get("sha").is_some_and(Value::is_null) && entry.get("content").is_none()) {
                tree.as_array_mut().unwrap().push(entry.clone());
            }
        }
        let sha = format!("tree-{}", state.trees.len());
        state.trees.insert(sha.clone(), tree);
        response(201, json!({"sha":sha}))
    }
    fn create_commit(&self, body: &Value, state: &mut State) -> Response {
        assert!(
            body.get("author").is_none()
                && body.get("committer").is_none()
                && body.get("signature").is_none()
        );
        assert_eq!(body["parents"].as_array().unwrap().len(), 1);
        assert_eq!(body["parents"][0], state.base);
        let sha = format!("commit-{}", state.commits.len());
        let commit = json!({"sha":sha,"tree":{"sha":body["tree"]},"message":body["message"],"parents":[{"sha":body["parents"][0]}],"verification":{"verified":!state.settings.unsigned}});
        state.commits.insert(sha.clone(), commit.clone());
        response(201, commit)
    }
    fn patch_pull(&self, rest: &str, body: &Value, state: &mut State) -> Response {
        let n: u64 = rest.rsplit('/').next().unwrap().parse().unwrap();
        let pr = state.prs.get_mut(&n).unwrap();
        for (key, value) in body.as_object().unwrap() {
            if key == "base" {
                pr["base"]["ref"].clone_from(value);
            } else {
                pr[key].clone_from(value);
            }
        }
        response(200, pr.clone())
    }
    fn put_labels(&self, rest: &str, body: &Value, state: &mut State) -> Response {
        let n: u64 = rest.split('/').nth(1).unwrap().parse().unwrap();
        let labels: Vec<_> = body["labels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| json!({"name":l}))
            .collect();
        state.prs.get_mut(&n).unwrap()["labels"] = json!(labels);
        response(200, json!(labels))
    }
    fn list_pulls(&self, request: &Request, state: &State) -> Response {
        let page = request
            .url()
            .query_pairs()
            .find(|(k, _)| k == "page")
            .and_then(|(_, p)| p.parse::<usize>().ok())
            .unwrap_or(1);
        let size = state.settings.pr_page_size.unwrap_or(100);
        let prs: Vec<_> = state
            .prs
            .values()
            .rev()
            .skip((page - 1) * size)
            .take(size)
            .cloned()
            .collect();
        let mut reply = response(200, json!(prs));
        if page * size < state.prs.len() {
            reply.headers.insert(
                "link",
                format!(
                    "<https://api.github.com/repos/{}/pulls?state=all&page={}>; rel=\"next\"",
                    self.repo,
                    page + 1
                )
                .parse()
                .unwrap(),
            );
        }
        reply
    }
    fn pull_read(&self, ctx: &Ctx, state: &mut State) -> Response {
        let n = ctx.pull.unwrap();
        if state.settings.pr_lag_reads > 0 {
            state.settings.pr_lag_reads -= 1;
        } else if let Some(head) = state.pending_heads.remove(&n) {
            state.prs.get_mut(&n).unwrap()["head"]["sha"] = head;
        }
        // `mutate` rules fire after read lag: the request sees state at its
        // own point.
        self.fire_mutates(ctx, state);
        response(200, state.prs[&n].clone())
    }
    fn dispatch(&self, request: &Request, body: &Value, ctx: &Ctx, state: &mut State) -> Response {
        let path = ctx.path.as_str();
        if path == "/graphql" {
            return self.graphql(ctx, body, state);
        }
        if let Some(doc) = path.strip_prefix("/data/github/") {
            let Some((repo, kind)) = doc.rsplit_once('/') else {
                panic!("unexpected read {path}")
            };
            let Some(upstream) = state.upstreams.get(repo) else {
                panic!("unexpected read {path}")
            };
            return response(
                200,
                match kind {
                    "releases.json" => upstream.releases.clone(),
                    "tags.json" => upstream.tags.clone(),
                    _ => panic!("unexpected read {path}"),
                },
            );
        }
        let Some(rest) = path.strip_prefix("/repos/") else {
            panic!("unexpected read {path}")
        };
        let (owner, rest) = rest.split_once('/').unwrap();
        let (name, rest) = rest.split_once('/').unwrap_or((rest, ""));
        if format!("{owner}/{name}") != self.repo {
            let Some(upstream) = state.upstreams.get(&format!("{owner}/{name}")) else {
                panic!("unexpected read {path}")
            };
            return if ctx.method == Method::GET && rest == "contents" {
                response(200, upstream.contents.clone())
            } else if ctx.method == Method::GET
                && rest.starts_with("releases/tags/")
                && upstream.release_tags
            {
                let etag = state.release_note_body.as_ref().map_or_else(
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
                        json!({"body":state.release_note_body.clone().unwrap_or_else(|| {
                            "Release details 🦀\n".repeat(10_000)
                        })}),
                    )
                };
                reply.headers.insert("etag", etag.parse().unwrap());
                reply
            } else {
                panic!("unexpected read {path}")
            };
        }
        if ctx.method == Method::POST {
            match rest {
                "pulls" => self.create_pull(body, state),
                "git/blobs" => {
                    assert_eq!(body["encoding"], "utf-8");
                    response(
                        201,
                        json!({"sha":blob_sha(body["content"].as_str().unwrap())}),
                    )
                }
                "git/trees" => self.create_tree(body, state),
                "git/commits" => self.create_commit(body, state),
                _ => panic!("unexpected write {path}"),
            }
        } else if ctx.method == Method::PATCH && rest.starts_with("pulls/") {
            self.patch_pull(rest, body, state)
        } else if ctx.method == Method::PUT && rest.ends_with("/labels") {
            self.put_labels(rest, body, state)
        } else if ctx.method == Method::GET {
            if rest == "pulls" {
                self.list_pulls(request, state)
            } else if rest.starts_with("pulls/") {
                self.pull_read(ctx, state)
            } else if rest.starts_with(&format!("branches/{}", state.default_branch)) {
                response(200, json!({"commit":{"sha":state.base}}))
            } else if rest == "branches" {
                let branches: Vec<_> = state
                    .refs
                    .iter()
                    .map(|(n, sha)| {
                        json!({"name":n.strip_prefix("refs/heads/").unwrap(),"commit":{"sha":sha}})
                    })
                    .collect();
                response(200, json!(branches))
            } else if let Some(sha) = rest.strip_prefix("commits/") {
                response(200, json!({"commit":state.commits[sha]}))
            } else if rest.is_empty() {
                response(
                    200,
                    json!({"node_id":state.repo_id,"default_branch":state.default_branch,
                           "allow_auto_merge":true,"allow_squash_merge":true}),
                )
            } else if let Some(branch) = rest.strip_prefix("git/ref/heads/") {
                // url::Url keeps percent-encoded slashes in a path segment.
                let branch = branch.replace("%2F", "/");
                match state.refs.get(&format!("refs/heads/{branch}")) {
                    Some(sha) => response(200, json!({"object":{"sha":sha}})),
                    None => response(404, json!({})),
                }
            } else if let Some(sha) = rest.strip_prefix("git/commits/") {
                response(200, state.commits[sha].clone())
            } else if let Some(sha) = rest.strip_prefix("git/trees/") {
                response(
                    200,
                    json!({"tree":state.trees[sha],"truncated":state.settings.truncated}),
                )
            } else if rest.starts_with("compare/unknown...") {
                response(404, json!({"message":"Not Found"}))
            } else if let Some(spec) = rest.strip_prefix("compare/") {
                let sha = spec.split("...").nth(1).unwrap();
                let history: Vec<_> = state
                    .history
                    .iter()
                    .map(String::as_str)
                    .chain(std::iter::once(sha))
                    .collect();
                let commits: Vec<_> = if spec.starts_with(&format!("{}...", state.base))
                    && state.merged_heads.contains(sha)
                {
                    Vec::new()
                } else {
                    history
                        .iter()
                        .map(|sha| {
                            let commit = &state.commits[*sha];
                            json!({"sha":sha,"author":{"login":commit.get("actor").and_then(Value::as_str).unwrap_or("patchula[bot]")},"committer":{"login":commit.get("committer_login").and_then(Value::as_str).unwrap_or("web-flow")},"commit":commit})
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
