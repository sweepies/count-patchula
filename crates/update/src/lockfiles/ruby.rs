//! `Gemfile.lock` through `bundle lock --update`, with Ruby from mise.
use super::{pin::Pinned, *};

pub(super) static DESCRIPTOR: Tool = Tool {
    steers: &[Manager::Bundler],
    tools: &["ruby"],
    versions: true,
    install: Install::Plain,
    proof: Proof::DeclaredAndResolved,
    run: Run::Direct,
    refresh: Refresh::Locks(&[Manager::Bundler]),
    output: Output::Default,
};

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
        context: context::Context<'_>,
        jobs: &[&Job],
    ) -> Result<(), Error> {
        let context::Context {
            driver,
            root,
            baseline,
            directory,
            config,
            versions,
            maintenance,
            public,
        } = context;
        let cwd = root.join(directory);
        let lock = context::join(directory, "Gemfile.lock");
        let gems: BTreeSet<_> = jobs
            .iter()
            .filter_map(|j| j.package().map(str::to_owned))
            .collect();
        let mut args = vec!["lock".to_string(), "--update".into()];
        if !maintenance {
            args.extend(gems.iter().cloned());
        }
        let home = driver.join("ruby");
        let path = |p: &str| home.join(p).to_string_lossy().into_owned();
        let ruby = ruby_version(&cwd, &context::version(versions, "ruby")?);
        let (credentials, entries) = self.bundler_env();
        let scopes = credentials::scopes(entries.iter().copied());
        let secrets = credentials::secrets(entries.iter().copied());
        let invocation = |args| Invocation {
            descriptor: &DESCRIPTOR,
            tools: vec![("ruby".into(), ruby.clone())],
            bin: caches::Bin::Bundle,
            cwd: Some(cwd.clone()),
            env: vec![
                // Repository .bundle/config never redirects the run.
                ("BUNDLE_APP_CONFIG".into(), path("app-config")),
                ("BUNDLE_USER_HOME".into(), path("bundle")),
                ("GEM_HOME".into(), path("gems")),
                ("BUNDLE_FROZEN".into(), "false".into()),
                ("BUNDLE_DEPLOYMENT".into(), "false".into()),
            ]
            .into_iter()
            .chain(credentials.iter().cloned())
            .collect(),
            credentials: scopes.clone(),
            secrets: secrets.clone(),
            public,
            args,
        };
        // Bundler moves a gem as far as its requirement allows and cannot be
        // told a version, so a package whose declaration stays as it is has
        // the requirement pinned for one resolution and restored for the next.
        let mut pinned = Pinned::apply(root, directory, config, Manager::Bundler, jobs)?;
        self.held_tool(driver, invocation(args)).await?;
        if !pinned.is_empty() {
            pinned.restore()?;
            self.held_tool(driver, invocation(vec!["lock".to_string()]))
                .await?;
        }
        let before = baseline.get(&lock).map(|b| b.content.as_str());
        let after = std::fs::read_to_string(root.join(&lock)).ok();
        let manifest = context::join(directory, "Gemfile");
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
            )
            .into());
        }
        Ok(())
    }
}
