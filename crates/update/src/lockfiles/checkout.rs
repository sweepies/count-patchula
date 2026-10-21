use super::{Blob, Plan, Runner, blob_sha, collect, hash, materialized, safe_path};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::{collections::BTreeMap, path::Path};

impl Runner {
    async fn git(
        &self,
        driver: &Path,
        git_dir: &Path,
        args: Vec<String>,
    ) -> Result<Vec<u8>, String> {
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
        let token = if let Some(credentials) = &self.credentials {
            credentials
                .token()
                .await
                .map_err(|_| "GitHub authentication failed".to_owned())?
        } else {
            self.token.clone()
        };
        spec.env.insert(
            "GIT_CONFIG_VALUE_0".into(),
            format!(
                "Authorization: Basic {}",
                STANDARD.encode(format!("x-access-token:{token}"))
            )
            .into(),
        );
        spec.complete_output = true;
        spec.output_limit = 16 * 1024 * 1024;
        self.executor
            .run(spec)
            .await
            .map_err(|e| format!("git: {e}"))
    }
    pub(super) async fn checkout(
        &self,
        plan: &Plan,
        root: &Path,
        driver: &Path,
    ) -> Result<BTreeMap<String, Blob>, String> {
        let clones = self.cache.join("clones");
        std::fs::create_dir_all(&clones).map_err(|e| e.to_string())?;
        let name = hash(plan.action.repository.as_bytes());
        let clone = clones.join(&name);
        let lock = std::fs::File::options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(clones.join(format!("{name}.lock")))
            .map_err(|e| e.to_string())?;
        lock.try_lock()
            .map_err(|_| "repository clone is in use; rerun".to_owned())?;
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
        let repo = &plan.action.repository;
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
        let base = &plan.action.base_commit;
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
        for entry in tree.split(|b| *b == 0).filter(|e| !e.is_empty()) {
            let entry = std::str::from_utf8(entry).map_err(|_| "non UTF-8 repository path")?;
            let (metadata, path) = entry.split_once('\t').ok_or("invalid Git tree entry")?;
            if !materialized(path) && !plan.contents.contains_key(path) {
                continue;
            }
            if !safe_path(path) {
                return Err("unsafe checkout path".into());
            }
            let mut parts = metadata.split(' ');
            let mode = parts.next().ok_or("missing tree mode")?;
            let kind = parts.next().ok_or("missing tree type")?;
            let oid = parts.next().ok_or("missing tree OID")?;
            if !matches!(mode, "100644" | "100755") || kind != "blob" {
                return Err(format!("unsupported checkout mode for {path}"));
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
                return Err(format!("checkout blob SHA mismatch: {path}"));
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
        for file in &plan.action.files {
            if baseline.get(&file.path).map(|b| b.sha.as_str()) != Some(&file.blob_sha) {
                return Err(format!(
                    "checkout disagrees with extracted manifest {}",
                    file.path
                ));
            }
        }
        Ok(baseline)
    }
}
