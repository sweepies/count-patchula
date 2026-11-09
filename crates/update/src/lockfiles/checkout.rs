use super::{
    Blob, Ecosystem, Error, Plan, Runner, ToolFailure, blob_sha, cargo_config, collect,
    directory_of_cargo_config, hash, materialized, safe_path, under,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::{collections::BTreeMap, path::Path};

impl Runner {
    async fn git(
        &self,
        driver: &Path,
        git_dir: &Path,
        args: Vec<String>,
    ) -> Result<Vec<u8>, Error> {
        let mut spec = self.command(driver);
        spec.args.extend([
            "exec".into(),
            "--".into(),
            "git".into(),
            "-c".into(),
            "core.hooksPath=/dev/null".into(),
            "-c".into(),
            "credential.helper=".into(),
            "-c".into(),
            "core.autocrlf=false".into(),
            format!("--git-dir={}", git_dir.display()),
        ]);
        spec.args.extend(args);
        spec.env.insert(
            "GIT_INDEX_FILE".into(),
            driver.join("index").into_os_string(),
        );
        spec.env.insert("GIT_CONFIG_COUNT".into(), "1".into());
        spec.env.insert(
            "GIT_CONFIG_KEY_0".into(),
            "http.https://github.com/.extraheader".into(),
        );
        let token = self.github_token().await?;
        let encoded = STANDARD.encode(format!("x-access-token:{token}"));
        let header = format!("Authorization: Basic {encoded}");
        spec.env
            .insert("GIT_CONFIG_VALUE_0".into(), header.clone().into());
        spec.secrets = vec![header, encoded, token];
        spec.complete_output = true;
        spec.output_limit = 16 * 1024 * 1024;
        self.executor.run(spec).await.map_err(|failure| {
            Error::Tool(ToolFailure {
                context: "git".into(),
                failure,
            })
        })
    }
    pub(super) async fn checkout(
        &self,
        plan: &Plan,
        root: &Path,
        driver: &Path,
    ) -> Result<(BTreeMap<String, Blob>, BTreeMap<String, String>), Error> {
        let clones = self.cache.join("clones");
        std::fs::create_dir_all(&clones).map_err(|e| e.to_string())?;
        let name = hash(plan.action().repository.as_bytes());
        let clone = clones.join(&name);
        let lock = std::fs::File::options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(clones.join(format!("{name}.lock")))
            .map_err(|e| e.to_string())?;
        // Commands in other checkouts share the clone; wait for theirs to finish.
        let _lock = tokio::task::spawn_blocking(move || lock.lock().map(|()| lock))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("cannot lock the repository clone: {e}"))?;
        if !clone.join("HEAD").is_file() {
            self.git(
                driver,
                &clone,
                vec![
                    "init".into(),
                    "--bare".into(),
                    clone.to_string_lossy().into_owned(),
                ],
            )
            .await?;
        }
        let repo = &plan.action().repository;
        let (owner, name) = repo.split_once('/').ok_or("invalid repository")?;
        if [owner, name].iter().any(|s| {
            s.is_empty()
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        }) {
            return Err("invalid repository".into());
        }
        let url = format!("https://github.com/{repo}.git");
        for (key, value) in [
            ("core.repositoryformatversion", "1"),
            ("remote.origin.url", url.as_str()),
            ("remote.origin.promisor", "true"),
            ("remote.origin.partialclonefilter", "blob:none"),
            ("extensions.partialClone", "origin"),
        ] {
            self.git(
                driver,
                &clone,
                vec!["config".into(), key.into(), value.into()],
            )
            .await?;
        }
        let base = &plan.action().base_commit;
        if base.len() != 40 || !base.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("invalid checkout base OID".into());
        }
        // Inspect the ref without dereferencing an object: cat-file can trigger
        // an implicit promisor fetch of an unknown commit and its history.
        let cached = self
            .git(
                driver,
                &clone,
                vec![
                    "rev-parse".into(),
                    "--verify".into(),
                    "refs/patchula/base".into(),
                ],
            )
            .await;
        if !cached
            .as_ref()
            .is_ok_and(|bytes| std::str::from_utf8(bytes).is_ok_and(|oid| oid.trim() == base))
        {
            self.git(
                driver,
                &clone,
                vec![
                    "fetch".into(),
                    "--filter=blob:none".into(),
                    "--depth=1".into(),
                    "--no-tags".into(),
                    "origin".into(),
                    format!("+{base}:refs/patchula/base"),
                ],
            )
            .await?;
        }
        let tree = self
            .git(
                driver,
                &clone,
                vec![
                    "ls-tree".into(),
                    "-rz".into(),
                    "--full-tree".into(),
                    base.clone(),
                ],
            )
            .await?;
        let mut entries = BTreeMap::new();
        let mut cargo_configs = BTreeMap::new();
        for entry in tree.split(|b| *b == 0).filter(|e| !e.is_empty()) {
            let entry = std::str::from_utf8(entry).map_err(|_| "non UTF-8 repository path")?;
            let (metadata, path) = entry.split_once('\t').ok_or("invalid Git tree entry")?;
            let cargo = self.reads_cargo_config(plan, path);
            if !materialized(path) && !cargo && !plan.contents.contains_key(path) {
                continue;
            }
            if !safe_path(path) {
                return Err("unsafe checkout path".into());
            }
            let mut parts = metadata.split(' ');
            let mode = parts.next().ok_or("missing tree mode")?;
            let kind = parts.next().ok_or("missing tree type")?;
            let oid = parts.next().ok_or("missing tree OID")?;
            let regular = matches!(mode, "100644" | "100755") && kind == "blob";
            if cargo {
                // A linked Cargo configuration defines no registries.
                if regular {
                    cargo_configs.insert(path.to_owned(), oid.to_owned());
                }
                continue;
            }
            if !regular {
                return Err(format!("unsupported checkout mode for {path}").into());
            }
            entries.insert(path.to_owned(), (oid.to_owned(), mode.to_owned()));
        }
        let paths: Vec<_> = entries.keys().cloned().collect();
        for chunk in paths.chunks(64) {
            let mut args = vec![
                format!("--work-tree={}", root.display()),
                "checkout".into(),
                base.clone(),
                "--".into(),
            ];
            args.extend_from_slice(chunk);
            self.git(driver, &clone, args).await?;
        }
        let files = collect(root)?;
        let mut baseline = BTreeMap::new();
        for (path, content) in files {
            let (sha, mode) = entries.get(&path).ok_or("unexpected checkout file")?;
            if blob_sha(&content) != *sha {
                return Err(format!("checkout blob SHA mismatch: {path}").into());
            }
            baseline.insert(
                path,
                Blob {
                    sha: sha.clone(),
                    mode: mode.clone(),
                    content,
                },
            );
        }
        // Manifest edits were extracted through the API; the clone must agree.
        for file in &plan.action().files {
            if baseline.get(&file.path).map(|b| b.sha.as_str()) != Some(&file.blob_sha) {
                return Err(
                    format!("checkout disagrees with extracted manifest {}", file.path).into(),
                );
            }
        }
        let cargo_configs = self
            .cargo_configs(&clone, base, driver, cargo_configs)
            .await?;
        Ok((baseline, cargo_configs))
    }

    /// Whether `path` is a Cargo configuration at or above a project the
    /// plan relocks, read for its registries when a tools entry may cover
    /// them.
    fn reads_cargo_config(&self, plan: &Plan, path: &str) -> bool {
        !self.registries.is_empty()
            && cargo_config(path)
            && plan.lock_jobs.iter().any(|job| {
                let owner = directory_of_cargo_config(path);
                job.ecosystem == Ecosystem::Cargo
                    && (job.directory == owner || under(&job.directory, owner))
            })
    }

    /// The text of the Cargo configurations `oids` names, checked out apart
    /// from the tree tools run in, so no tool reads them and no other file
    /// depends on them. One that is not UTF-8, is over 1 MiB or does not match
    /// its blob defines no registries.
    async fn cargo_configs(
        &self,
        clone: &Path,
        base: &str,
        driver: &Path,
        oids: BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, String>, Error> {
        let mut out = BTreeMap::new();
        if oids.is_empty() {
            return Ok(out);
        }
        let work = driver.join("cargo-config");
        std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        let paths: Vec<_> = oids.keys().cloned().collect();
        for chunk in paths.chunks(64) {
            let mut args = vec![
                format!("--work-tree={}", work.display()),
                "checkout".into(),
                base.to_owned(),
                "--".into(),
            ];
            args.extend_from_slice(chunk);
            self.git(driver, clone, args).await?;
        }
        for (path, oid) in oids {
            let Ok(bytes) = std::fs::read(work.join(&path)) else {
                continue;
            };
            if bytes.len() > 1 << 20 {
                continue;
            }
            if let Ok(text) = String::from_utf8(bytes)
                && blob_sha(&text) == oid
            {
                out.insert(path, text);
            }
        }
        Ok(out)
    }
}
