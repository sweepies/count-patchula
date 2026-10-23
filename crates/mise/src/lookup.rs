//! Version lists from the pinned mise, run in a sandbox.
use crate::{Error, policy, sandbox_env};
use patchula_process::{CommandSpec, Executor};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

/// The mise package the Count pins, installed into its own tool store.
pub const PACKAGE: &str = "aqua:jdx/mise";
/// Backends mise skips even when a registry shorthand lists them first.
const DISABLED_BACKENDS: &str = "asdf,vfox";
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(180);
/// A slow host can take most of this to serve the mise archive once.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(1800);
const OUTPUT_LIMIT: usize = 16 << 20;
const PARALLEL: usize = 4;

/// One version in a `mise ls-remote --json` listing. Backends leave out what
/// they do not know: Node gives a date only, GitHub backends add a release URL.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub release_url: Option<String>,
    #[serde(default)]
    pub prerelease: Option<bool>,
}

/// What to ask mise. Deliberately not Debug: it carries a token.
pub struct Query<'a> {
    pub tool: &'a str,
    /// A version prefix mise filters by, such as `lts` for Node.
    pub prefix: Option<&'a str>,
    /// Passed to mise as `MISE_GITHUB_TOKEN`, and nothing else is inherited.
    pub token: Option<&'a str>,
    /// Whether mise may read the public mise-versions mirror before upstream.
    pub versions_host: bool,
}

pub struct Lookup {
    executor: Arc<dyn Executor>,
    /// A mise to bootstrap with; it only installs and starts the pinned one.
    mise: PathBuf,
    data: PathBuf,
    version: String,
    installed: tokio::sync::OnceCell<()>,
    slots: tokio::sync::Semaphore,
}

#[derive(Deserialize)]
struct Tool {
    backend: String,
}

impl Lookup {
    pub fn new(executor: Arc<dyn Executor>, mise: PathBuf, data: PathBuf, version: String) -> Self {
        Self {
            executor,
            mise,
            data,
            version,
            installed: Default::default(),
            slots: tokio::sync::Semaphore::new(PARALLEL),
        }
    }
    /// The pinned mise version, which decides what listings look like.
    pub fn version(&self) -> &str {
        &self.version
    }
    fn spec(
        &self,
        home: &Path,
        work: &Path,
        token: Option<&str>,
        versions_host: bool,
    ) -> CommandSpec {
        let mut env = sandbox_env(home, &self.data);
        // Nothing installs while looking up: the pinned mise is installed first
        // and a lookup never needs another tool.
        env.insert("MISE_AUTO_INSTALL".into(), "0".into());
        // Registry shorthands that list an asdf or vfox plugin fall through to
        // the next backend, or fail when there is none.
        env.insert("MISE_DISABLE_BACKENDS".into(), DISABLED_BACKENDS.into());
        env.insert(
            "MISE_USE_VERSIONS_HOST".into(),
            if versions_host { "1" } else { "0" }.into(),
        );
        // Only the token passed here is used: no `gh` login, credential command
        // or Git credential helper (the last is off by default).
        env.insert("MISE_GITHUB_GH_CLI_TOKENS".into(), "0".into());
        if let Some(token) = token {
            env.insert("MISE_GITHUB_TOKEN".into(), token.into());
        }
        CommandSpec {
            program: self.mise.clone(),
            args: vec!["--no-config".into()],
            cwd: work.into(),
            env,
            timeout: LOOKUP_TIMEOUT,
            output_limit: OUTPUT_LIMIT,
            complete_output: true,
        }
    }
    /// Each run gets its own empty working directory and home, so there is no
    /// project for mise to find and nothing to carry between runs.
    async fn run(
        &self,
        token: Option<&str>,
        versions_host: bool,
        configure: impl FnOnce(&mut CommandSpec),
    ) -> Result<Vec<u8>, Error> {
        let scratch = tempfile::tempdir().map_err(|e| Error::Failed(e.to_string()))?;
        let (home, work) = (scratch.path().join("home"), scratch.path().join("work"));
        for dir in [&home, &work] {
            std::fs::create_dir_all(dir).map_err(|e| Error::Failed(e.to_string()))?;
        }
        let mut spec = self.spec(&home, &work, token, versions_host);
        configure(&mut spec);
        self.executor.run(spec).await.map_err(Error::Failed)
    }
    async fn install(&self, token: Option<&str>) -> Result<(), Error> {
        self.installed
            .get_or_try_init(|| async {
                self.run(token, true, |spec| {
                    spec.args
                        .extend(["install".into(), format!("{PACKAGE}@{}", self.version)]);
                    spec.timeout = INSTALL_TIMEOUT;
                    spec.output_limit = 1 << 16;
                })
                .await
                .map(drop)
            })
            .await
            .map(drop)
    }
    /// The pinned mise, started through the bootstrap one with the tool store.
    fn exec(&self, spec: &mut CommandSpec, args: &[&str]) {
        spec.args.extend([
            "exec".into(),
            format!("{PACKAGE}@{}", self.version),
            "--".into(),
            "mise".into(),
        ]);
        spec.args.extend(args.iter().map(|a| (*a).to_owned()));
    }
    /// `mise ls-remote --json --prerelease`, as mise lists the tool. Prereleases
    /// are listed so the Count's own policy decides who is offered one.
    pub async fn ls_remote(&self, query: &Query<'_>) -> Result<Vec<Release>, Error> {
        policy::check(query.tool)?;
        if let Some(prefix) = query.prefix {
            policy::check(prefix)?;
        }
        let _slot = self
            .slots
            .acquire()
            .await
            .map_err(|e| Error::Failed(e.to_string()))?;
        self.install(query.token).await?;
        let mut args = vec!["ls-remote", "--json", "--prerelease", "--", query.tool];
        args.extend(query.prefix);
        let out = self
            .run(query.token, query.versions_host, |spec| {
                self.exec(spec, &args)
            })
            .await?;
        let releases: Vec<Release> = serde_json::from_slice(&out)
            .map_err(|e| Error::Unreadable(format!("`{}`: {e}", query.tool)))?;
        if releases.is_empty() {
            return Err(Error::Empty(query.tool.into()));
        }
        Ok(releases)
    }
    /// The backend mise picks for a tool, such as `npm:prettier` for `prettier`.
    pub async fn backend(&self, tool: &str, token: Option<&str>) -> Result<String, Error> {
        policy::check(tool)?;
        let _slot = self
            .slots
            .acquire()
            .await
            .map_err(|e| Error::Failed(e.to_string()))?;
        self.install(token).await?;
        let out = self
            .run(token, true, |spec| {
                self.exec(spec, &["tool", "--json", "--", tool])
            })
            .await?;
        let tool: Tool = serde_json::from_slice(&out)
            .map_err(|e| Error::Unreadable(format!("`{tool}`: {e}")))?;
        Ok(tool.backend)
    }
}
