//! `.github/workflows/actions.lock` through `gh actions-lock`. An update
//! runs only over the edited workflows the lock already lists — Renovate
//! runs the whole directory, which would let someone else's broken pin
//! hold the Count's update. Lockfile maintenance runs `--relock` over the
//! whole directory, which advances moved refs and prunes workflows that no
//! longer exist. `--no-onboard` and `--no-narrow` are never dropped: the
//! Count adds nothing to the lockfile and rewrites no `uses:` ref it did
//! not edit.
use super::*;

/// `gh-actions-lock`'s mise spec: its release assets are bare binaries
/// named by platform, so `bin` picks the one to install.
pub(super) const ACTIONS_LOCK_SPEC: &str = "github:github/gh-actions-lock[bin=gh-actions-lock]";

/// How the tool's stdout, stderr and exit status reach the run whole.
/// The process runner bounds a failed command's captured streams, and
/// the findings JSON decides the classification, so the shell keeps all
/// three in the driver directory for the run to read.
const SHELL: &str = concat!(
    "\"$0\" \"$@\" 1>\"$ACTIONS_LOCK_STDOUT\" 2>\"$ACTIONS_LOCK_STDERR\";",
    " printf %s \"$?\" >\"$ACTIONS_LOCK_STATUS\""
);

pub(super) static DESCRIPTOR: Tool = Tool {
    // A workflow can name private repositories, so its verdict is never
    // public-only and the run keeps the installation's own partition.
    steers: &[Manager::GithubActions],
    tools: &[ACTIONS_LOCK_SPEC],
    // The repository does not pin gh-actions-lock; `defaults.toml` does.
    versions: false,
    install: Install::Plain,
    // Workflows have no manifest names `validate_targets` can key on; the
    // run proves the lock with `--verify-local` and its own read-back.
    proof: Proof::None,
    run: Run::Shell,
    output: Output::Default,
};

/// The findings `--json` reports for a run. `findings` repeats every
/// workflow's findings at the top level, which is all the run reads.
#[derive(serde::Deserialize)]
struct Report {
    findings: Vec<Finding>,
}
/// One finding as the tool's `format.Finding` writes it: `remediation` and
/// `retryable` are `omitempty`, so absent means empty.
#[derive(serde::Deserialize)]
struct Finding {
    category: String,
    severity: String,
    detail: String,
    remediation: Option<String>,
    #[serde(default)]
    retryable: bool,
    workflow: Option<String>,
}

/// The entries a lockfile records: each workflow's `owner/repo@ref` list,
/// and every entry's resolved commit under `dependencies:`. The tool
/// leaves out either map when it is empty (`marshalDeterministic`): a lock
/// whose listed workflows use no remote action has no `dependencies:`.
#[derive(serde::Deserialize)]
struct Written {
    #[serde(default)]
    workflows: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    dependencies: BTreeMap<String, WrittenEntry>,
}
#[derive(serde::Deserialize)]
struct WrittenEntry {
    commit: Option<String>,
    /// The entries a composite action's steps use.
    #[serde(default)]
    uses: Vec<String>,
}

/// The hold for an action the lockfile does not list: onboarding it is the
/// owner's step, never the Count's.
const NOT_ONBOARDED: &str = "an action the lockfile does not list yet; the Count never onboards";

/// A lock key as the tool writes it: the canonical pin when it parses as
/// one, else the key itself.
fn canonical(key: &str) -> String {
    contract::actions_lock_pin(key).unwrap_or_else(|| key.to_owned())
}

/// A recorded commit without the `sha1-` prefix the tool may write.
fn commit_hex(commit: &str) -> &str {
    commit.strip_prefix("sha1-").unwrap_or(commit)
}

/// Parse `text` as a lockfile, naming it `what` when it does not parse.
fn written(text: &str, what: &str) -> Result<Written, Failure> {
    serde_yaml_ng::from_str(text)
        .map_err(|e| Failure::Text(format!("actions.lock: unparseable {what}: {e}")))
}

/// The base lockfile's text and the workflows it lists, each with its
/// entries.
fn listed(
    baseline: &BTreeMap<String, BaseFile>,
) -> Result<(&str, BTreeMap<String, BTreeSet<String>>), Failure> {
    let Some(base) = baseline.get(contract::ACTIONS_LOCK) else {
        return Err("actions.lock job without the lockfile".into());
    };
    let listed = contract::actions_lock_workflows(&base.content).ok_or_else(corrupt)?;
    Ok((&base.content, listed))
}

/// What the base lockfile cannot say means it is corrupt or newer than
/// this `gh-actions-lock` understands.
fn corrupt() -> Failure {
    Failure::Text(
        "actions.lock: a corrupt or newer lockfile; regenerate it with `gh actions-lock`".into(),
    )
}

/// Every secret form `secrets` holds removed from `text`, the masking a
/// tool failure would give it.
fn redact(secrets: &[String], text: &str) -> String {
    secrets.iter().fold(text.to_owned(), |text, secret| {
        text.replace(secret.as_str(), "***")
    })
}

/// A held run's reason: the first line names the hold, the rest is the
/// collapsed block the report shows.
fn held(reason: &str, detail: &str) -> Failure {
    Failure::Text(format!("actions.lock: {reason}\n{detail}"))
}

/// The finding that names the hold: a retryable error means GitHub was
/// unreachable or rate-limited and the update waits; otherwise the first
/// error the fix run could not have resolved — `ref-changed`, `stale`
/// and `not-pinned` are what the run rewrites, so the real cause sits
/// beside them.
fn classify(stdout: &str, waiting: &str) -> Result<Option<String>, String> {
    let report: Report = serde_json::from_str(stdout)
        .map_err(|e| format!("unparseable gh-actions-lock findings: {e}"))?;
    let detail = |finding: &Finding| {
        let mut detail = format!("{}: {}", finding.category, finding.detail);
        if let Some(workflow) = &finding.workflow {
            detail.push_str(&format!("\nworkflow: {workflow}"));
        }
        if let Some(remediation) = &finding.remediation {
            detail.push_str(&format!("\nremediation: {remediation}"));
        }
        detail
    };
    let blocking = |finding: &&Finding| finding.severity == "error";
    if let Some(finding) = report
        .findings
        .iter()
        .find(|finding| blocking(finding) && finding.retryable)
    {
        return Ok(Some(format!(
            "GitHub was unreachable or rate-limited; {waiting} waits for the next run\n{}",
            detail(finding)
        )));
    }
    Ok(report
        .findings
        .iter()
        .find(|finding| {
            blocking(finding)
                && !matches!(
                    finding.category.as_str(),
                    "ref-changed" | "stale" | "not-pinned"
                )
        })
        .map(|finding| format!("{}\n{}", reason(&finding.category), detail(finding))))
}

/// The hold's reason, phrased for the report; an unknown category reports
/// itself, which a `gh-actions-lock` bump can always add.
fn reason(category: &str) -> String {
    match category {
        // The pin the lock records is gone from upstream history.
        "unreachable-pin" => "a pin is no longer in upstream history".into(),
        // The repository is deleted, or the scoped token cannot read it:
        // public actions only until an operator setting names more.
        "repo-unavailable" => "a private action the token cannot read".into(),
        "onboarding-required" => NOT_ONBOARDED.into(),
        other => other.to_owned(),
    }
}

/// The last line of a failed tool's stderr — token refusal, a corrupt or
/// newer lockfile — as the hold's reason.
fn exit_two(stderr: &str) -> String {
    let line = stderr
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("gh-actions-lock failed");
    if line.contains("token") {
        "the token refused".into()
    } else if line.contains("lock") {
        format!("a corrupt or newer lockfile: {line}")
    } else {
        line.into()
    }
}

/// One `gh-actions-lock` invocation's captured result.
struct Outcome {
    code: u8,
    stdout: String,
    stderr: String,
}

/// What every `gh-actions-lock` invocation of one run shares: the pinned
/// version, the whitelisted environment with the granted token, and the
/// scratch directory the shell writes the streams and status to.
struct Session {
    version: String,
    env: Vec<(String, String)>,
    secrets: Vec<String>,
    scratch: PathBuf,
}

impl Session {
    /// The tool's arguments behind the shell that captures its streams.
    fn args(flags: &[&str], paths: &[String]) -> Vec<String> {
        ["-c", SHELL, "gh-actions-lock"]
            .into_iter()
            .chain(flags.iter().copied())
            .map(str::to_owned)
            .chain(paths.iter().cloned())
            .collect()
    }
    /// Run the tool with `args` from the tree root and read back what the
    /// shell captured, masked.
    async fn invoke(
        &self,
        runner: &Runner,
        driver: &Path,
        root: &Path,
        args: Vec<String>,
    ) -> Result<Outcome, Failure> {
        runner
            .tool(
                driver,
                Invocation {
                    descriptor: &DESCRIPTOR,
                    tools: vec![(ACTIONS_LOCK_SPEC.into(), self.version.clone())],
                    bin: caches::Bin::ActionsLock,
                    cwd: Some(root.to_path_buf()),
                    env: self.env.clone(),
                    credentials: Vec::new(),
                    secrets: self.secrets.clone(),
                    public: false,
                    args,
                },
            )
            .await?;
        let read = |name: &str| {
            std::fs::read_to_string(self.scratch.join(name))
                .map(|text| redact(&self.secrets, &text))
                .map_err(|e| Failure::Text(e.to_string()))
        };
        let code = read("status")?
            .trim()
            .parse::<u8>()
            .map_err(|e| Failure::Text(e.to_string()))?;
        Ok(Outcome {
            code,
            stdout: read("stdout")?,
            stderr: read("stderr")?,
        })
    }
}

impl Runner {
    /// The session for `repository`'s run: the pinned version, the token
    /// granted and masked, `gh` and `git` off the PATH and the tool on
    /// github.com.
    async fn actions_session(
        &self,
        driver: &Path,
        root: &Path,
        repository: &str,
    ) -> Result<Session, Failure> {
        let version = default_version(ACTIONS_LOCK_SPEC)?.to_owned();
        // The scratch directory the shell writes the run's streams and
        // status to, and an empty PATH: `gh` and `git` stay unreachable
        // whatever the sandbox has, and `GH_HOST` keeps the tool on
        // github.com anyway.
        let scratch = driver.join("actions-lock");
        let empty_path = scratch.join("path");
        std::fs::create_dir_all(&empty_path).map_err(|e| e.to_string())?;
        let token = self.actions_lock_token(repository).await?;
        tracing::info!(kind = ?token.kind, %repository, "actions.lock token granted");
        let mut env = vec![
            ("GH_HOST".into(), "github.com".into()),
            ("GH_REPO".into(), repository.into()),
            (
                "GH_ACTIONS_LOCK_REPO_ROOT".into(),
                root.to_string_lossy().into_owned(),
            ),
            ("CI".into(), "false".into()),
            ("PATH".into(), empty_path.to_string_lossy().into_owned()),
            (
                "ACTIONS_LOCK_STDOUT".into(),
                scratch.join("stdout").to_string_lossy().into_owned(),
            ),
            (
                "ACTIONS_LOCK_STDERR".into(),
                scratch.join("stderr").to_string_lossy().into_owned(),
            ),
            (
                "ACTIONS_LOCK_STATUS".into(),
                scratch.join("status").to_string_lossy().into_owned(),
            ),
        ];
        let mut secrets = Vec::new();
        token.grant(&mut env, &mut secrets);
        Ok(Session {
            version,
            env,
            secrets,
            scratch,
        })
    }

    /// Regenerate the listed workflows' `actions.lock` entries around the
    /// update's edits, or hold the update with the tool's reason. `unread`
    /// is the workflows and action files the checkout left out for not
    /// being regular files.
    pub(super) async fn actions_lock(
        &self,
        context: context::Context<'_>,
        work: &Work<'_>,
        unread: &BTreeSet<String>,
    ) -> Result<(), Failure> {
        let context::Context {
            driver,
            root,
            baseline,
            config,
            ..
        } = context;
        let (_, listed) = listed(baseline)?;
        // The edited workflows the lock lists, and the listed workflows
        // that call an edited in-repository action file through `$/`:
        // the tool locks a `$/` composite's remote `uses:` under each
        // caller, and never a `./` one. Callers are resolved in the
        // edited tree; what cannot be resolved holds the update, a file
        // the checkout left out included: the tool would follow a link
        // the tree does not hold.
        let keys: BTreeSet<String> = listed.keys().cloned().collect();
        let read = |path: &str| -> Result<Option<String>, String> {
            if unread.contains(path) {
                return Err("not a regular file".into());
            }
            match std::fs::read_to_string(root.join(path)) {
                Ok(text) => Ok(Some(text)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e.to_string()),
            }
        };
        let mut composites: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for path in work.edits.keys().filter(|path| {
            patchula_manager::roles::action_file(path)
                && !patchula_manager::roles::github_workflow(path)
        }) {
            let callers = contract::actions_lock_callers(path, &keys, &read)
                .map_err(|reason| held(&reason, ""))?;
            if !callers.is_empty() {
                composites.insert(path.clone(), callers);
            }
        }
        let scope: Vec<String> = work
            .edits
            .keys()
            .filter(|path| patchula_manager::roles::github_workflow(path) && keys.contains(*path))
            .cloned()
            .chain(composites.values().flatten().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if scope.is_empty() {
            return Ok(());
        }
        let session = self.actions_session(driver, root, work.repository).await?;
        let fix = session
            .invoke(self, driver, root, Session::args(&fix_flags(false), &scope))
            .await?;
        check(&fix, "the update")?;
        // `valid` on stdout is not proof: `--no-onboard` exits 0 while
        // refusing to cover an action. The tool's own coverage check runs
        // offline on the same paths, then the lock is read back against
        // the edit.
        verify(self, &session, driver, root, &scope).await?;
        prove(root, work, &scope, &composites, config)
    }

    /// Lockfile maintenance: `--relock` over the whole workflow directory,
    /// which advances the refs that moved upstream in the workflows the
    /// lock lists and prunes listed workflows that no longer exist. It
    /// never onboards, and it may change nothing but `actions.lock`:
    /// `before` is the tree as the run found it, and `unread` the
    /// workflows and action files the checkout left out for not being
    /// regular files. Answers the moved refs the regenerated lock now
    /// follows.
    pub(super) async fn actions_relock(
        &self,
        context: context::Context<'_>,
        work: &Work<'_>,
        before: &BTreeMap<String, String>,
        unread: &BTreeSet<String>,
    ) -> Result<Vec<contract::MovedRef>, Failure> {
        let context::Context {
            driver,
            root,
            baseline,
            ..
        } = context;
        let (base, listed) = listed(baseline)?;
        // Nothing listed is nothing to relock, and `--no-onboard` would
        // add nothing.
        if listed.is_empty() {
            return Ok(Vec::new());
        }
        // What the regenerated lock must list: every listed workflow the
        // repository still holds. The tool keeps exactly the workflows it
        // discovers directly under `.github/workflows/`; one the checkout
        // left out is still the repository's, so a relock that drops it
        // fails rather than publishes the drop.
        let kept: BTreeSet<String> = listed
            .into_keys()
            .filter(|path| {
                patchula_manager::roles::github_workflow(path)
                    && (before.contains_key(path) || unread.contains(path))
            })
            .collect();
        let session = self.actions_session(driver, root, work.repository).await?;
        // No paths: only a whole-directory run prunes deleted workflows.
        let fix = session
            .invoke(self, driver, root, Session::args(&fix_flags(true), &[]))
            .await?;
        check(&fix, "maintenance")?;
        // Workflows and action files stay byte-identical: a narrowing or
        // migration rewrite is never published by maintenance.
        let after = collect(root)?;
        if let Some(path) = before
            .keys()
            .chain(after.keys())
            .find(|path| *path != contract::ACTIONS_LOCK && before.get(*path) != after.get(*path))
        {
            return Err(held(
                "the tool modified a file other than actions.lock",
                &format!("tool modified unexpected file {path}"),
            ));
        }
        // The tool removes a lockfile it pruned empty. That is the change
        // only when every workflow the lock listed is gone.
        let Some(lock) = after.get(contract::ACTIONS_LOCK) else {
            if kept.is_empty() {
                return Ok(Vec::new());
            }
            return Err(held(
                "the relock removed the lockfile while workflows it lists remain",
                &kept.into_iter().collect::<Vec<_>>().join("\n"),
            ));
        };
        let relocked = contract::actions_lock_workflows(lock).ok_or_else(|| {
            Failure::Text("actions.lock: unparseable regenerated lockfile".into())
        })?;
        let relisted: BTreeSet<String> = relocked.into_keys().collect();
        if relisted != kept {
            let detail = kept
                .symmetric_difference(&relisted)
                .map(|path| {
                    if kept.contains(path) {
                        format!("dropped {path}")
                    } else {
                        format!("added {path}")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            return Err(held(
                "the relock changed which workflows the lockfile lists",
                &detail,
            ));
        }
        // The tool's own coverage check, offline, over the workflows the
        // lock lists: an unlisted workflow is not this run's and would
        // always fail it.
        let scope: Vec<String> = kept.into_iter().collect();
        if !scope.is_empty() {
            verify(self, &session, driver, root, &scope).await?;
        }
        moved(base, lock)
    }
}

/// The flags a fix run passes, with `--relock` for maintenance.
fn fix_flags(relock: bool) -> Vec<&'static str> {
    let mut flags = vec![
        "--no-interactive",
        "--no-onboard",
        "--no-narrow",
        "--ignore-local-actions",
    ];
    if relock {
        flags.push("--relock");
    }
    flags.push("--json=valid,findings");
    flags
}

/// A fix run's exit status as the run's verdict: 0 passes, 1 holds with
/// the finding that names the cause — `waiting` is what waits when GitHub
/// was unreachable — and anything else holds with the tool's stderr.
fn check(fix: &Outcome, waiting: &str) -> Result<(), Failure> {
    match fix.code {
        0 => Ok(()),
        1 => Err(
            match classify(&fix.stdout, waiting).map_err(Failure::Text)? {
                Some(reason) => Failure::Text(format!("actions.lock: {reason}")),
                None => held("blocking findings the fix run did not resolve", &fix.stdout),
            },
        ),
        _ => Err(held(&exit_two(&fix.stderr), "")),
    }
}

/// `--verify-local` over `scope`: every action ref those workflows hold
/// has its lock entry, or the run holds — `--no-onboard` exits 0 while
/// refusing to cover one.
async fn verify(
    runner: &Runner,
    session: &Session,
    driver: &Path,
    root: &Path,
    scope: &[String],
) -> Result<(), Failure> {
    let verified = session
        .invoke(
            runner,
            driver,
            root,
            Session::args(
                &["--verify-local", "--ignore-local-actions", "--json"],
                scope,
            ),
        )
        .await?;
    if verified.code == 0 {
        return Ok(());
    }
    let reason = classify(&verified.stdout, "the run")
        .ok()
        .flatten()
        .unwrap_or_else(|| exit_two(&verified.stderr));
    Err(held(NOT_ONBOARDED, &reason))
}

/// The refs a `--relock` advanced: every entry both locks hold whose
/// commit changed, under each workflow the regenerated lock reaches it
/// from through `workflows:` and the composites' `uses:`. The lock, not
/// the run's `ref-moved` findings, is the record: the tool reports a
/// finding for a moved direct ref but not for one inside a composite,
/// which it re-pins with the composite's whole closure.
fn moved(base: &str, lock: &str) -> Result<Vec<contract::MovedRef>, Failure> {
    let commits = |written: &Written| -> BTreeMap<String, String> {
        written
            .dependencies
            .iter()
            .filter_map(|(key, entry)| {
                let commit = commit_hex(entry.commit.as_deref()?);
                Some((canonical(key), commit.to_ascii_lowercase()))
            })
            .collect()
    };
    let (base, lock) = (written(base, "lockfile")?, written(lock, "lockfile")?);
    let (before, after) = (commits(&base), commits(&lock));
    let uses: BTreeMap<String, Vec<String>> = lock
        .dependencies
        .iter()
        .map(|(key, entry)| {
            (
                canonical(key),
                entry.uses.iter().map(|u| canonical(u)).collect(),
            )
        })
        .collect();
    let mut moved = BTreeSet::new();
    for (workflow, roots) in &lock.workflows {
        let mut reached = BTreeSet::new();
        let mut queue: Vec<String> = roots.iter().map(|root| canonical(root)).collect();
        while let Some(key) = queue.pop() {
            if reached.insert(key.clone()) {
                queue.extend(uses.get(&key).into_iter().flatten().cloned());
            }
        }
        for key in reached {
            if let (Some(from), Some(to)) = (before.get(&key), after.get(&key))
                && from != to
            {
                moved.insert(contract::MovedRef {
                    workflow: workflow.clone(),
                    dependency: key,
                    from: from.clone(),
                    to: to.clone(),
                });
            }
        }
    }
    Ok(moved.into_iter().collect())
}

/// The lock the tool wrote names every step-level `uses:` in the files
/// the run scoped, under the workflow it is written from, or for a `$/`
/// composite under every listed workflow that calls it — keys are
/// canonical as the tool writes them, owner and repository lowercased and
/// the action's sub-path dropped — and a written SHA is the entry's
/// `commit`. `jobs.<id>.uses` reusable workflows are not the lock's, and
/// an edited workflow outside the scope is not this run's. A dep
/// `verify-local` missed is held before the commit opens.
fn prove(
    root: &Path,
    work: &Work<'_>,
    scope: &[String],
    composites: &BTreeMap<String, BTreeSet<String>>,
    config: &Config,
) -> Result<(), Failure> {
    let lock = std::fs::read_to_string(root.join(contract::ACTIONS_LOCK))
        .map_err(|e| Failure::Text(e.to_string()))?;
    let written = written(&lock, "regenerated lockfile")?;
    let workflows: BTreeMap<&str, BTreeSet<String>> = written
        .workflows
        .iter()
        .map(|(path, keys)| (path.as_str(), keys.iter().map(|k| canonical(k)).collect()))
        .collect();
    let commits: BTreeMap<String, Option<&str>> = written
        .dependencies
        .iter()
        .map(|(key, entry)| (canonical(key), entry.commit.as_deref()))
        .collect();
    let edited: BTreeMap<String, String> = work
        .edits
        .iter()
        .filter(|(path, _)| scope.contains(path) || composites.contains_key(*path))
        .map(|(path, content)| (path.clone(), content.clone()))
        .collect();
    let extraction = patchula_manager::Scanner::new(config)
        .map_err(Failure::Text)?
        .extract(&patchula_manager::all_read(&edited))
        .map_err(Failure::Text)?;
    for dep in &extraction.dependencies {
        // The tool locks step-level actions only: `jobs.<id>.uses`
        // reusable workflows, images and local actions are not its.
        if dep.kind != patchula_words::DependencyKind::Action || dep.source != Source::GithubTags {
            continue;
        }
        // A workflow edit proves under its own path; a `$/` composite's
        // under every listed workflow that calls it.
        let under: Vec<&str> = match composites.get(&dep.file) {
            Some(callers) => callers.iter().map(String::as_str).collect(),
            None => vec![dep.file.as_str()],
        };
        let entry =
            contract::actions_lock_key(&dep.package, dep.digest.as_deref().unwrap_or(&dep.current));
        if let Some(workflow) = under.iter().find(|workflow| {
            !workflows
                .get(**workflow)
                .is_some_and(|entries| entries.contains(&entry))
        }) {
            return Err(held(
                NOT_ONBOARDED,
                &format!("{entry} is not under {workflow}"),
            ));
        }
        if let Some(digest) = &dep.digest {
            let commit = commits.get(&entry).copied().flatten();
            let recorded = commit.map(commit_hex);
            if !recorded.is_some_and(|recorded| recorded.eq_ignore_ascii_case(digest)) {
                return Err(held(
                    "the lockfile did not record the written pin",
                    &format!("{entry} records {commit:?}, not {digest}"),
                ));
            }
        }
    }
    Ok(())
}
