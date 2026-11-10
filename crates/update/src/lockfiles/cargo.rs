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
    refresh: Refresh::Locks(&[Manager::Cargo]),
    output: Output::Default,
};

/// What a Cargo run is given: its registry environment, the credential
/// scopes it holds, every secret value a failure must not repeat, and
/// whether it may share the `public` cache partition.
pub(super) type CargoAccess<'a> = (&'a [(String, String)], &'a [String], &'a [String], bool);

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

impl Runner {
    /// Regenerate every Cargo.lock committed in one directory.
    pub(super) async fn cargo_locks(
        &self,
        context: context::Context<'_>,
        jobs: &[&Job],
        cargo_configs: &BTreeMap<String, String>,
    ) -> Result<(), Error> {
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
        for job in jobs {
            self.cargo_update(
                driver,
                &cwd,
                &rust,
                job,
                maintenance,
                (&env, &credentials, &secrets, public),
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
        rust: &str,
        job: &Job,
        maintenance: bool,
        access: CargoAccess<'_>,
    ) -> Result<(), Error> {
        let (env, credentials, secrets, public) = access;
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
                    env: env.to_vec(),
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
