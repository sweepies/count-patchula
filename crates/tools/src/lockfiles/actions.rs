//! `.github/workflows/actions.lock` through `gh actions-lock`, run only
//! over the edited workflows the lock already lists — Renovate runs the
//! whole directory, which would let someone else's broken pin hold the
//! Count's update. `--no-onboard` and `--no-narrow` are never dropped:
//! the Count adds nothing to the lockfile and rewrites no `uses:` ref it
//! did not edit.
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
#[derive(serde::Deserialize)]
struct Finding {
    category: String,
    severity: String,
    detail: String,
    remediation: Option<String>,
    retryable: bool,
    workflow: Option<String>,
}

/// The entries the regenerated lockfile records: each workflow's
/// `owner/repo@ref` list, and every entry's resolved commit under
/// `dependencies:`.
#[derive(serde::Deserialize)]
struct Written {
    workflows: BTreeMap<String, Vec<String>>,
    dependencies: BTreeMap<String, WrittenEntry>,
}
#[derive(serde::Deserialize)]
struct WrittenEntry {
    commit: Option<String>,
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
fn classify(stdout: &str) -> Result<Option<String>, String> {
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
            "GitHub was unreachable or rate-limited; the update waits for the next run\n{}",
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
        "onboarding-required" => {
            "an action the lockfile does not list yet; the Count never onboards".into()
        }
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

impl Runner {
    /// Regenerate the listed workflows' `actions.lock` entries around the
    /// update's edits, or hold the update with the tool's reason.
    pub(super) async fn actions_lock(
        &self,
        context: context::Context<'_>,
        work: &Work<'_>,
    ) -> Result<(), Failure> {
        let context::Context {
            driver,
            root,
            baseline,
            config,
            ..
        } = context;
        let Some(base) = baseline.get(contract::ACTIONS_LOCK) else {
            return Err("actions.lock job without the lockfile".into());
        };
        let Some(listed) = contract::actions_lock_workflows(&base.content) else {
            return Err(corrupt());
        };
        // The edited workflows the lock lists, and the listed workflows
        // that call an edited in-repository action file through `$/`:
        // the tool locks a `$/` composite's remote `uses:` under each
        // caller, and never a `./` one. Callers are resolved in the
        // edited tree; what cannot be resolved holds the update.
        let keys: BTreeSet<String> = listed.keys().cloned().collect();
        let read = |path: &str| -> Result<Option<String>, String> {
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
        let defaults = tool_defaults()?;
        let version = defaults["tools"][ACTIONS_LOCK_SPEC]
            .as_str()
            .ok_or("invalid tool defaults")?;
        // The scratch directory the shell writes the run's streams and
        // status to, and an empty PATH: `gh` and `git` stay unreachable
        // whatever the sandbox has, and `GH_HOST` keeps the tool on
        // github.com anyway.
        let scratch = driver.join("actions-lock");
        let empty_path = scratch.join("path");
        std::fs::create_dir_all(&empty_path).map_err(|e| e.to_string())?;
        let token = self.actions_lock_token(work.repository).await?;
        tracing::info!(kind = ?token.kind, repository = %work.repository, "actions.lock token granted");
        let mut env = vec![
            ("GH_HOST".into(), "github.com".into()),
            ("GH_REPO".into(), work.repository.into()),
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
        let run = |args: Vec<String>, secrets: Vec<String>| Invocation {
            descriptor: &DESCRIPTOR,
            tools: vec![(ACTIONS_LOCK_SPEC.into(), version.into())],
            bin: caches::Bin::ActionsLock,
            cwd: Some(root.to_path_buf()),
            env: env.clone(),
            credentials: Vec::new(),
            secrets,
            public: false,
            args,
        };
        let mut fix = vec![
            "-c".into(),
            SHELL.into(),
            "gh-actions-lock".into(),
            "--no-interactive".into(),
            "--no-onboard".into(),
            "--no-narrow".into(),
            "--ignore-local-actions".into(),
            "--json=valid,findings".into(),
        ];
        fix.extend(scope.iter().cloned());
        self.tool(driver, run(fix, secrets.clone())).await?;
        let code = std::fs::read_to_string(scratch.join("status"))
            .map_err(|e| e.to_string())?
            .trim()
            .parse::<u8>()
            .map_err(|e| e.to_string())?;
        let stdout = redact(
            &secrets,
            &std::fs::read_to_string(scratch.join("stdout")).map_err(|e| e.to_string())?,
        );
        let stderr = redact(
            &secrets,
            &std::fs::read_to_string(scratch.join("stderr")).map_err(|e| e.to_string())?,
        );
        match code {
            0 => {}
            1 => {
                return Err(match classify(&stdout).map_err(Failure::Text)? {
                    Some(reason) => Failure::Text(format!("actions.lock: {reason}")),
                    None => held("blocking findings the fix run did not resolve", &stdout),
                });
            }
            _ => return Err(held(&exit_two(&stderr), "")),
        }
        // `valid` on stdout is not proof: `--no-onboard` exits 0 while
        // refusing to cover an action. The tool's own coverage check runs
        // offline on the same paths, then the lock is read back against
        // the edit.
        let mut verify = vec![
            "-c".into(),
            SHELL.into(),
            "gh-actions-lock".into(),
            "--verify-local".into(),
            "--ignore-local-actions".into(),
            "--json".into(),
        ];
        verify.extend(scope.iter().cloned());
        self.tool(driver, run(verify, secrets.clone())).await?;
        let verified = std::fs::read_to_string(scratch.join("status"))
            .map_err(|e| e.to_string())?
            .trim()
            .parse::<u8>()
            .map_err(|e| e.to_string())?;
        if verified != 0 {
            let stdout = redact(
                &secrets,
                &std::fs::read_to_string(scratch.join("stdout")).map_err(|e| e.to_string())?,
            );
            let stderr = redact(
                &secrets,
                &std::fs::read_to_string(scratch.join("stderr")).map_err(|e| e.to_string())?,
            );
            let reason = classify(&stdout)
                .ok()
                .flatten()
                .unwrap_or_else(|| exit_two(&stderr));
            return Err(held(
                "an action the lockfile does not list yet; the Count never onboards",
                &reason,
            ));
        }
        prove(root, work, &scope, &composites, config)
    }
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
    let written: Written = serde_yaml_ng::from_str(&lock).map_err(|e| {
        Failure::Text(format!(
            "actions.lock: unparseable regenerated lockfile: {e}"
        ))
    })?;
    let canonical = |key: &str| contract::actions_lock_pin(key).unwrap_or_else(|| key.to_owned());
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
                "an action the lockfile does not list yet; the Count never onboards",
                &format!("{entry} is not under {workflow}"),
            ));
        }
        if let Some(digest) = &dep.digest {
            let commit = commits.get(&entry).copied().flatten();
            let recorded = commit
                .and_then(|commit| commit.strip_prefix("sha1-"))
                .or(commit);
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
