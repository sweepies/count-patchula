//! A `Plan` builder for tests, behind the `testing` feature: tests draft the
//! plan they need instead of poking fields, so the fields can go private
//! without touching them again.
use super::{Change, Kind, Plan, Stage};
use crate::batch;
use patchula_lockfiles as contract;
use patchula_report::{BranchAction, FileAction};
use std::collections::{BTreeMap, BTreeSet};

/// The draft's lockfile jobs, moving the plan to `Built` when a later stage
/// was drafted — the plan owes the tools again.
fn owed(plan: &mut Plan) -> &mut Vec<contract::Job> {
    if !matches!(plan.stage, Stage::Built { .. }) {
        plan.stage = Stage::Built { jobs: Vec::new() };
    }
    match &mut plan.stage {
        Stage::Built { jobs } => jobs,
        _ => unreachable!(),
    }
}
/// The draft's deleted set, moving the plan to `Generated` when an earlier
/// stage was drafted — the tools' output replaces what they still owed.
fn removed(plan: &mut Plan) -> &mut BTreeSet<String> {
    if !matches!(plan.stage, Stage::Generated { .. }) {
        plan.stage = Stage::Generated {
            deleted: BTreeSet::new(),
        };
    }
    match &mut plan.stage {
        Stage::Generated { deleted } => deleted,
        _ => unreachable!(),
    }
}

/// A plan under construction. `build` returns the plan exactly as drafted:
/// nothing is recomputed, so an explicit or absent fingerprint reaches
/// commit trailers and transcripts as it did when tests poked the field.
pub struct Draft {
    plan: Plan,
}
impl Draft {
    /// A plan for `action`'s branch, as [`Plan::new`] starts it: no edits,
    /// no results, an empty fingerprint.
    pub fn new(action: BranchAction) -> Self {
        Self {
            plan: Plan::new(action),
        }
    }
    /// A draft of an already-built plan, to alter it.
    pub fn from(plan: Plan) -> Self {
        Self { plan }
    }
    /// An ordinary update plan, not a maintenance one.
    pub fn update(mut self) -> Self {
        self.plan.kind = Kind::Update;
        self
    }
    /// A lockfile-maintenance plan.
    pub fn maintenance(mut self) -> Self {
        self.plan.kind = Kind::Maintenance;
        self
    }
    /// The batch PR, carrying `items`.
    pub fn batch(mut self, items: Vec<batch::Item>) -> Self {
        self.plan.kind = Kind::Batch(items);
        self
    }
    /// Record `change` as one of the plan's updates.
    pub fn change(mut self, change: Change) -> Self {
        self.plan.changes.push(change);
        self
    }
    /// The plan's whole change list.
    pub fn changes(mut self, changes: Vec<Change>) -> Self {
        self.plan.changes = changes;
        self
    }
    /// The draft's change list, for a test that edits a row in place.
    pub fn changes_mut(&mut self) -> &mut Vec<Change> {
        &mut self.plan.changes
    }
    /// A file the plan writes, with the report entry recording it.
    pub fn edit(
        mut self,
        path: impl Into<String>,
        content: impl Into<String>,
        file: FileAction,
    ) -> Self {
        self.plan.contents.insert(path.into(), content.into());
        self.plan.action.files.push(file);
        self
    }
    /// A file the plan writes.
    pub fn content(mut self, path: impl Into<String>, content: impl Into<String>) -> Self {
        self.plan.contents.insert(path.into(), content.into());
        self
    }
    /// The draft's contents, for a test that edits them in place.
    pub fn contents_mut(&mut self) -> &mut BTreeMap<String, String> {
        &mut self.plan.contents
    }
    /// The report entry for a file the plan changes, as the planner records
    /// it.
    pub fn file(mut self, file: FileAction) -> Self {
        self.plan.action.files.push(file);
        self
    }
    /// A path a lockfile run removed; the write drops it from the tree.
    pub fn delete(mut self, path: impl Into<String>) -> Self {
        removed(&mut self.plan).insert(path.into());
        self
    }
    /// The draft's deleted set, for a test that edits it in place.
    pub fn deleted_mut(&mut self) -> &mut BTreeSet<String> {
        removed(&mut self.plan)
    }
    /// The `actions.lock` refs a maintenance relock advanced.
    pub fn moved_refs(mut self, moved: Vec<contract::MovedRef>) -> Self {
        self.plan.moved_refs = moved;
        self
    }
    /// A lockfile job the plan still owes.
    pub fn lock_job(mut self, job: contract::Job) -> Self {
        owed(&mut self.plan).push(job);
        self
    }
    /// The plan's whole lockfile job set.
    pub fn lock_jobs(mut self, jobs: Vec<contract::Job>) -> Self {
        *owed(&mut self.plan) = jobs;
        self
    }
    /// The draft's lockfile jobs, for a test that edits one in place.
    pub fn lock_jobs_mut(&mut self) -> &mut Vec<contract::Job> {
        owed(&mut self.plan)
    }
    /// Whether the plan's lockfiles already ran.
    pub fn lockfiles_ready(mut self, ready: bool) -> Self {
        if ready {
            removed(&mut self.plan);
        } else {
            owed(&mut self.plan);
        }
        self
    }
    /// An explicit plan fingerprint; unset, the draft keeps what it was made
    /// with, so the bytes the trailer carries match the test's past.
    pub fn fingerprint(mut self, fingerprint: impl Into<String>) -> Self {
        self.plan.fingerprint = fingerprint.into();
        self
    }
    /// The fingerprint the plan's inputs produced, as the marker hashes it.
    pub fn input_fingerprint(mut self, fingerprint: impl Into<String>) -> Self {
        self.plan.input_fingerprint = fingerprint.into();
        self
    }
    /// The hash of the branch's identity, as the ownership marker names it.
    pub fn identity(mut self, identity: impl Into<String>) -> Self {
        self.plan.identity = identity.into();
        self
    }
    /// Whether a rerun writes the plan again.
    pub fn retry(mut self, retry: bool) -> Self {
        self.plan.retry = retry;
        self
    }
    /// The plan's branch.
    pub fn branch(mut self, branch: impl Into<String>) -> Self {
        self.plan.action.branch = branch.into();
        self
    }
    /// The extraction commit the plan's edits and write apply to.
    pub fn base(mut self, commit: impl Into<String>) -> Self {
        self.plan.action.base_commit = commit.into();
        self
    }
    /// The commit message the write uses.
    pub fn message(mut self, message: impl Into<String>) -> Self {
        self.plan.action.message = message.into();
        self
    }
    /// Whether the write is a manifest-only lockfile-pending test branch.
    pub fn lockfile_pending(mut self, pending: bool) -> Self {
        self.plan.action.lockfile_pending = pending;
        self
    }
    /// The plan as drafted.
    pub fn build(self) -> Plan {
        self.plan
    }
}
