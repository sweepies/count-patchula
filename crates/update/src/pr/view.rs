//! The pull request reads the lifecycle made and the admission decisions
//! they feed. Pure: no client, no cache and no clock — the view answers
//! only what a read supplied, so a read that never ran is an `Unread`
//! error the caller fails closed on, never a guess.
use crate::Plan;
use crate::dashboard::Controls;
use crate::markers;
use crate::plan::Hold;
use crate::pull_requests::{
    Tracked, body, branch as head_branch, checked, owned_pr, same_repo, suppresses,
};
use chrono::{DateTime, Duration, Utc};
use patchula_config::Config;
use patchula_report::BranchStatus;
use std::collections::{BTreeMap, BTreeSet};

/// Whose pull requests the view decides over: the run's one actor and the
/// repository's `owner/name`.
#[derive(Clone, Copy)]
pub(crate) struct Owner<'a> {
    /// The authenticated account's login.
    pub actor: &'a str,
    /// The repository's `owner/name`.
    pub repo: &'a str,
}

/// A read the view needed did not run, or did not return every element.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Unread;

/// The pull requests the reads supplied, kept apart by which read answered
/// them so a decision can only ask for what was fetched.
pub(crate) struct Pulls<'a> {
    /// The open pull requests.
    open: Vec<&'a Tracked>,
    /// What each head branch's closed read returned, `Err(Unread)` when the
    /// read failed or came back incomplete. Filled for exactly the branches
    /// [`closed_reads`] names.
    closed: BTreeMap<String, Result<Vec<&'a Tracked>, Unread>>,
    /// Owned PRs created in the last hour; `None` when that read did not run.
    recent: Option<u32>,
    /// The reads that filled the view decoded every element; `false` when
    /// one was unreadable and no set-wide question may be answered.
    complete: bool,
}

impl<'a> Pulls<'a> {
    /// A view over `open` PRs alone; the closed branches [`closed_reads`]
    /// names are then recorded with [`Pulls::read_closed`], and `recent` is
    /// the hourly count the recent read supplied (`None`: it did not run).
    pub(crate) fn from_open(open: Vec<&'a Tracked>, recent: Option<u32>, complete: bool) -> Self {
        Self {
            open,
            closed: BTreeMap::new(),
            recent,
            complete,
        }
    }

    /// Record what `head`'s closed read returned: the PRs, or `Unread` for
    /// a failed or incomplete read.
    pub(crate) fn read_closed(&mut self, head: String, prs: Result<Vec<&'a Tracked>, Unread>) {
        self.closed.insert(head, prs);
    }

    /// The open PRs headed for `branch` in `repo`, in listing order — a PR
    /// on the branch from another fork is not the plan's to read.
    pub(crate) fn open_on(&self, branch: &str, repo: &str) -> Vec<&'a Tracked> {
        self.open
            .iter()
            .filter(|t| head_branch(&t.pr) == branch && same_repo(&t.pr, repo))
            .copied()
            .collect()
    }

    /// The open PRs the Count owns.
    pub(crate) fn owned_open<'b>(
        &'b self,
        owner: Owner<'b>,
    ) -> impl Iterator<Item = &'a Tracked> + 'b {
        self.open
            .iter()
            .filter(move |t| owned_pr(&t.pr, owner.actor, owner.repo))
            .copied()
    }

    /// The closed unmerged PR keeping `plan` closed: the Count's, still
    /// suppressing and carrying the plan's key. `Err(Unread)` when the
    /// branch's closed PRs were not read, or not all of them.
    pub(crate) fn closed_for(
        &self,
        plan: &Plan,
        owner: Owner<'_>,
    ) -> Result<Option<&'a Tracked>, Unread> {
        let prs = self
            .closed
            .get(plan.action().branch.as_str())
            .ok_or(Unread)?
            .as_ref()
            .map_err(|e| *e)?;
        let key = plan.update_key();
        Ok(prs.iter().copied().find(|t| {
            t.pr.state == "closed"
                && t.pr.merged_at.is_none()
                && owned_pr(&t.pr, owner.actor, owner.repo)
                && suppresses(&t.pr)
                && markers::ownership_marker(body(&t.pr)).is_some_and(|m| m.update == key)
        }))
    }
}

/// The head branches whose closed PRs [`admit`] will ask
/// [`Pulls::closed_for`] about: planned plans with no open PR on the
/// branch and no "bring back" tick.
pub(crate) fn closed_reads(
    pulls: &Pulls<'_>,
    plans: &[Plan],
    controls: &Controls,
    owner: Owner<'_>,
) -> Vec<String> {
    plans
        .iter()
        .filter(|p| {
            p.action().status == BranchStatus::Planned
                && pulls.open_on(&p.action().branch, owner.repo).is_empty()
                && !controls.recreate.contains(&p.update_key())
        })
        .map(|p| p.action().branch.clone())
        .collect()
}

/// The open owned PRs whose branches no plan wants and whose updates the
/// batch does not carry — the obsolete-closure targets, in listing order.
pub(crate) fn obsolete<'a>(
    pulls: &Pulls<'a>,
    desired: &BTreeSet<String>,
    controls: &Controls,
    owner: Owner<'_>,
) -> Vec<&'a Tracked> {
    pulls
        .owned_open(owner)
        .filter(|t| !desired.contains(head_branch(&t.pr)))
        .filter(|t| {
            !markers::ownership_marker(body(&t.pr))
                .is_some_and(|m| controls.batched.contains(&m.update))
        })
        .collect()
}

/// Owned PRs created in the last hour, the count the recent read supplies.
pub(crate) fn recent(all: &[Tracked], owner: Owner<'_>, now: DateTime<Utc>) -> u32 {
    counted(all, owner, now).count() as u32
}

/// The oldest `created_at` among the owned PRs `recent` counts — when it
/// leaves the window, an hour later, the hourly count first decays.
/// `None` when the count is zero.
pub(crate) fn recent_oldest(
    all: &[Tracked],
    owner: Owner<'_>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    counted(all, owner, now).min()
}

/// The `created_at` of every owned PR inside the hour window `recent`
/// counts.
fn counted<'a>(
    all: &'a [Tracked],
    owner: Owner<'a>,
    now: DateTime<Utc>,
) -> impl Iterator<Item = DateTime<Utc>> + 'a {
    all.iter().filter_map(move |t| {
        if !owned_pr(&t.pr, owner.actor, owner.repo) {
            return None;
        }
        t.pr.created_at
            .as_deref()
            .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
            .map(|at| at.with_timezone(&Utc))
            .filter(|at| *at >= now - Duration::hours(1))
    })
}

/// The lifecycle's admission decisions over the view: whether each planned
/// plan may publish this run. The view is the only thing read; the plans
/// are the only thing written — holds, protections and retry flags.
pub(crate) fn admit(
    pulls: &Pulls<'_>,
    plans: &mut [Plan],
    controls: &Controls,
    config: &Config,
    owner: Owner<'_>,
) {
    if !pulls.complete {
        // Every per-plan decision reads the whole listing: an unreadable
        // PR could hide the open, closed or recent pull a decision needs,
        // so no planned plan may create, update or hold.
        for plan in plans
            .iter_mut()
            .filter(|p| p.action().status == BranchStatus::Planned)
        {
            plan.protect("repository pull request list is incomplete; left untouched");
        }
        return;
    }
    let mut active = pulls.owned_open(owner).count() as u32;
    // An hourly count the read never supplied reads as the limit reached.
    let mut hourly = pulls.recent.unwrap_or(u32::MAX);
    for plan in plans
        .iter_mut()
        .filter(|p| p.action().status == BranchStatus::Planned)
    {
        let existing = pulls.open_on(&plan.action().branch, owner.repo);
        if existing.len() > 1
            || existing
                .first()
                .is_some_and(|t| !owned_pr(&t.pr, owner.actor, owner.repo))
        {
            plan.protect("unowned or ambiguous open PR for branch");
            continue;
        }
        let key = plan.update_key();
        if existing.is_empty() && !controls.recreate.contains(&key) {
            match pulls.closed_for(plan, owner) {
                Ok(Some(_)) => {
                    plan.hold(Hold::ClosedByOwner);
                    continue;
                }
                Err(Unread) => {
                    plan.protect(
                        "closed pull requests for this branch could not be read; left untouched",
                    );
                    continue;
                }
                Ok(None) => {}
            }
        }
        if let Some(existing) = existing.first() {
            // A newer target must not take over a PR that just moved into
            // the batch; it gets its own PR once that one closes.
            if markers::ownership_marker(body(&existing.pr))
                .is_some_and(|m| controls.batched.contains(&m.update))
            {
                plan.hold(Hold::WaitingForBatch);
                continue;
            }
            if checked(&existing.pr) {
                plan.request_retry();
            }
            continue;
        }
        // Security updates never queue behind ordinary ones. Nor does the
        // batch PR: it replaces the open PRs it carries, which still count
        // until it exists, so holding it would keep them open for good.
        let bypass = controls.create.contains(&key)
            || controls.recreate.contains(&key)
            || plan.batch_items().is_some()
            || plan.changes().iter().any(|c| !c.advisories.is_empty());
        if !bypass && config.prs.concurrent.is_some_and(|n| active >= n) {
            plan.hold(Hold::Concurrent);
            continue;
        }
        if !bypass && config.prs.per_hour.is_some_and(|n| hourly >= n) {
            plan.hold(Hold::PerHour);
            continue;
        }
        active = active.saturating_add(1);
        hourly = hourly.saturating_add(1);
    }
}

#[cfg(test)]
mod oracle;
