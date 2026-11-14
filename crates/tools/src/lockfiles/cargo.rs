//! `Cargo.lock` through `cargo update`, outside the repository tree so no
//! repository configuration reaches it; credentialed registries arrive by
//! name, from the environment.
use super::*;

pub(super) static DESCRIPTOR: Tool = Tool {
    steers: &[Manager::Cargo],
    tools: &["rust"],
    versions: true,
    install: Install::None,
    proof: Proof::DeclaredAndResolved,
    run: Run::Rustup("rust"),
    output: Output::Default,
};

/// What a Cargo run is given.
#[derive(Clone, Copy)]
pub(super) struct CargoAccess<'a> {
    /// The registry environment.
    pub env: &'a [(String, String)],
    /// The credential scopes the run holds.
    pub credentials: &'a [String],
    /// Every secret value a failure must not repeat.
    pub secrets: &'a [String],
    /// Whether the run may share the `public` cache partition.
    pub public: bool,
}
/// What one directory's jobs share.
#[derive(Clone, Copy)]
pub(super) struct CargoDirectory<'a> {
    /// The toolchain pin.
    pub rust: &'a str,
    /// Whether the run is lockfile maintenance.
    pub maintenance: bool,
    /// The cooldown the repository committed for the directory.
    pub cooldown: Option<&'a patchula_manager::Cooldown>,
}

/// Whether Cargo treats `target` as incompatible with `current`: a different
/// major version, or a different minor (or patch) one below 1.0. Cargo then
/// keeps both in the lock.
fn crosses_compatibility(current: &str, target: &str) -> bool {
    use patchula_versioning::{Ecosystem as Scheme, Versioning};
    use patchula_words::Update;
    match (Scheme::Cargo.parse(current), Scheme::Cargo.parse(target)) {
        (Ok(current), Ok(target)) => Scheme::Cargo.classify(&current, &target) == Update::Major,
        _ => false,
    }
}
/// The version and source of each crates.io entry for `package` in a
/// Cargo.lock. A git or other registry package of the same name is another
/// package.
pub(super) fn crates_io_entries(lock: &str, package: &str) -> Vec<(String, String)> {
    let Ok(lock) = toml_edit::Document::parse(lock) else {
        return Vec::new();
    };
    lock.get("package")
        .and_then(toml_edit::Item::as_array_of_tables)
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("name").and_then(toml_edit::Item::as_str) == Some(package))
        .filter_map(|entry| {
            let source = entry.get("source").and_then(toml_edit::Item::as_str)?;
            if !patchula_manager::crates_io(source) {
                return None;
            }
            Some((
                entry.get("version")?.as_str()?.to_owned(),
                source.to_owned(),
            ))
        })
        .collect()
}

/// Whether `version` names a Cargo too old to read publish-age values:
/// Rust before 1.100 ignores the environment keys entirely. A pin that is
/// not a plain version, such as `nightly`, counts as new.
fn predates_publish_age(version: &str) -> bool {
    let mut parts = version.split('.');
    let (Some(major), Some(minor)) = (
        parts.next().and_then(|p| p.parse::<u64>().ok()),
        parts.next().and_then(|p| p.parse::<u64>().ok()),
    ) else {
        return false;
    };
    (major, minor) < (1, 100)
}

impl Runner {
    /// Regenerate every Cargo.lock committed in one directory.
    pub(super) async fn cargo_locks(
        &self,
        context: context::Context<'_>,
        jobs: &[&Job],
        cargo_configs: &BTreeMap<String, String>,
        cooldowns: &BTreeMap<String, patchula_manager::CargoCooldown>,
        warnings: &mut Vec<String>,
    ) -> Result<(), Failure> {
        let context::Context {
            driver,
            root,
            directory,
            versions,
            maintenance,
            public,
            ..
        } = context;
        let cwd = root.join(directory);
        // A cooldown the repository commits but the Count cannot read fails
        // the directory's jobs, maintenance included, as its lookups
        // already fail: running with `0` would waive what it commits.
        let committed = match cooldowns.get(directory) {
            Some(patchula_manager::CargoCooldown::Unread(reason)) => {
                return Err(reason.clone().into());
            }
            Some(patchula_manager::CargoCooldown::Set(cooldown)) => Some(cooldown),
            None => None,
        };
        // Cargo runs outside the repository and reads none of its
        // configuration. It gets each credentialed registry the
        // `.cargo/config.toml` files at or above the project define, by
        // name, from the environment.
        let registries: Vec<_> = cargo_configs
            .iter()
            .filter(|(path, _)| within(directory, directory_of_cargo_config(path)))
            .map(|(_, text)| credentials::cargo_registries(text))
            .collect();
        let (env, entries) = self.cargo_env(&registries);
        let credentials = credentials::scopes(entries.iter().copied());
        let secrets = credentials::secrets(entries.iter().copied());
        let rust = context::version(versions, "rust")?;
        // A Cargo that predates publish-age reads ignores the keys: lookup's
        // floor still holds the targets it picks, but the crates the run
        // newly locks move unheld. Warn once for the directory.
        if jobs
            .iter()
            .any(|job| committed.map_or(0, |c| c.hours).max(job.wait_hours) > 0)
            && predates_publish_age(&rust)
        {
            warnings.push(format!(
                "{}: rust {rust} predates Cargo 1.100's publish-age support; \
                 the cooldown binds direct updates but transitive crates do not wait",
                context::join(directory, "Cargo.toml"),
            ));
        }
        for job in jobs {
            self.cargo_update(
                driver,
                &cwd,
                job,
                CargoDirectory {
                    rust: &rust,
                    maintenance,
                    cooldown: committed,
                },
                CargoAccess {
                    env: &env,
                    credentials: &credentials,
                    secrets: &secrets,
                    public,
                },
            )
            .await?;
        }
        Ok(())
    }
    /// Moves Cargo.lock for one job, against the lock as it stands: a job whose
    /// target is already locked has nothing to do. A package moves with
    /// `--precise` to the exact target. A target across a semver-incompatible boundary is a second
    /// lock entry beside the old one, which other crates may still require, so
    /// `--precise` on the old entry cannot reach it; `--workspace` locks what
    /// the edited manifests need and leaves every other package where it is.
    pub(super) async fn cargo_update(
        &self,
        driver: &Path,
        cwd: &Path,
        job: &Job,
        directory: CargoDirectory<'_>,
        access: CargoAccess<'_>,
    ) -> Result<(), Failure> {
        let CargoDirectory {
            rust,
            maintenance,
            cooldown,
        } = directory;
        let CargoAccess {
            env,
            credentials,
            secrets,
            public,
        } = access;
        // The publish age Cargo holds every crate this job newly locks to:
        // the longer of the directory's committed cooldown — verbatim, in
        // the grammar the repository already gave Cargo — and the wait the
        // job's update answered to. `0` holds nothing, and keeps a cooldown
        // a parent directory or the operator's own Cargo configuration
        // would impose from leaking into the run. `allow` is reserved for
        // the job whose security fix sits inside the window.
        let mut env = env.to_vec();
        env.push((
            "CARGO_REGISTRY_GLOBAL_MIN_PUBLISH_AGE".into(),
            match cooldown {
                Some(cooldown) if cooldown.hours >= job.wait_hours => cooldown.value.clone(),
                _ if job.wait_hours == 0 => "0".into(),
                _ => format!("{} hours", job.wait_hours),
            },
        ));
        env.push((
            "CARGO_RESOLVER_INCOMPATIBLE_PUBLISH_AGE".into(),
            if job.publish_age_allow {
                "allow".into()
            } else {
                "deny".into()
            },
        ));
        let update = |args: Vec<String>| {
            let mut all = vec![
                "update".into(),
                "--manifest-path".into(),
                cwd.join("Cargo.toml").to_string_lossy().into_owned(),
            ];
            all.extend(args);
            self.held_tool(
                driver,
                Invocation {
                    descriptor: &DESCRIPTOR,
                    tools: vec![("rust".into(), rust.into())],
                    bin: caches::Bin::Cargo,
                    cwd: None,
                    env: env.clone(),
                    credentials: credentials.to_vec(),
                    secrets: secrets.to_vec(),
                    public,
                    args: all,
                },
            )
        };
        let Some(package) = job.package() else {
            let args = if maintenance {
                vec![]
            } else {
                vec!["--workspace".into()]
            };
            update(args).await?;
            return Ok(());
        };
        let current = job.current().unwrap_or("");
        let target = job.target().ok_or("missing Cargo target")?;
        let crossing = crosses_compatibility(current, target);
        if crossing {
            update(vec!["--workspace".into()]).await?;
        }
        // Address what the lock holds now: an earlier job or the workspace
        // update may have moved the entry this job was planned against.
        let lock = std::fs::read_to_string(cwd.join("Cargo.lock")).map_err(|e| e.to_string())?;
        let locked = crates_io_entries(&lock, package);
        if locked
            .iter()
            .any(|(version, _)| same_release(version, target))
        {
            return Ok(());
        }
        // Cargo keeps one entry per compatibility range, so this is the one
        // crates.io entry that can still move to the target. A git or other
        // registry copy of the name is another package, and the source in the
        // spec keeps an equal name and version from being ambiguous.
        let mut movable: Vec<String> = locked
            .iter()
            .filter(|(version, _)| !crosses_compatibility(version, target))
            .map(|(version, source)| format!("{source}#{package}@{version}"))
            .collect();
        if movable.is_empty() && !crossing {
            // Nothing matches; let cargo name the package it cannot find.
            movable.push(format!("{package}@{current}"));
        }
        for spec in movable {
            update(vec!["-p".into(), spec, "--precise".into(), target.into()]).await?;
        }
        Ok(())
    }
}
