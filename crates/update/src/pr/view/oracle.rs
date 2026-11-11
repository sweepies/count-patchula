//! The differential oracle (the design's Net 1): over every shape of pull
//! request the decisions read, `admit` fed the whole history decides
//! exactly what it decides fed a model of the narrowed reads — the open
//! PRs, the hourly count when `prs.perHour` is configured and the closed
//! unmerged PRs of each branch `closed_reads` names — except the accepted
//! divergence `KEY_ON_OTHER_BRANCH`, pinned by its own test.
use super::*;
use crate::Change;
use crate::batch::Item;
use crate::markers::Marker;
use crate::plan::testing::Draft;
use patchula_github::model;
use patchula_report::BranchAction;
use patchula_words::{Source, Update};

const ACTOR: &str = "patchula[bot]";
const REPO: &str = "sweepies/count-patchula";
const PLAN_BRANCH: &str = "patchula/serde-1.x";
const OTHER_BRANCH: &str = "patchula/left-pad-1.x";
const OWNER: Owner<'static> = Owner {
    actor: ACTOR,
    repo: REPO,
};

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-11T12:00:00Z")
        .expect("constant")
        .to_utc()
}

/// Who authored the PR: the Count or anyone else.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Author {
    Count,
    Other,
}
/// The PR's state: merged PRs are `closed` with `merged_at`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PrState {
    Open,
    Closed,
    Merged,
}
/// The PR's head branch: the plan's or another.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Head {
    Plan,
    Other,
}
/// The PR's head repository: this one, a fork or deleted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HeadRepo {
    Same,
    Fork,
    Gone,
}
/// The ownership marker the body carries: none, the plan's key or another.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Marked {
    None,
    Plan,
    Other,
}
/// The closure marker ending the body, if any.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Closure {
    None,
    Obsolete,
    Batched,
}
/// Whether the body carries a batch-items section.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Items {
    Absent,
    Present,
}
/// When the PR was created: inside the hour or older.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Created {
    Recent,
    Older,
}
/// Whether the body ticks the re-check box: `checked` reads it and a ticked
/// open PR retries its plan.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tick {
    Unticked,
    Recheck,
}
/// One pull request as the decisions read it.
#[derive(Clone, Copy, Debug)]
struct Shape {
    author: Author,
    state: PrState,
    head: Head,
    repo: HeadRepo,
    marked: Marked,
    closure: Closure,
    items: Items,
    created: Created,
    tick: Tick,
}

/// The shapes the oracle excludes: a closed PR carrying the plan's key on
/// another head branch. Today's whole listing suppresses the plan on them;
/// the narrowed closed read will not — the accepted behaviour change
/// `closed_key_on_another_branch_suppresses_today` pins.
const KEY_ON_OTHER_BRANCH: (PrState, Head, Marked) = (PrState::Closed, Head::Other, Marked::Plan);

/// Every shape, minus the `KEY_ON_OTHER_BRANCH` family. The ninth axis is
/// the re-check tick `checked` reads, which the design's list rolls into
/// "marker"; the other eight are its enumeration.
fn shapes() -> Vec<Shape> {
    let mut out = Vec::with_capacity(2 * 3 * 2 * 3 * 3 * 3 * 2 * 2 * 2);
    for author in [Author::Count, Author::Other] {
        for state in [PrState::Open, PrState::Closed, PrState::Merged] {
            for head in [Head::Plan, Head::Other] {
                for repo in [HeadRepo::Same, HeadRepo::Fork, HeadRepo::Gone] {
                    for marked in [Marked::None, Marked::Plan, Marked::Other] {
                        for closure in [Closure::None, Closure::Obsolete, Closure::Batched] {
                            for items in [Items::Absent, Items::Present] {
                                for created in [Created::Recent, Created::Older] {
                                    for tick in [Tick::Unticked, Tick::Recheck] {
                                        if (state, head, marked) != KEY_ON_OTHER_BRANCH {
                                            out.push(Shape {
                                                author,
                                                state,
                                                head,
                                                repo,
                                                marked,
                                                closure,
                                                items,
                                                created,
                                                tick,
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

/// A shape's nine field values, for the pair-coverage accounting.
fn fields(shape: &Shape) -> [u8; 9] {
    [
        shape.author as u8,
        shape.state as u8,
        shape.head as u8,
        shape.repo as u8,
        shape.marked as u8,
        shape.closure as u8,
        shape.items as u8,
        shape.created as u8,
        shape.tick as u8,
    ]
}
/// How many values each field has, in `fields` order.
const ARITY: [u8; 9] = [2, 3, 2, 3, 3, 3, 2, 2, 2];

fn body_for(shape: &Shape, plan_key: &str) -> String {
    let mut body = String::new();
    if shape.items == Items::Present {
        body.push_str(&markers::hidden(
            markers::ITEMS,
            &[Item {
                origin: "o".repeat(64),
                number: Some(9),
                title: "batched update".into(),
                targets: Vec::new(),
            }],
        ));
        body.push('\n');
    }
    if shape.tick == Tick::Recheck {
        body.push_str("- [x] check it again <!-- patchula-recheck -->\n");
    }
    match shape.marked {
        Marked::None => body.push_str("a pull request body"),
        Marked::Plan => {
            body.push_str(&markers::ownership(&Marker::new(
                "a".repeat(64),
                plan_key.to_owned(),
            )));
        }
        Marked::Other => {
            body.push_str(&markers::ownership(&Marker::new(
                "a".repeat(64),
                "b".repeat(64),
            )));
        }
    }
    match shape.closure {
        Closure::None => {}
        Closure::Obsolete => {
            body.push('\n');
            body.push_str(markers::OBSOLETE);
        }
        Closure::Batched => {
            body.push('\n');
            body.push_str(markers::BATCHED);
        }
    }
    body
}

fn tracked(shape: &Shape, number: u64, plan_key: &str) -> Tracked {
    Tracked::from(model::PullRequest {
        node_id: format!("PR_node{number}"),
        number,
        html_url: format!("https://github.com/{REPO}/pull/{number}"),
        state: match shape.state {
            PrState::Open => "open",
            PrState::Closed | PrState::Merged => "closed",
        }
        .into(),
        title: format!("pull {number}"),
        body: Some(body_for(shape, plan_key)),
        user: Some(model::Actor {
            login: match shape.author {
                Author::Count => ACTOR,
                Author::Other => "renovate[bot]",
            }
            .into(),
        }),
        draft: Some(false),
        labels: Vec::new(),
        merged_at: (shape.state == PrState::Merged).then(|| "2026-11-10T12:00:00Z".to_owned()),
        auto_merge: None,
        mergeable: None,
        mergeable_state: None,
        created_at: Some(
            match shape.created {
                Created::Recent => "2026-11-11T11:45:00Z",
                Created::Older => "2026-11-11T09:00:00Z",
            }
            .into(),
        ),
        updated_at: None,
        head: model::PullRequestHead {
            branch: match shape.head {
                Head::Plan => PLAN_BRANCH,
                Head::Other => OTHER_BRANCH,
            }
            .into(),
            sha: format!("{number}ead"),
            repo: match shape.repo {
                HeadRepo::Same => Some(REPO.to_owned()),
                HeadRepo::Fork => Some("a-fork/count-patchula".to_owned()),
                HeadRepo::Gone => None,
            }
            .map(|full_name| model::PullRequestRepository { full_name }),
        },
        base: model::PullRequestBase {
            branch: "main".into(),
            repo: Some(model::PullRequestRepository {
                full_name: REPO.to_owned(),
            }),
        },
    })
}

/// The plan the whole oracle runs against: one update on `PLAN_BRANCH`.
fn plan() -> Plan {
    Draft::new(BranchAction {
        repository: REPO.into(),
        branch: PLAN_BRANCH.into(),
        base_commit: "b".repeat(40),
        status: BranchStatus::Planned,
        reason: None,
        message: "m".into(),
        previous_commit: None,
        commit: None,
        verified: None,
        lockfile_pending: false,
        files: Vec::new(),
    })
    .change(Change {
        source: Source::Crates,
        package: "serde".into(),
        registry: None,
        file: "Cargo.toml".into(),
        current: "1.0.0".into(),
        current_version: None,
        value: Some("1.0.1".into()),
        version: Some("1.0.1".into()),
        digest: None,
        kind: Update::Minor,
        merge: false,
        labels: Vec::new(),
        release_notes: false,
        body_notes: Vec::new(),
        advisories: Vec::new(),
        image: None,
    })
    .build()
}

/// What the dashboard ticked for the plan's key.
#[derive(Clone, Copy, Debug)]
enum Ask {
    /// Nothing.
    Planned,
    /// "Bring it back": `controls.recreate`.
    BringBack,
    /// "Create it anyway": `controls.create`.
    CreateNow,
}

fn controls(ask: Ask, key: &str, batched: bool) -> Controls {
    let mut controls = Controls::default();
    match ask {
        Ask::Planned => {}
        Ask::BringBack => {
            controls.recreate.insert(key.to_owned());
        }
        Ask::CreateNow => {
            controls.create.insert(key.to_owned());
        }
    }
    if batched {
        controls.batched.insert(key.to_owned());
    }
    controls
}

/// The `prs` limits under test: none, each alone, both above one so
/// counting past one PR matters.
const LIMITS: [(Option<u32>, Option<u32>); 4] = [
    (None, None),
    (Some(1), None),
    (None, Some(1)),
    (Some(2), Some(2)),
];

fn config(concurrent: Option<u32>, per_hour: Option<u32>) -> Config {
    static BASE: std::sync::LazyLock<Config> =
        std::sync::LazyLock::new(|| patchula_config::defaults().expect("bundled config"));
    let mut config = BASE.clone();
    config.prs.concurrent = concurrent;
    config.prs.per_hour = per_hour;
    config
}

/// The narrowed reads modelled over the same history: the open PRs, the
/// hourly count only when `prs.perHour` is configured, and each branch
/// `closed_reads` names filled with its own closed unmerged PRs.
fn narrowed<'a>(
    all: &'a [Tracked],
    plans: &[Plan],
    controls: &Controls,
    per_hour: bool,
) -> Pulls<'a> {
    let mut pulls = Pulls::from_open(
        all.iter().filter(|t| t.pr.state == "open").collect(),
        per_hour.then(|| recent(all, OWNER, now())),
        true,
    );
    for head in closed_reads(&pulls, plans, controls, OWNER) {
        pulls.read_closed(
            head.clone(),
            Ok(all
                .iter()
                .filter(|t| {
                    t.pr.state == "closed" && t.pr.merged_at.is_none() && head_branch(&t.pr) == head
                })
                .collect()),
        );
    }
    pulls
}

/// What `admit` wrote into the plan: status, reason, which hold and retry.
fn outcome(plan: &Plan) -> (BranchStatus, Option<String>, Vec<bool>, bool) {
    (
        plan.action().status,
        plan.action().reason.clone(),
        [
            Hold::Window,
            Hold::Concurrent,
            Hold::PerHour,
            Hold::WaitingForBatch,
            Hold::Batched,
            Hold::ClosedByOwner,
            Hold::LockfilesCurrent,
            Hold::MaintenanceWindow,
            Hold::MaintenanceDone,
            Hold::Blocked(String::new()),
        ]
        .iter()
        .map(|h| plan.held(h))
        .collect(),
        plan.retry(),
    )
}

/// Assert the whole-history fill and the narrowed-read model decide
/// identically for `history`, including which open PRs would close as
/// obsolete.
fn run(history: &[Shape], ask: Ask, limits: (Option<u32>, Option<u32>), batched: bool) {
    let key = plan().update_key();
    let controls = controls(ask, &key, batched);
    let config = config(limits.0, limits.1);
    let all: Vec<Tracked> = history
        .iter()
        .enumerate()
        .map(|(i, s)| tracked(s, i as u64 + 1, &key))
        .collect();
    let context = format!("{history:?} {ask:?} {limits:?} batched={batched}");

    let mut whole_plans = vec![plan()];
    let whole = Pulls::from_listing(&all, &whole_plans, &controls, OWNER, now(), true);
    admit(&whole, &mut whole_plans, &controls, &config, OWNER);

    let mut narrowed_plans = vec![plan()];
    let narrowed_pulls = narrowed(
        &all,
        &narrowed_plans,
        &controls,
        config.prs.per_hour.is_some(),
    );
    admit(
        &narrowed_pulls,
        &mut narrowed_plans,
        &controls,
        &config,
        OWNER,
    );

    assert_eq!(
        outcome(&whole_plans[0]),
        outcome(&narrowed_plans[0]),
        "{context}"
    );
    let desired = BTreeSet::from([PLAN_BRANCH.to_owned()]);
    let numbers = |pulls: &Pulls<'_>| -> Vec<u64> {
        obsolete(pulls, &desired, &controls, OWNER)
            .iter()
            .map(|t| t.pr.number)
            .collect()
    };
    assert_eq!(numbers(&whole), numbers(&narrowed_pulls), "{context}");
}

/// One-PR histories, exhaustively: every shape against a planned, a
/// "bring back"-ticked and a "create"-ticked plan, with and without each
/// limit and batch membership.
#[test]
fn one_pr_histories() {
    for shape in shapes() {
        for ask in [Ask::Planned, Ask::BringBack, Ask::CreateNow] {
            for limits in LIMITS {
                for batched in [false, true] {
                    run(&[shape], ask, limits, batched);
                }
            }
        }
    }
}

/// Two-PR histories are sampled, the full product being the square of the
/// shape grid: the first PR walks every shape and the second the 64 shapes
/// a coprime stride reaches from it, so every field of one PR still meets
/// every field of the other (asserted below). The configured variants
/// then run a sixteenth of the same walk, since the only way the two
/// fills can differ is already pinned by `KEY_ON_OTHER_BRANCH`.
#[test]
fn two_pr_histories() {
    let grid = shapes();
    let n = grid.len();
    let mut seen: BTreeMap<(u8, u8), BTreeSet<(u8, u8)>> = BTreeMap::new();
    for (i, first) in grid.iter().enumerate() {
        for k in 0..64usize {
            let second = grid[(i + 41 * k) % n];
            let (one, two) = (fields(first), fields(&second));
            for (a, &fa) in one.iter().enumerate() {
                for (b, &fb) in two.iter().enumerate() {
                    seen.entry((a as u8, b as u8)).or_default().insert((fa, fb));
                }
            }
            run(&[*first, second], Ask::Planned, (None, None), false);
        }
    }
    // Every field of one PR met every field of the other.
    for a in 0..9u8 {
        for b in 0..9u8 {
            assert_eq!(
                seen[&(a, b)].len(),
                (ARITY[a as usize] * ARITY[b as usize]) as usize,
                "fields {a} and {b} did not meet in every combination"
            );
        }
    }
}

/// The same walk, a sixteenth of it, under every tick, limit and batch
/// membership.
#[test]
fn two_pr_histories_under_ticks_and_limits() {
    let grid = shapes();
    let n = grid.len();
    for i in (0..n).step_by(16) {
        for k in 0..64usize {
            let history = [grid[i], grid[(i + 41 * k) % n]];
            for ask in [Ask::Planned, Ask::BringBack, Ask::CreateNow] {
                for limits in LIMITS {
                    for batched in [false, true] {
                        run(&history, ask, limits, batched);
                    }
                }
            }
        }
    }
}

/// The one shape family the narrowed read decides differently — a closed
/// PR carrying the plan's key on another branch. The whole listing the
/// lifecycle reads today still suppresses the plan; the narrowed model
/// does not, which is the behaviour change item 3 makes. This pins
/// today's side; it flips when the closed read narrows.
#[test]
fn closed_key_on_another_branch_suppresses_today() {
    let shape = Shape {
        author: Author::Count,
        state: PrState::Closed,
        head: Head::Other,
        repo: HeadRepo::Same,
        marked: Marked::Plan,
        closure: Closure::None,
        items: Items::Absent,
        created: Created::Older,
        tick: Tick::Unticked,
    };
    let key = plan().update_key();
    let controls = Controls::default();
    let config = config(None, None);
    let all = vec![tracked(&shape, 1, &key)];

    let mut plans = vec![plan()];
    let whole = Pulls::from_listing(&all, &plans, &controls, OWNER, now(), true);
    admit(&whole, &mut plans, &controls, &config, OWNER);
    assert!(
        plans[0].held(&Hold::ClosedByOwner),
        "{:?}",
        outcome(&plans[0])
    );

    let mut plans = vec![plan()];
    let narrowed_pulls = narrowed(&all, &plans, &controls, false);
    admit(&narrowed_pulls, &mut plans, &controls, &config, OWNER);
    assert!(
        !plans[0].held(&Hold::ClosedByOwner),
        "{:?}",
        outcome(&plans[0])
    );
}

/// A branch whose closed PRs were never read, or read incompletely, is
/// `Unread` and `admit` protects the plan: a missing read can never come
/// out as a created PR.
#[test]
fn an_unread_closed_branch_protects_the_plan() {
    let mut plans = vec![plan()];
    let pulls = Pulls::from_open(Vec::new(), Some(0), true);
    admit(
        &pulls,
        &mut plans,
        &Controls::default(),
        &config(None, None),
        OWNER,
    );
    assert_eq!(plans[0].action().status, BranchStatus::Protected);
    assert_eq!(
        plans[0].action().reason.as_deref(),
        Some("closed pull requests for this branch could not be read; left untouched")
    );

    let mut plans = vec![plan()];
    let mut pulls = Pulls::from_open(Vec::new(), Some(0), true);
    pulls.read_closed(PLAN_BRANCH.to_owned(), Err(Unread));
    admit(
        &pulls,
        &mut plans,
        &Controls::default(),
        &config(None, None),
        OWNER,
    );
    assert_eq!(plans[0].action().status, BranchStatus::Protected);
}

/// An unreadable element anywhere in the listing protects every planned
/// plan, as it did inline before.
#[test]
fn an_incomplete_listing_protects_every_planned_plan() {
    for mut pulls in [
        Pulls::from_open(Vec::new(), Some(0), false),
        Pulls::from_listing(&[], &[plan()], &Controls::default(), OWNER, now(), false),
    ] {
        let mut plans = vec![plan()];
        pulls.read_closed(PLAN_BRANCH.to_owned(), Ok(Vec::new()));
        admit(
            &pulls,
            &mut plans,
            &Controls::default(),
            &config(None, None),
            OWNER,
        );
        assert_eq!(plans[0].action().status, BranchStatus::Protected);
        assert_eq!(
            plans[0].action().reason.as_deref(),
            Some("repository pull request list is incomplete; left untouched")
        );
    }
}
