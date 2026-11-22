//! `Gemfile.lock` through `bundle lock --update`, with Ruby from mise.
//!
//! A Gemfile is Ruby, and Bundler cannot resolve without evaluating it, so
//! Bundler never sees the repository's: it evaluates one the Count renders
//! from its own reading (`patchula_manager::render_gemfile`), in a scratch
//! directory outside the checkout with a copy of `Gemfile.lock` and nothing
//! else, and only `Gemfile.lock` comes back. A Gemfile that needs a form the
//! Count does not render, or a lock that records a gem from git, a path or
//! a plugin source, is refused.
use super::*;

pub(super) static DESCRIPTOR: Tool = Tool {
    steers: &[Manager::Bundler],
    tools: &["ruby"],
    install: Install::Plain,
    proof: Proof::DeclaredAndResolved,
    run: Run::Direct,
    output: Output::Default,
};

/// The `Gemfile.lock` sections whose gems Bundler takes from git, a path or
/// a plugin, evaluating their gemspecs or the plugin's code to resolve them.
const REFUSED_SECTIONS: [&str; 3] = ["GIT", "PATH", "PLUGIN SOURCE"];

/// Why a lock cannot be regenerated without evaluating Ruby the repository
/// chose, or `None`.
pub(super) fn refused_lock(lock: &str) -> Option<String> {
    lock.lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .find(|line| REFUSED_SECTIONS.contains(line))
        .map(|section| {
            format!("it records gems from a {section} section, which Bundler resolves by evaluating Ruby")
        })
}

/// The reason a Bundler update is refused, for the dashboard and report.
fn refusal(directory: &str, why: &str) -> Failure {
    Failure::Refused(format!(
        "Gemfile.lock in {} is not regenerated: {why}, and the Count never runs repository or package code",
        context::describe_dir(directory)
    ))
}

/// `.ruby-version`, then the Gemfile's `ruby` directive, then the default.
fn ruby_version(cwd: &Path, pinned: &str) -> String {
    if let Ok(text) = std::fs::read_to_string(cwd.join(".ruby-version"))
        && let Some(token) = text.split_whitespace().next()
    {
        let version = token.strip_prefix("ruby-").unwrap_or(token);
        if context::numeric(version, None) {
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
                if context::numeric(version, None) {
                    return version.into();
                }
            }
        }
    }
    pinned.into()
}

/// Write the Gemfile rendered from the checkout's into `scratch`, with the
/// files it reads beside it.
fn render(cwd: &Path, scratch: &Path, directory: &str) -> Result<(), Failure> {
    let gemfile = std::fs::read_to_string(cwd.join("Gemfile")).map_err(|e| e.to_string())?;
    let rendered = patchula_manager::render_gemfile(&gemfile)
        .map_err(|why| refusal(directory, &format!("Gemfile {why}")))?;
    std::fs::write(scratch.join("Gemfile"), rendered.text).map_err(|e| e.to_string())?;
    for name in rendered.reads {
        if let Ok(text) = std::fs::read_to_string(cwd.join(&name)) {
            std::fs::write(scratch.join(&name), text).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

impl Runner {
    pub(super) async fn bundle_lock(
        &self,
        context: context::Context<'_>,
        jobs: &[&Job],
    ) -> Result<(), Failure> {
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
        let committed = std::fs::read_to_string(cwd.join("Gemfile.lock")).ok();
        if let Some(why) = committed.as_deref().and_then(refused_lock) {
            return Err(refusal(directory, &why));
        }
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
        // Outside the checkout, so Bundler finds no file of the
        // repository's beyond the two it is given.
        let scratch = tempfile::Builder::new()
            .prefix("bundler-")
            .tempdir_in(driver)
            .map_err(|e| e.to_string())?;
        let ruby = ruby_version(&cwd, &context::version(versions, "ruby")?);
        let (credentials, entries) = self.bundler_env();
        let held = credentials::Held::of(entries.iter().copied());
        let invocation = |args| Invocation {
            descriptor: &DESCRIPTOR,
            tools: vec![("ruby".into(), ruby.clone())],
            bin: caches::Bin::Bundle,
            cwd: Some(scratch.path().to_owned()),
            env: vec![
                (
                    "BUNDLE_GEMFILE".into(),
                    path_of(&scratch.path().join("Gemfile")),
                ),
                // No repository .bundle/config redirects the run.
                ("BUNDLE_APP_CONFIG".into(), path("app-config")),
                ("BUNDLE_USER_HOME".into(), path("bundle")),
                ("GEM_HOME".into(), path("gems")),
                ("BUNDLE_FROZEN".into(), "false".into()),
                ("BUNDLE_DEPLOYMENT".into(), "false".into()),
                // The Ruby's own Bundler: never one the lock names, which
                // Bundler would fetch from RubyGems and run.
                ("BUNDLE_VERSION".into(), "system".into()),
                // A protocol list naming no transport: git clones nothing.
                ("GIT_ALLOW_PROTOCOL".into(), "none".into()),
            ]
            .into_iter()
            .chain(credentials.iter().cloned())
            .collect(),
            held: held.clone(),
            refused: None,
            public,
            args,
        };
        if let Some(text) = &committed {
            std::fs::write(scratch.path().join("Gemfile.lock"), text).map_err(|e| e.to_string())?;
        }
        // Bundler moves a gem as far as its requirement allows and cannot be
        // told a version, so a package whose declaration stays as it is has
        // the requirement pinned for one resolution and restored for the next.
        let mut pinned = pin::apply(root, directory, config, Manager::Bundler, jobs)?;
        render(&cwd, scratch.path(), directory)?;
        self.tool(driver, invocation(args)).await?;
        if !pinned.is_empty() {
            pinned.restore()?;
            render(&cwd, scratch.path(), directory)?;
            self.tool(driver, invocation(vec!["lock".to_string()]))
                .await?;
        }
        let after = std::fs::read_to_string(scratch.path().join("Gemfile.lock"))
            .map_err(|_| "tool did not produce Gemfile.lock".to_owned())?;
        std::fs::write(cwd.join("Gemfile.lock"), &after).map_err(|e| e.to_string())?;
        let before = baseline.get(&lock).map(|b| b.content.as_str());
        let manifest = context::join(directory, "Gemfile");
        let edited = baseline.get(&manifest).map(|b| b.content.clone())
            != std::fs::read_to_string(root.join(&manifest)).ok();
        if !maintenance && (edited || !gems.is_empty()) && before == Some(after.as_str()) {
            let trigger = if edited {
                context::Trigger::ManifestEdit
            } else {
                context::Trigger::TargetedUpgrade
            };
            return Err(context::unchanged("Gemfile.lock", directory, trigger));
        }
        Ok(())
    }
}

fn path_of(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
