//! `admit` over the reads the view holds: a closed PR suppresses only the
//! plan on its own branch, and every read that is missing or incomplete
//! fails closed.
use super::*;
use crate::Change;
use crate::markers::Marker;
use crate::plan::testing::{self, Draft};
use patchula_github::model;
use patchula_report::BranchAction;
use patchula_words::Source;

const ACTOR: &str = "patchula[bot]";
const REPO: &str = "sweepies/count-patchula";
const PLAN_A_BRANCH: &str = "patchula/serde-1.x";
const PLAN_B_BRANCH: &str = "patchula/left-pad-1.x";
const OTHER_BRANCH: &str = "patchula/unrelated-1.x";
const OWNER: Owner<'static> = Owner {
    actor: ACTOR,
    repo: REPO,
};

/// A closed, unmerged PR of the Count's on `branch`, its ownership marker
/// carrying `key`.
fn closed(number: u64, branch: &str, key: &str) -> Tracked {
    Tracked::from(model::PullRequest {
        node_id: format!("PR_node{number}"),
        number,
        html_url: format!("https://github.com/{REPO}/pull/{number}"),
        state: "closed".into(),
        title: format!("pull {number}"),
        body: Some(markers::ownership(&Marker::new(
            "a".repeat(64),
            key.to_owned(),
        ))),
        user: Some(model::Actor {
            login: ACTOR.into(),
        }),
        draft: Some(false),
        labels: Vec::new(),
        merged_at: None,
        auto_merge: None,
        mergeable: None,
        mergeable_state: None,
        created_at: Some("2026-11-11T09:00:00Z".into()),
        updated_at: None,
        head: model::PullRequestHead {
            branch: branch.into(),
            sha: format!("{number}ead"),
            repo: Some(model::PullRequestRepository {
                full_name: REPO.to_owned(),
            }),
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
        ..testing::action(branch)
    })
    .change(Change {
        source: Source::Crates,
        file: "Cargo.toml".into(),
        value: Some(value.into()),
        version: Some(value.into()),
        release_notes: false,
        ..testing::change(package)
    })
    .build()
}

/// Two plans, each on its own branch with its own update key.
fn plans() -> Vec<Plan> {
    vec![
        plan_on(PLAN_A_BRANCH, "serde", "1.0.1"),
        plan_on(PLAN_B_BRANCH, "left-pad", "1.0.1"),
    ]
}

fn config(concurrent: Option<u32>, per_hour: Option<u32>) -> Config {
    let mut config = patchula_config::defaults().expect("bundled config");
    config.prs.concurrent = concurrent;
    config.prs.per_hour = per_hour;
    config
}

/// The view over `all` as the lifecycle's reads fill it: no open PRs, and
/// each branch `closed_reads` names answered with its own closed PRs.
fn read<'a>(all: &'a [Tracked], plans: &[Plan], controls: &Controls) -> Pulls<'a> {
    let mut pulls = Pulls::from_open(Vec::new(), Some(0), true);
    for head in closed_reads(&pulls, plans, controls, OWNER) {
        let prs = all.iter().filter(|t| head_branch(&t.pr) == head).collect();
        pulls.read_closed(head, Ok(prs));
    }
    pulls
}

/// What `admit` wrote into the plan: status, reason, the holds and retry.
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

/// A closed PR suppresses only the plan on its own branch whose key it
/// carries: plan A's key on another branch, or plan B's key on plan A's
/// branch, holds no plan.
#[test]
fn a_closed_pull_suppresses_only_its_own_branch_and_key() {
    let a = plans()[0].update_key();
    let b = plans()[1].update_key();
    for pr in [closed(1, OTHER_BRANCH, &a), closed(1, PLAN_A_BRANCH, &b)] {
        let all = [pr];
        let mut plans = plans();
        let pulls = read(&all, &plans, &Controls::default());
        admit(
            &pulls,
            &mut plans,
            &Controls::default(),
            &config(None, None),
            OWNER,
        );
        for plan in &plans {
            assert!(
                !plan.held(&Hold::ClosedByOwner) && plan.action().status == BranchStatus::Planned,
                "{:?}",
                outcome(plan)
            );
        }
    }
    // On its own branch with its own key, it holds that plan alone.
    let all = [closed(1, PLAN_A_BRANCH, &a)];
    let mut plans = plans();
    let pulls = read(&all, &plans, &Controls::default());
    admit(
        &pulls,
        &mut plans,
        &Controls::default(),
        &config(None, None),
        OWNER,
    );
    assert!(plans[0].held(&Hold::ClosedByOwner));
    assert!(!plans[1].held(&Hold::ClosedByOwner));
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
    let controls = Controls::default();
    let config = config(None, None);
    // Plan B's branch carries B's closed suppressor; plan A's read failed.
    let all = vec![closed(1, PLAN_B_BRANCH, &plans()[1].update_key())];

    let mut unread = plans();
    let mut pulls = Pulls::from_open(Vec::new(), Some(0), true);
    pulls.read_closed(PLAN_A_BRANCH.to_owned(), Err(Unread));
    pulls.read_closed(PLAN_B_BRANCH.to_owned(), Ok(all.iter().collect()));
    admit(&pulls, &mut unread, &controls, &config, OWNER);
    assert_eq!(unread[0].action().status, BranchStatus::Protected);
    assert!(unread[1].held(&Hold::ClosedByOwner));

    // Plan B decides exactly as it does when plan A's branch read too.
    let mut both = plans();
    let pulls = read(&all, &both, &controls);
    admit(&pulls, &mut both, &controls, &config, OWNER);
    assert_eq!(outcome(&unread[1]), outcome(&both[1]));
}

/// An unreadable element anywhere in the reads protects every planned
/// plan.
#[test]
fn an_incomplete_read_protects_every_planned_plan() {
    let mut pulls = Pulls::from_open(Vec::new(), Some(0), false);
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

/// One pull request in a history: who opened it, its state, its head
/// branch, the update key its ownership marker carries and how it closed.
#[derive(Clone, Copy, Debug)]
struct Shape {
    count: bool,
    state: &'static str,
    head: &'static str,
    key: Option<usize>,
    closure: Option<&'static str>,
}

/// Every combination of author, state (merged is closed with
/// `merged_at`), head, marker key (plan A's, plan B's or none) and closure.
fn shapes() -> Vec<Shape> {
    let mut out = Vec::new();
    for count in [true, false] {
        for state in ["open", "closed", "merged"] {
            for head in [PLAN_A_BRANCH, PLAN_B_BRANCH, OTHER_BRANCH] {
                for key in [None, Some(0), Some(1)] {
                    for closure in [None, Some(markers::OBSOLETE), Some(markers::BATCHED)] {
                        out.push(Shape {
                            count,
                            state,
                            head,
                            key,
                            closure,
                        });
                    }
                }
            }
        }
    }
    out
}

fn shaped(shape: Shape, number: u64, keys: &[String]) -> Tracked {
    let mut pr = closed(number, shape.head, "");
    let mut body = match shape.key {
        Some(plan) => markers::ownership(&Marker::new("a".repeat(64), keys[plan].clone())),
        None => "a pull request body".into(),
    };
    if let Some(closure) = shape.closure {
        body.push('\n');
        body.push_str(closure);
    }
    pr.pr.body = Some(body);
    pr.pr.state = if shape.state == "open" {
        "open"
    } else {
        "closed"
    }
    .into();
    pr.pr.merged_at = (shape.state == "merged").then(|| "2026-11-10T12:00:00Z".into());
    if !shape.count {
        pr.pr.user = Some(model::Actor {
            login: "renovate[bot]".into(),
        });
    }
    pr
}

/// `admit` consults only the closed reads `closed_reads` names. Over every
/// one-PR history and a spread of two-PR histories, with and without a
/// "bring back" tick, the view filled exactly as the lifecycle fills it
/// decides the same as one that also answers every other head with all of
/// that head's closed PRs.
#[test]
fn admit_consults_only_the_closed_reads_it_selects() {
    let keys: Vec<String> = plans().iter().map(Plan::update_key).collect();
    let grid = shapes();
    let mut histories: Vec<Vec<Shape>> = grid.iter().map(|s| vec![*s]).collect();
    for (i, first) in grid.iter().enumerate() {
        for k in 0..6 {
            histories.push(vec![*first, grid[(i * 7 + k * 13 + 1) % grid.len()]]);
        }
    }
    let mut recreate = Controls::default();
    recreate.recreate.extend(keys.iter().cloned());
    let config = config(None, None);
    for history in &histories {
        let all: Vec<Tracked> = history
            .iter()
            .enumerate()
            .map(|(i, shape)| shaped(*shape, i as u64 + 1, &keys))
            .collect();
        for controls in [&Controls::default(), &recreate] {
            let open: Vec<&Tracked> = all.iter().filter(|t| t.pr.state == "open").collect();
            let closed_on = |head: &str| -> Vec<&Tracked> {
                all.iter()
                    .filter(|t| {
                        t.pr.state == "closed"
                            && t.pr.merged_at.is_none()
                            && head_branch(&t.pr) == head
                    })
                    .collect()
            };
            let decide = |extra: bool| {
                let mut plans = plans();
                let mut pulls = Pulls::from_open(open.clone(), Some(0), true);
                let selected = closed_reads(&pulls, &plans, controls, OWNER);
                for head in [PLAN_A_BRANCH, PLAN_B_BRANCH, OTHER_BRANCH] {
                    if extra || selected.iter().any(|s| s == head) {
                        pulls.read_closed(head.into(), Ok(closed_on(head)));
                    }
                }
                admit(&pulls, &mut plans, controls, &config, OWNER);
                plans.iter().map(outcome).collect::<Vec<_>>()
            };
            assert_eq!(decide(false), decide(true), "{history:?}");
        }
    }
}
