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
const PLAN_A_BRANCH: &str = "patchula/serde-1.x";
const PLAN_B_BRANCH: &str = "patchula/left-pad-1.x";
const OTHER_BRANCH: &str = "patchula/unrelated-1.x";
const OWNER: Owner<'static> = Owner {
    actor: ACTOR,
    repo: REPO,
};

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-11T12:00:00Z")
        .expect("constant")
        .to_utc()
}

impl<'a> Pulls<'a> {
    /// The view the old `pulls?state=all` listing supplied: the open PRs,
    /// the hourly count of owned creations and, under every branch
    /// [`closed_reads`] names, the listing's whole set of closed unmerged
    /// PRs — which is what kept a closed PR carrying a plan's key on
    /// another branch suppressing. The oracle's whole-history side,
    /// against which the narrowed fill is checked.
    fn from_listing(
        all: &'a [Tracked],
        plans: &[Plan],
        controls: &Controls,
        owner: Owner<'_>,
        now: DateTime<Utc>,
        complete: bool,
    ) -> Self {
        let mut pulls = Self::from_open(
            all.iter().filter(|t| t.pr.state == "open").collect(),
            Some(recent(all, owner, now)),
            complete,
        );
        let closed: Vec<&Tracked> = all
            .iter()
            .filter(|t| t.pr.state == "closed" && t.pr.merged_at.is_none())
            .collect();
        for head in closed_reads(&pulls, plans, controls, owner) {
            pulls.read_closed(head, Ok(closed.clone()));
        }
        pulls
    }
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
/// The PR's head branch: one plan's or another's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Head {
    PlanA,
    PlanB,
    Other,
}
/// The PR's head repository: this one, a fork or deleted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HeadRepo {
    Same,
    Fork,
    Gone,
}
/// The ownership marker the body carries: none, one plan's key or another.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Marked {
    None,
    PlanA,
    PlanB,
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

/// The shapes the oracle excludes: a closed, unmerged, owned and still
/// suppressing PR carrying one plan's key anywhere but that plan's
/// branch. The whole listing suppressed the keyed plan on them; the
/// narrowed closed read does not — the accepted behaviour change the
/// `*_no_longer_suppresses` tests pin.
fn divergent(shape: &Shape) -> bool {
    let foreign = match shape.marked {
        Marked::PlanA => shape.head != Head::PlanA,
        Marked::PlanB => shape.head != Head::PlanB,
        _ => false,
    };
    shape.state == PrState::Closed
        && shape.author == Author::Count
        && shape.repo == HeadRepo::Same
        && shape.closure == Closure::None
        && shape.items == Items::Absent
        && foreign
}

/// Every shape, minus the [`divergent`] family. The ninth axis is the
/// re-check tick `checked` reads, which the design's list rolls into
/// "marker"; the other eight are its enumeration.
fn shapes() -> Vec<Shape> {
    let mut out = Vec::with_capacity(2 * 3 * 3 * 3 * 4 * 3 * 2 * 2 * 2);
    for author in [Author::Count, Author::Other] {
        for state in [PrState::Open, PrState::Closed, PrState::Merged] {
            for head in [Head::PlanA, Head::PlanB, Head::Other] {
                for repo in [HeadRepo::Same, HeadRepo::Fork, HeadRepo::Gone] {
                    for marked in [Marked::None, Marked::PlanA, Marked::PlanB, Marked::Other] {
                        for closure in [Closure::None, Closure::Obsolete, Closure::Batched] {
                            for items in [Items::Absent, Items::Present] {
                                for created in [Created::Recent, Created::Older] {
                                    for tick in [Tick::Unticked, Tick::Recheck] {
                                        let shape = Shape {
                                            author,
                                            state,
                                            head,
                                            repo,
                                            marked,
                                            closure,
                                            items,
                                            created,
                                            tick,
                                        };
                                        if !divergent(&shape) {
                                            out.push(shape);
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
const ARITY: [u8; 9] = [2, 3, 3, 3, 4, 3, 2, 2, 2];

/// The update keys the oracle's two plans carry — plan A's and plan B's.
struct Keys {
    a: String,
    b: String,
}
/// The key `marked` names: `Other` is a key no plan holds.
impl Keys {
    fn marked(&self, marked: Marked) -> Option<String> {
        match marked {
            Marked::None => None,
            Marked::PlanA => Some(self.a.clone()),
            Marked::PlanB => Some(self.b.clone()),
            Marked::Other => Some("b".repeat(64)),
        }
    }
}

fn body_for(shape: &Shape, keys: &Keys) -> String {
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
        _ => {
            body.push_str(&markers::ownership(&Marker::new(
                "a".repeat(64),
                keys.marked(shape.marked).expect("a marked body has a key"),
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

fn tracked(shape: &Shape, number: u64, keys: &Keys) -> Tracked {
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
        body: Some(body_for(shape, keys)),
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
                Head::PlanA => PLAN_A_BRANCH,
                Head::PlanB => PLAN_B_BRANCH,
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

/// One planned update on `branch`, updating `package` to `value`.
fn plan_on(branch: &str, package: &str, value: &str) -> Plan {
    Draft::new(BranchAction {
        repository: REPO.into(),
        branch: branch.into(),
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
        package: package.into(),
        registry: None,
        file: "Cargo.toml".into(),
        current: "1.0.0".into(),
        current_version: None,
        value: Some(value.into()),
        version: Some(value.into()),
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

/// The two plans the oracle runs against: each on its own branch, each
/// with its own update key.
fn plans() -> Vec<Plan> {
    vec![
        plan_on(PLAN_A_BRANCH, "serde", "1.0.1"),
        plan_on(PLAN_B_BRANCH, "left-pad", "1.0.1"),
    ]
}

fn keys() -> Keys {
    Keys {
        a: plan_on(PLAN_A_BRANCH, "serde", "1.0.1").update_key(),
        b: plan_on(PLAN_B_BRANCH, "left-pad", "1.0.1").update_key(),
    }
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

fn controls(ask: Ask, keys: &Keys, batched: bool) -> Controls {
    let mut controls = Controls::default();
    for key in [&keys.a, &keys.b] {
        match ask {
            Ask::Planned => {}
            Ask::BringBack => {
                controls.recreate.insert(key.clone());
            }
            Ask::CreateNow => {
                controls.create.insert(key.clone());
            }
        }
        if batched {
            controls.batched.insert(key.clone());
        }
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
/// identically for `history` over the two planned plans — each plan's
/// `active`/`hourly` fold and `closed_reads` spanning both branches —
/// including which open PRs would close as obsolete.
fn run(history: &[Shape], ask: Ask, limits: (Option<u32>, Option<u32>), batched: bool) {
    let keys = keys();
    let controls = controls(ask, &keys, batched);
    let config = config(limits.0, limits.1);
    let all: Vec<Tracked> = history
        .iter()
        .enumerate()
        .map(|(i, s)| tracked(s, i as u64 + 1, &keys))
        .collect();
    let context = format!("{history:?} {ask:?} {limits:?} batched={batched}");

    let mut whole_plans = plans();
    let whole = Pulls::from_listing(&all, &whole_plans, &controls, OWNER, now(), true);
    admit(&whole, &mut whole_plans, &controls, &config, OWNER);

    let mut narrowed_plans = plans();
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
        whole_plans.iter().map(outcome).collect::<Vec<_>>(),
        narrowed_plans.iter().map(outcome).collect::<Vec<_>>(),
        "{context}"
    );
    let desired = BTreeSet::from([PLAN_A_BRANCH.to_owned(), PLAN_B_BRANCH.to_owned()]);
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

/// One shape family the narrowed read decides differently — a closed PR
/// carrying a plan's key on a branch that is not the plan's. The whole
/// listing suppressed the keyed plan on it; the narrowed closed read does
/// not, which is the accepted behaviour change these tests pin.
fn run_divergent(shape: Shape, keyed: usize) {
    let keys = keys();
    let controls = Controls::default();
    let config = config(None, None);
    let all = vec![tracked(&shape, 1, &keys)];

    let mut whole_plans = plans();
    let whole = Pulls::from_listing(&all, &whole_plans, &controls, OWNER, now(), true);
    admit(&whole, &mut whole_plans, &controls, &config, OWNER);
    // The whole listing's set under every branch key found the keyed PR:
    // the plan whose key it carried was held closed-by-owner.
    assert!(
        whole_plans[keyed].held(&Hold::ClosedByOwner),
        "{shape:?}: {:?}",
        outcome(&whole_plans[keyed])
    );

    let mut narrowed_plans = plans();
    let narrowed_pulls = narrowed(&all, &narrowed_plans, &controls, false);
    admit(
        &narrowed_pulls,
        &mut narrowed_plans,
        &controls,
        &config,
        OWNER,
    );
    // The narrowed read answers only the plan's own branch: its key on
    // another branch holds nothing, and no plan is protected.
    for (i, plan) in narrowed_plans.iter().enumerate() {
        assert!(
            !plan.held(&Hold::ClosedByOwner) && plan.action().status == BranchStatus::Planned,
            "{shape:?} plan {i}: {:?}",
            outcome(plan)
        );
    }
}

/// The plan's key on a stranger branch: plan A is no longer held.
#[test]
fn a_closed_pull_with_plan_as_key_on_another_branch_no_longer_suppresses() {
    run_divergent(
        Shape {
            author: Author::Count,
            state: PrState::Closed,
            head: Head::Other,
            repo: HeadRepo::Same,
            marked: Marked::PlanA,
            closure: Closure::None,
            items: Items::Absent,
            created: Created::Older,
            tick: Tick::Unticked,
        },
        0,
    );
}

/// Plan B's key on plan A's branch: plan B is no longer held — the case a
/// branch rename in the same week made real.
#[test]
fn a_closed_pull_on_plan_as_branch_carrying_plan_bs_key_suppresses_neither() {
    run_divergent(
        Shape {
            author: Author::Count,
            state: PrState::Closed,
            head: Head::PlanA,
            repo: HeadRepo::Same,
            marked: Marked::PlanB,
            closure: Closure::None,
            items: Items::Absent,
            created: Created::Older,
            tick: Tick::Unticked,
        },
        1,
    );
}

/// `prs.perHour` set and no recent read (`recent: None`) must never come
/// out as capacity: `admit` reads the absent read as the limit reached
/// and every non-bypass plan holds `PerHour`. Production always runs the
/// read when the limit is configured; the fail-closed fold is pinned here.
#[test]
fn a_missing_recent_read_fails_closed_when_per_hour_is_set() {
    let mut plans = plans();
    let mut pulls = Pulls::from_open(Vec::new(), None, true);
    pulls.read_closed(PLAN_A_BRANCH.to_owned(), Ok(Vec::new()));
    pulls.read_closed(PLAN_B_BRANCH.to_owned(), Ok(Vec::new()));
    admit(
        &pulls,
        &mut plans,
        &Controls::default(),
        &config(None, Some(1)),
        OWNER,
    );
    for plan in &plans {
        assert!(plan.held(&Hold::PerHour), "{:?}", outcome(plan));
    }
}

/// A branch whose closed PRs were never read, or read incompletely, is
/// `Unread` and `admit` protects the plan: a missing read can never come
/// out as a created PR.
#[test]
fn an_unread_closed_branch_protects_the_plan() {
    let mut unread = plans();
    let pulls = Pulls::from_open(Vec::new(), Some(0), true);
    admit(
        &pulls,
        &mut unread,
        &Controls::default(),
        &config(None, None),
        OWNER,
    );
    for plan in &unread {
        assert_eq!(plan.action().status, BranchStatus::Protected);
        assert_eq!(
            plan.action().reason.as_deref(),
            Some("closed pull requests for this branch could not be read; left untouched")
        );
    }

    let mut partial = plans();
    let mut pulls = Pulls::from_open(Vec::new(), Some(0), true);
    pulls.read_closed(PLAN_A_BRANCH.to_owned(), Err(Unread));
    pulls.read_closed(PLAN_B_BRANCH.to_owned(), Ok(Vec::new()));
    admit(
        &pulls,
        &mut partial,
        &Controls::default(),
        &config(None, None),
        OWNER,
    );
    assert_eq!(partial[0].action().status, BranchStatus::Protected);
    assert_eq!(partial[1].action().status, BranchStatus::Planned);
}

/// A per-branch `Err(Unread)` inside a multi-branch run is per-branch:
/// plan A is protected while plan B's branch read answered, so plan B is
/// decided on its own merits — identical to its outcome with both reads.
#[test]
fn an_unreadable_branch_among_several_protects_only_its_plan() {
    let keys = keys();
    let controls = Controls::default();
    let config = config(None, None);
    // Plan B's branch carries B's closed suppressor; plan A's read failed.
    let suppressor = Shape {
        author: Author::Count,
        state: PrState::Closed,
        head: Head::PlanB,
        repo: HeadRepo::Same,
        marked: Marked::PlanB,
        closure: Closure::None,
        items: Items::Absent,
        created: Created::Older,
        tick: Tick::Unticked,
    };
    let all = vec![tracked(&suppressor, 1, &keys)];

    let mut unread = plans();
    let mut pulls = Pulls::from_open(Vec::new(), Some(0), true);
    pulls.read_closed(PLAN_A_BRANCH.to_owned(), Err(Unread));
    pulls.read_closed(PLAN_B_BRANCH.to_owned(), Ok(all.iter().collect()));
    admit(&pulls, &mut unread, &controls, &config, OWNER);
    assert_eq!(unread[0].action().status, BranchStatus::Protected);
    assert!(unread[1].held(&Hold::ClosedByOwner));

    // Plan B decides exactly as it does when plan A's branch read too.
    let mut read = plans();
    let narrowed_pulls = narrowed(&all, &read, &controls, false);
    admit(&narrowed_pulls, &mut read, &controls, &config, OWNER);
    assert_eq!(outcome(&unread[1]), outcome(&read[1]));
}

/// An unreadable element anywhere in the listing protects every planned
/// plan, as it did inline before.
#[test]
fn an_incomplete_listing_protects_every_planned_plan() {
    for mut pulls in [
        Pulls::from_open(Vec::new(), Some(0), false),
        Pulls::from_listing(&[], &plans(), &Controls::default(), OWNER, now(), false),
    ] {
        let mut plans = plans();
        pulls.read_closed(PLAN_A_BRANCH.to_owned(), Ok(Vec::new()));
        pulls.read_closed(PLAN_B_BRANCH.to_owned(), Ok(Vec::new()));
        admit(
            &pulls,
            &mut plans,
            &Controls::default(),
            &config(None, None),
            OWNER,
        );
        for plan in &plans {
            assert_eq!(plan.action().status, BranchStatus::Protected);
            assert_eq!(
                plan.action().reason.as_deref(),
                Some("repository pull request list is incomplete; left untouched")
            );
        }
    }
}
