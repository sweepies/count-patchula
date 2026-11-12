//! The write path as named steps over one repository's plans, one
//! authoritative inventory and one shared reconciliation tail.
use super::{Engine, Error, Result, Stage, Work, fail, lockfile_concurrency, stale};
use patchula_config::Config;
use patchula_manager::Dependency;
use patchula_report::{
    BranchAction, BranchStatus, PullRequestAction, PullRequestStatus, Repository, Status,
};
use patchula_update::dashboard::Controls;
use patchula_update::{Inventory, Lifecycle, Plan, RestoreOutcome, Settlement};

/// One run's write context: what every repository's pipeline shares.
pub(super) struct Write<'a> {
    engine: &'a Engine,
    now: chrono::DateTime<chrono::Utc>,
    dry_run: bool,
    allow_lockfile_pending: bool,
    note_sources: patchula_notes::Client,
    pub(super) branches: Vec<BranchAction>,
    pub(super) pull_requests: Vec<PullRequestAction>,
}

/// A write step's failure, with the severity the report gives it declared
/// once per step. `Repo` fails the repository; `Stale` marks it stale for
/// the rerun that re-reads what changed; `Warning` leaves it complete.
/// Any of them ends the repository's write here — a step whose failure must
/// not end it carries the failure inside its result instead.
pub(super) enum StepFailure {
    Repo(String),
    Stale(String),
    Warning(String),
}

/// An update error's declared severity: a lost race with the repository is
/// stale — nothing was written against the changed state and a rerun
/// retries it — and anything else is the repository's failure.
impl From<patchula_update::Error> for StepFailure {
    fn from(error: patchula_update::Error) -> Self {
        match error {
            patchula_update::Error::Raced(reason) => Self::Stale(reason.into()),
            error => Self::Repo(error.to_string()),
        }
    }
}

/// Applies a step's declared severity.
pub(super) fn failed(repo: &mut Repository, failure: StepFailure) {
    match failure {
        StepFailure::Repo(reason) => fail(repo, reason),
        StepFailure::Stale(reason) => stale(repo, reason),
        StepFailure::Warning(reason) => repo.warnings.push(reason),
    }
}

/// The dashboard's failure fails the repository, after whatever earlier
/// failure the caller checked first so the first failure keeps its reason.
fn dashboard_failed(repo: &mut Repository, settlement: &Settlement) {
    if let Err(error) = &settlement.dashboard {
        fail(repo, format!("dependency dashboard: {error}"));
    }
}

impl Write<'_> {
    pub(super) fn new(
        engine: &Engine,
        now: chrono::DateTime<chrono::Utc>,
        dry_run: bool,
        allow_lockfile_pending: bool,
    ) -> Write<'_> {
        Write {
            engine,
            now,
            dry_run,
            allow_lockfile_pending,
            note_sources: patchula_notes::Client::new(engine.sources.clone()),
            branches: Vec::new(),
            pull_requests: Vec::new(),
        }
    }

    /// One repository's write: plan it, settle it from receipts or write it,
    /// then reconcile through the shared tail.
    pub(super) async fn repository(&mut self, work: &mut Work) -> Result<()> {
        let Some(config) = work.config.take() else {
            return Ok(());
        };
        if work.report.status != Status::Complete {
            return Ok(());
        }
        let mut controls = std::mem::take(&mut work.controls);
        let mut snapshot = std::mem::take(&mut work.snapshot);
        let inventory = work.inventory.take();
        let dependencies: Vec<Dependency> = work
            .dependencies
            .iter()
            .map(|dep| dep.dependency.clone())
            .collect();
        let repo = &mut work.report;
        let since = self.engine.last_run(&repo.repository).await?;
        let mut plans = match self.plans(repo, &config, &dependencies, &snapshot, since) {
            Ok(plans) => plans,
            Err(failure) => {
                failed(repo, failure);
                return Ok(());
            }
        };
        self.maintenance(repo, &config, &snapshot, &mut plans, since)
            .await?;
        // Every write needs the inventory read during lookup; a missing one
        // is a broken invariant, failed closed rather than assumed away.
        let inventory = if self.dry_run {
            None
        } else {
            match inventory.as_ref() {
                Some(inventory) => Some(inventory),
                None => {
                    fail(repo, "write inventory missing");
                    return Ok(());
                }
            }
        };
        if let Some(inventory) = inventory {
            if let Err(failure) = self.controls(
                repo,
                &config,
                &dependencies,
                &snapshot,
                &mut plans,
                &mut controls,
                inventory,
                since,
            ) {
                failed(repo, failure);
                return Ok(());
            }
            self.tooling(repo, &config, &snapshot, inventory, &mut plans);
            match self.restore(repo, &config, &mut plans, inventory).await {
                Ok(RestoreOutcome::Complete) => {
                    let settlement = self
                        .settle(repo, &config, inventory, &mut plans, None)
                        .await;
                    if settlement.merge_actions.iter().any(|a| {
                        matches!(
                            a.status,
                            PullRequestStatus::Error | PullRequestStatus::Protected
                        )
                    }) {
                        fail(repo, "auto-merge failed or was protected; inspect actions");
                    }
                    dashboard_failed(repo, &settlement);
                    // A raced auto-merge still makes the repository stale
                    // once failures have had their say: a rerun retries it.
                    if let Some(reason) = settlement
                        .merge_actions
                        .iter()
                        .find(|a| a.status == PullRequestStatus::Stale)
                        .and_then(|a| a.reason.clone())
                    {
                        stale(repo, reason);
                    }
                    self.pull_requests.extend(
                        plans
                            .iter()
                            .filter(|p| {
                                !settlement
                                    .merge_actions
                                    .iter()
                                    .any(|a| a.branch == p.action().branch)
                            })
                            .filter_map(|p| {
                                p.restored_pr().map(|tracked| {
                                    patchula_update::pull_requests::unchanged(
                                        &repo.repository,
                                        &p.action().branch,
                                        &tracked.pr,
                                    )
                                })
                            }),
                    );
                    self.pull_requests.extend(settlement.merge_actions);
                    self.branches
                        .extend(plans.into_iter().map(|p| p.into_action()));
                    return Ok(());
                }
                Ok(RestoreOutcome::Partial) => {}
                Err(failure) => {
                    failed(repo, failure);
                    return Ok(());
                }
            }
        }
        let mut lifecycle = if let Some(inventory) = inventory {
            match self
                .lifecycle(repo, &config, &mut plans, &controls, inventory)
                .await
            {
                Ok(state) => Some(state),
                Err(failure) => {
                    failed(repo, failure);
                    return Ok(());
                }
            }
        } else {
            self.pull_requests.extend(
                plans
                    .iter()
                    .filter(|p| p.action().status == BranchStatus::Planned)
                    .map(|p| PullRequestAction {
                        repository: repo.repository.clone(),
                        branch: p.action().branch.clone(),
                        number: None,
                        url: None,
                        status: PullRequestStatus::Planned,
                        reason: Some(
                            "create or reconcile after authoritative live inventory".into(),
                        ),
                    }),
            );
            None
        };
        if inventory.is_some() {
            self.lockfiles(&mut plans, &mut snapshot, &config).await;
        }
        let cleanup = match (self.dry_run, repo.degraded(), inventory) {
            (false, false, Some(inventory)) => self.cleanup(repo, &plans, inventory).await,
            _ => Ok(Vec::new()),
        };
        match cleanup {
            Ok(actions) => self.branches.extend(actions),
            Err(failure) => {
                failed(repo, failure);
                return Ok(());
            }
        }
        if !self.dry_run {
            self.engine.progress.stage(Stage::Writing, plans.len());
        }
        if let Some(inventory) = inventory {
            self.publish(
                &mut plans,
                &snapshot,
                repo.default_branch.as_deref().expect("configured branch"),
                &inventory.actor,
                config.prs.rebase,
            )
            .await;
        }
        if let Some(inventory) = inventory
            && let Some(state) = lifecycle.as_mut()
        {
            state
                .publish_with_sources(&self.engine.github, &self.note_sources, &mut plans)
                .await;
            let settlement = self
                .settle(repo, &config, inventory, &mut plans, Some(&mut *state))
                .await;
            dashboard_failed(repo, &settlement);
            if state.actions.iter().any(|a| {
                matches!(
                    a.status,
                    PullRequestStatus::Error | PullRequestStatus::Protected
                )
            }) {
                fail(
                    repo,
                    "one or more pull request actions failed or were protected; inspect actions",
                );
            }
            self.pull_requests.append(&mut state.actions);
        }
        for plan in &plans {
            if matches!(
                plan.action().status,
                BranchStatus::Error | BranchStatus::Protected
            ) {
                fail(
                    repo,
                    "one or more branch actions failed or were protected; inspect actions",
                );
            }
        }
        // Failures and protections have had their say; any action that lost a
        // race with the repository still makes it stale for the rerun, and
        // the first such action names what changed. `stale` itself never
        // overrides a failure.
        let stale_reason = plans
            .iter()
            .find(|p| p.action().status == BranchStatus::Stale)
            .and_then(|p| p.action().reason.clone())
            .or_else(|| {
                self.pull_requests
                    .iter()
                    .filter(|a| a.repository == repo.repository)
                    .find(|a| a.status == PullRequestStatus::Stale)
                    .and_then(|a| a.reason.clone())
            });
        if let Some(reason) = stale_reason {
            stale(repo, reason);
        }
        self.branches
            .extend(plans.into_iter().map(|p| p.into_action()));
        Ok(())
    }

    /// The run's schedule turn for every repository that stayed complete,
    /// including ones settled from their write receipts.
    pub(super) async fn record_runs(&self, work: &mut [Work]) {
        if self.dry_run {
            return;
        }
        for work in work
            .iter_mut()
            .filter(|work| work.report.status == Status::Complete)
        {
            if let Err(error) = self
                .engine
                .record_run(&work.report.repository, self.now)
                .await
            {
                failed(
                    &mut work.report,
                    StepFailure::Warning(format!("last run cache: {error}")),
                );
            }
        }
    }

    /// The repository's branch plans from its report row.
    fn plans(
        &self,
        repo: &Repository,
        config: &Config,
        dependencies: &[Dependency],
        snapshot: &patchula_update::Snapshot,
        since: Option<chrono::DateTime<chrono::Utc>>,
    ) -> std::result::Result<Vec<Plan>, StepFailure> {
        patchula_update::plan(
            repo,
            config,
            dependencies,
            snapshot,
            self.allow_lockfile_pending,
            since,
            self.now,
        )
        .map_err(|e| StepFailure::Repo(e.to_string()))
    }

    /// The lockfile-maintenance plan, when one is due. A cache or config
    /// failure here fails the run, not the repository.
    async fn maintenance(
        &self,
        repo: &mut Repository,
        config: &Config,
        snapshot: &patchula_update::Snapshot,
        plans: &mut Vec<Plan>,
        since: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<()> {
        let Some(mut maintenance) =
            patchula_update::maintenance::maintenance(repo, config, snapshot, since, self.now)
                .map_err(|e| Error::Invalid(e.to_string()))?
        else {
            return Ok(());
        };
        patchula_update::maintenance::defer_completed_refresh(
            &self.engine.cache,
            &mut maintenance,
            config,
            self.now,
        )
        .await?;
        plans.push(maintenance);
        Ok(())
    }

    /// Dashboard controls onto the plans, then the batch PR over the
    /// inventory's open owned pull requests.
    #[allow(clippy::too_many_arguments)]
    fn controls(
        &self,
        repo: &Repository,
        config: &Config,
        dependencies: &[Dependency],
        snapshot: &patchula_update::Snapshot,
        plans: &mut Vec<Plan>,
        controls: &mut Controls,
        inventory: &Inventory,
        since: Option<chrono::DateTime<chrono::Utc>>,
    ) -> std::result::Result<(), StepFailure> {
        controls.apply(plans, dependencies);
        match patchula_update::batch::apply(
            plans,
            repo,
            config,
            dependencies,
            snapshot,
            &inventory.pull_requests(&repo.repository),
            &inventory.actor,
            self.allow_lockfile_pending,
            since,
            self.now,
        ) {
            Ok(batched) => {
                controls.batched = batched;
                Ok(())
            }
            Err(error) => Err(StepFailure::Repo(error.to_string())),
        }
    }

    /// Tooling-analysis warnings onto the plans and the report.
    fn tooling(
        &self,
        repo: &mut Repository,
        config: &Config,
        snapshot: &patchula_update::Snapshot,
        inventory: &Inventory,
        plans: &mut [Plan],
    ) {
        let branch = repo.default_branch.clone().expect("configured branch");
        let evidence = patchula_update::tooling::Evidence::read(snapshot, &branch, inventory);
        repo.tooling = patchula_update::tooling::apply(&evidence, config, &branch, plans);
    }

    /// Settle what verified receipts already prove.
    async fn restore(
        &self,
        repo: &Repository,
        config: &Config,
        plans: &mut [Plan],
        inventory: &Inventory,
    ) -> std::result::Result<RestoreOutcome, StepFailure> {
        patchula_update::restore_inventory(
            &self.engine.cache,
            repo,
            config,
            plans,
            self.engine.recheck,
            self.engine.advertised_server.as_deref(),
            inventory,
        )
        .await
        .map_err(StepFailure::from)
    }

    /// The REST lifecycle view for a full write: every open and closed pull
    /// request, prepared against the plans.
    async fn lifecycle(
        &self,
        repo: &Repository,
        config: &Config,
        plans: &mut [Plan],
        controls: &Controls,
        inventory: &Inventory,
    ) -> std::result::Result<Lifecycle, StepFailure> {
        let mut state = Lifecycle::prepare_with_controls(
            &self.engine.github,
            repo,
            plans,
            config,
            self.now,
            self.engine.recheck,
            controls,
            &inventory.actor,
        )
        .await
        .map_err(StepFailure::from)?;
        state.set_advertised_server(self.engine.advertised_server.clone());
        state.set_merge_settings(inventory.merge_settings.clone());
        Ok(state)
    }

    /// Generate lockfiles for the plans that need them.
    async fn lockfiles(
        &self,
        plans: &mut [Plan],
        snapshot: &mut patchula_update::Snapshot,
        config: &Config,
    ) {
        if let Some(runner) = &self.engine.lockfiles {
            self.engine.progress.stage(Stage::Lockfiles, plans.len());
            patchula_update::lock_all(
                runner,
                plans,
                snapshot,
                config,
                lockfile_concurrency(),
                &|| self.engine.progress.advance(1),
            )
            .await;
        } else {
            for plan in plans.iter_mut() {
                if plan.lock_work().is_some() {
                    plan.error("lockfile generation requires a disk cache or an injected runner");
                }
            }
        }
    }

    /// Delete obsolete owned branches, leased on the default-branch head.
    async fn cleanup(
        &self,
        repo: &Repository,
        plans: &[Plan],
        inventory: &Inventory,
    ) -> std::result::Result<Vec<BranchAction>, StepFailure> {
        patchula_update::cleanup(
            &self.engine.github,
            &self.engine.cache,
            &repo.repository,
            repo.commit.as_deref().expect("extracted commit"),
            repo.default_branch.as_deref().expect("configured branch"),
            plans,
            inventory,
            self.dry_run,
        )
        .await
        .map_err(StepFailure::from)
    }

    /// Publish the planned branches: trees, commits and the leased ref move.
    async fn publish(
        &self,
        plans: &mut [Plan],
        snapshot: &patchula_update::Snapshot,
        default_branch: &str,
        actor: &str,
        rebase: patchula_config::Rebase,
    ) {
        patchula_update::apply_all(
            &self.engine.github,
            plans,
            snapshot,
            default_branch,
            actor,
            rebase,
            self.dry_run,
        )
        .await;
    }

    /// The shared tail both outcomes call: auto-merge, the dashboard and the
    /// write receipts over the run's one inventory.
    async fn settle(
        &self,
        repo: &mut Repository,
        config: &Config,
        inventory: &Inventory,
        plans: &mut [Plan],
        lifecycle: Option<&mut Lifecycle>,
    ) -> Settlement {
        let mut settlement = patchula_update::settle(
            &self.engine.github,
            &self.engine.cache,
            repo,
            config,
            inventory,
            plans,
            lifecycle,
            self.engine.recheck,
            self.engine.advertised_server.as_deref(),
            self.now,
        )
        .await;
        repo.warnings.append(&mut settlement.warnings);
        settlement
    }
}
