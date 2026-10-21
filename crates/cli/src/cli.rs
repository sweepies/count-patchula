use std::path::PathBuf;
use usage_rs::{Args, Cli, Subcommands, ValueEnum};

/// Count Patchula vants to update your deps. A fast dependency update bot for GitHub.
///
/// Exit codes: 0 = success; 1 = runtime error; 2 = usage or configuration error.
///
/// No interactive prompts are used. Future prompts require both stdin and stdout
/// to be TTYs and use demand.
#[derive(Cli)]
#[usage(bin = "patchula", version)]
pub struct App {
    /// Run the API-only GitHub App webhook server (no subcommand).
    #[usage(long)]
    pub server: bool,
    /// Operator Pkl settings file; also PATCHULA_SERVER_SETTINGS.
    #[usage(long)]
    pub settings: Option<PathBuf>,
    /// Server bind address; also PATCHULA_LISTEN (default 127.0.0.1:8080).
    #[usage(long)]
    pub listen: Option<std::net::SocketAddr>,
    /// Persistent server cache/queue; also PATCHULA_SERVER_CACHE.
    #[usage(long)]
    pub server_cache: Option<PathBuf>,
    /// GitHub App ID; also PATCHULA_APP_ID.
    #[usage(long)]
    pub app_id: Option<u64>,
    /// PEM private key file; also PATCHULA_APP_PRIVATE_KEY_FILE.
    #[usage(long)]
    pub private_key_file: Option<PathBuf>,
    /// Webhook secret file; also PATCHULA_WEBHOOK_SECRET_FILE.
    #[usage(long)]
    pub webhook_secret_file: Option<PathBuf>,
    /// Graceful server drain deadline; also PATCHULA_SHUTDOWN_TIMEOUT_SECS (default 60).
    #[usage(long)]
    pub shutdown_timeout_secs: Option<u64>,
    /// Checkbox wording; also PATCHULA_RECHECK. Default: now in server, next-run in CLI.
    #[usage(long, value_enum, global)]
    pub recheck: Option<Recheck>,
    /// Increase diagnostic verbosity; repeat for more detail.
    #[usage(short = 'v', long, count, global)]
    pub verbose: u8,
    /// Control ANSI colors in diagnostics.
    #[usage(long, value_enum, default = "auto", global)]
    pub color: Color,
    /// Emit machine-readable command results and errors (schema stays Pkl).
    #[usage(long, global)]
    pub json: bool,
    #[usage(subcommand)]
    pub command: Option<Command>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Color {
    Auto,
    Always,
    Never,
}

#[derive(Subcommands)]
pub enum Command {
    /// Inspect and validate Pkl configuration.
    Config(Config),
    /// Extract dependencies and report available updates without writing repositories.
    Lookup(Lookup),
    /// Create or update dependency branches and pull requests with verified API commits.
    Run(Run),
    /// Manage the lockfile runner's tool store (`PATCHULA_TOOLS_DIR`, or `<cache>.tools`).
    Tools(Tools),
    /// Browse dependencies and updates in the terminal; with `--local`, apply selected updates.
    Tui(Tui),
}

#[derive(Args)]
pub struct Tui {
    #[usage(flatten)]
    pub lookup: Lookup,
    /// Print a plain-text snapshot of the updates and exit, for pipes and CI logs.
    #[usage(long)]
    pub once: bool,
}

#[derive(Args)]
pub struct Tools {
    #[usage(subcommand)]
    pub command: ToolsCommand,
}

#[derive(Subcommands)]
pub enum ToolsCommand {
    /// Install the pinned lockfile toolchains ahead of time, e.g. in an image build.
    Install(ToolStore),
    /// Uninstall tool versions no run has used recently; pinned defaults stay.
    Prune(Prune),
}

#[derive(Args)]
pub struct ToolStore {
    /// Shared SQLite cache path; the tool store sits beside it.
    #[usage(long, default = ".patchula-cache.sqlite")]
    pub cache: PathBuf,
}

#[derive(Args)]
pub struct Prune {
    #[usage(flatten)]
    pub store: ToolStore,
    /// Remove versions unused for at least this many days.
    #[usage(long, default = "30")]
    pub days: i64,
}

#[derive(Args)]
pub struct Run {
    #[usage(flatten)]
    pub lookup: Lookup,
    /// Print planned branches, files and unified diffs without writing to GitHub.
    #[usage(long)]
    pub dry_run: bool,
    /// Permit manifest-only test branches for unsupported binary bun locks.
    #[usage(long)]
    pub allow_lockfile_pending: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Recheck {
    Now,
    NextRun,
}

#[derive(Args)]
pub struct Lookup {
    /// Repositories as owner/repo; omit to use the current directory's GitHub remote.
    pub repositories: Vec<String>,
    /// Process every repository the token can reach instead of naming them.
    #[usage(long, conflicts = "--local")]
    pub all_repos: bool,
    /// Shared SQLite cache path.
    #[usage(long, default = ".patchula-cache.sqlite")]
    pub cache: PathBuf,
    /// Maximum concurrent repository scans (1–100); HTTP limits are per host.
    #[usage(long, default = "8")]
    pub concurrency: usize,
    /// Override a host's HTTP limit as HOST=LIMIT (1–100); repeat per host. Default: 16.
    #[usage(long)]
    pub host_concurrency: Vec<String>,
    /// Freshness seconds for OCI tag lists without validators; 0 uses upstream policy.
    #[usage(long, default = "3600")]
    pub registry_freshness_secs: u64,
    /// Minimum freshness seconds for Go proxy lists/latest; 0 uses upstream policy.
    #[usage(long, default = "3600")]
    pub go_freshness_secs: u64,
    /// Local Pkl override applied to every selected repository.
    #[usage(long, conflicts = "--config-dir")]
    pub config: Option<PathBuf>,
    /// Per-repository local overrides: `<dir>/<owner>/<name>.pkl` wins over `<dir>/<name>.pkl`.
    /// Unmatched repositories discover `.github/patchula.pkl` then `patchula.pkl`; neither means skipped.
    /// All matched files are validated before repository scans. Mutually exclusive with `--config`.
    #[usage(long, conflicts = "--config")]
    pub config_dir: Option<PathBuf>,
    /// Read one repository from this local directory instead of GitHub. Lookups still reach
    /// registries, and nothing is written: use it with `lookup` or `run --dry-run`.
    #[usage(long, conflicts = "--config-dir")]
    pub local: Option<PathBuf>,
}

#[derive(Args)]
pub struct Config {
    #[usage(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Subcommands)]
pub enum ConfigCommand {
    /// Validate a Pkl config file or discover one in a repository directory.
    Check(Check),
    /// Print the bundled Patchula.pkl schema as Pkl source.
    Schema,
    /// Print the bundled PatchulaServer.pkl operator schema.
    ServerSchema,
    /// Validate an operator Pkl server settings file without credentials or network.
    CheckServer(Check),
}

#[derive(Args)]
pub struct Check {
    /// Pkl file or repository directory to check.
    pub path: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_has_no_subcommand_and_global_recheck_still_works_for_run() {
        let app = App::parse_from(
            &[
                "--server",
                "--settings",
                "/operator/server.pkl",
                "--recheck",
                "next-run",
            ]
            .map(std::ffi::OsStr::new),
        )
        .unwrap();
        assert!(app.server);
        assert!(app.command.is_none());
        assert_eq!(app.recheck, Some(Recheck::NextRun));
        let app =
            App::parse_from(&["run", "--recheck", "now", "owner/repo"].map(std::ffi::OsStr::new))
                .unwrap();
        assert_eq!(app.recheck, Some(Recheck::Now));
        assert!(matches!(app.command, Some(Command::Run(_))));
    }

    #[test]
    fn globals_work_after_nested_commands_and_verbose_counts() {
        let args = ["config", "schema", "-vvv", "--color", "never", "--json"];
        let words: Vec<_> = args.iter().map(std::ffi::OsStr::new).collect();
        let app = App::parse_from(&words).unwrap();
        assert_eq!(app.verbose, 3);
        assert_eq!(app.color, Color::Never);
        assert!(app.json);
    }

    #[test]
    fn exact_runner_invocation_and_optional_repositories() {
        let words = [
            "lookup",
            "--config",
            "examples/renovate-runner.pkl",
            "--cache",
            "tmp/cache.sqlite",
            "--concurrency",
            "4",
            "--json",
            "sweepies/renovate-runner",
        ]
        .map(std::ffi::OsStr::new);
        let app = App::parse_from(&words).unwrap();
        assert!(app.json);
        let Some(Command::Lookup(options)) = app.command else {
            panic!("lookup")
        };
        assert_eq!(options.repositories, vec!["sweepies/renovate-runner"]);
        assert_eq!(options.concurrency, 4);
        let app = App::parse_from(&[std::ffi::OsStr::new("lookup")]).unwrap();
        let Some(Command::Lookup(options)) = app.command else {
            panic!("lookup")
        };
        assert!(options.repositories.is_empty());
    }

    #[test]
    fn host_limits_repeat_without_consuming_repositories() {
        let words = [
            "lookup",
            "--host-concurrency",
            "api.github.com=32",
            "--host-concurrency",
            "ghcr.io=8",
            "--go-freshness-secs",
            "0",
            "owner/repo",
        ]
        .map(std::ffi::OsStr::new);
        let app = App::parse_from(&words).unwrap();
        let Some(Command::Lookup(options)) = app.command else {
            panic!("lookup")
        };
        assert_eq!(options.repositories, ["owner/repo"]);
        assert_eq!(options.host_concurrency, ["api.github.com=32", "ghcr.io=8"]);
        assert_eq!(options.go_freshness_secs, 0);
    }
}
