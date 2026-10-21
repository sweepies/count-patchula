//! `Gemfile.lock` through `bundle lock --update`, with Ruby from mise.
use super::*;

/// `.ruby-version`, then the Gemfile's `ruby` directive, then the default.
fn ruby_version(cwd: &Path, pinned: &str) -> String {
    let numeric = |v: &str| {
        v.split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    };
    if let Ok(text) = std::fs::read_to_string(cwd.join(".ruby-version"))
        && let Some(token) = text.split_whitespace().next()
    {
        let version = token.strip_prefix("ruby-").unwrap_or(token);
        if numeric(version) {
            return version.into();
        }
    }
    if let Ok(gemfile) = std::fs::read_to_string(cwd.join("Gemfile")) {
        for line in gemfile.lines() {
            let line = line.trim_start();
            if let Some(rest) = line
                .strip_prefix("ruby ")
                .or_else(|| line.strip_prefix("ruby("))
            {
                let version = rest.trim().trim_matches(['"', '\'', '(', ')', ' ']);
                if numeric(version) {
                    return version.into();
                }
            }
        }
    }
    pinned.into()
}

impl Runner {
    pub(super) async fn bundle_lock(
        &self,
        context: node::NodeContext<'_>,
        jobs: &[&Job],
    ) -> Result<(), String> {
        let node::NodeContext {
            driver,
            root,
            baseline,
            directory,
            versions,
            maintenance,
            ..
        } = context;
        let cwd = root.join(directory);
        let lock = node::join(directory, "Gemfile.lock");
        let gems: BTreeSet<_> = jobs.iter().filter_map(|j| j.package.clone()).collect();
        let mut args = vec!["lock".to_string(), "--update".into()];
        if !maintenance {
            args.extend(gems.iter().cloned());
        }
        let home = driver.join("ruby");
        let path = |p: &str| home.join(p).to_string_lossy().into_owned();
        self.tool(
            driver,
            Invocation {
                tools: vec![(
                    "ruby".into(),
                    ruby_version(&cwd, &node::version(versions, "ruby")?),
                )],
                bin: "bundle".into(),
                cwd: Some(cwd.clone()),
                env: vec![
                    // Repository .bundle/config never redirects the run.
                    ("BUNDLE_APP_CONFIG".into(), path("app-config")),
                    ("BUNDLE_USER_HOME".into(), path("bundle")),
                    ("GEM_HOME".into(), path("gems")),
                    ("BUNDLE_FROZEN".into(), "false".into()),
                    ("BUNDLE_DEPLOYMENT".into(), "false".into()),
                ],
                args,
            },
        )
        .await?;
        let before = baseline.get(&lock).map(|b| b.content.as_str());
        let after = std::fs::read_to_string(root.join(&lock)).ok();
        let manifest = node::join(directory, "Gemfile");
        let edited = baseline.get(&manifest).map(|b| b.content.clone())
            != std::fs::read_to_string(root.join(&manifest)).ok();
        if !maintenance && (edited || !gems.is_empty()) && before == after.as_deref() {
            return Err(format!(
                "Gemfile.lock did not change after the manifest edit in {}",
                if directory.is_empty() {
                    "the repository root"
                } else {
                    directory
                }
            ));
        }
        Ok(())
    }
}
