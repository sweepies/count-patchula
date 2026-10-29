use std::path::PathBuf;
use usage_rs::help::Palette;
use usage_rs::{Args, Cli, Subcommands, ValueEnum};

/// The bat beside the root help page.
const LOGO: &str = "
⠂⣤⣄⣀     ⡄⢠     ⣀⣠⣤⠐
 ⢹⣿⣿⣿⣷⣶⣤⣀⣿⣿⣀⣤⣶⣾⣿⣿⣿⡏
  ⠋⠙⢿⠿⣿⣿⣿⣿⣿⣿⣿⣿⠿⡿⠋⠙
      ⠘⠋⠹⢿⡿⠏⠙⠃
         ⠈⠁
";

/// Help colours: accent headings, bold names, dim placeholders. The accent
/// and the bat's gradient are painted by [`crate::palette::help`].
pub const PALETTE: Palette = Palette::DEFAULT
    .heading(crate::palette::HELP_HEADING)
    .option("bold")
    .metavar("dim")
    .command("bold");

/// Count Patchula vants to update your deps. A modern dependency updater with strong defaults.
///
/// Exit codes: 0 = success; 1 = runtime error; 2 = usage or configuration error;
/// `lookup` and `apply` add 4 = a lookup failed; `apply` adds 3 = updates still waiting.
#[derive(Cli)]
#[usage(
    bin = "patchula",
    version,
    unknown_flags = "error",
    logo = LOGO,
    logo_style = "bright-yellow",
    help_template = "{$dim}{{about}}{/$}

{{usage}}

{{commands}}

{{args}}

{{flags}}

{{after_help}}

{$dim+italic}Run 'patchula <command> --help' for more information on a command.{/$}",
    example("patchula lookup", help = "Report updates for the checkout here"),
    example(
        "patchula run --dry-run",
        help = "Plan branches and diffs, writing nothing"
    ),
    example("patchula tui", help = "Browse updates and apply the ones you pick"),
    example(
        "patchula apply --check",
        help = "Exit 3 in CI while updates are waiting"
    ),
    example(
        "patchula apply --manager github-actions",
        help = "Write only GitHub Actions updates"
    ),
    example(
        "patchula serve --settings server.pkl",
        help = "Run the GitHub App webhook server"
    )
)]
pub struct App {
    /// When a ticked checkbox says it takes effect; also PATCHULA_RECHECK. Default: now in the server, next-run in the CLI.
    #[usage(long, value_enum, value_name = "WHEN", global)]
    pub recheck: Option<Recheck>,
    /// Show request counts and phase timings after a batch; repeat for debug logs.
    #[usage(short = 'v', long, count, global)]
    pub verbose: u8,
    /// Control ANSI colors in diagnostics.
    #[usage(long, value_enum, value_name = "WHEN", default = "auto", global)]
    pub color: Color,
    /// Emit command results and errors as JSON; configs stay Pkl.
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
    /// Write available updates into the working tree here, without branches or pull requests.
    ///
    /// Exit codes: 0 = nothing selected is left to apply; 1 = runtime error; 2 = usage or
    /// configuration error; 3 = selected updates are still waiting: `--check` or `--dry-run`
    /// found some, or some could not be written in place; 4 = a dependency's lookup failed, so
    /// its updates are unknown. 4 takes precedence over 3.
    #[usage(
        example("patchula apply", help = "Write every available update here"),
        example(
            "patchula apply --check",
            help = "Write nothing; exit 3 if updates are waiting"
        ),
        example(
            "patchula apply --dry-run --type patch",
            help = "Show the patch updates as a diff"
        ),
        example(
            "patchula apply --manager github-actions --dependency 'glob:actions/*'",
            help = "Write only the actions/* updates"
        )
    )]
    Apply(Apply),
    /// Sign in to or out of a Count Patchula server for `--remote`.
    #[usage(
        example(
            "patchula auth login --server https://patchula.example.com",
            help = "Sign in through the server's GitHub App"
        ),
        example("patchula auth logout", help = "Forget the token for PATCHULA_SERVER")
    )]
    Auth(Auth),
    /// Inspect and validate Pkl configuration.
    #[usage(
        example(
            "patchula config check .",
            help = "Find and check this repository's config"
        ),
        example("patchula config check patchula.pkl", help = "Check one config file"),
        example(
            "patchula config schema > PatchulaConfig.pkl",
            help = "Save the schema for your editor"
        ),
        example(
            "patchula config check-server server.pkl",
            help = "Check operator settings"
        )
    )]
    Config(Config),
    /// Report available updates without writing; with no repository, reads the working tree here.
    #[usage(
        example("patchula lookup", help = "Report updates for the checkout here"),
        example(
            "patchula lookup acme/app acme/api",
            help = "Read two repositories through the API"
        ),
        example("patchula lookup --json", help = "The same report, as JSON"),
        example(
            "patchula lookup --remote",
            help = "Look up on your Count Patchula server"
        )
    )]
    Lookup(Lookup),
    /// Create or update dependency branches and pull requests with verified API commits.
    ///
    /// With no repository, `--dry-run` plans from the working tree, uncommitted changes
    /// included. A real run with no repository opens pull requests for this checkout's GitHub
    /// repository and requires a clean tree at the remote's default branch.
    ///
    /// Failed lookups are reported but the run still exits 0, since it keeps the pull requests
    /// it could plan; it holds off auto-merge and closing pull requests for that repository.
    #[usage(
        example(
            "patchula run --dry-run",
            help = "Plan branches and diffs from this tree"
        ),
        example(
            "patchula run acme/app",
            help = "Open or update pull requests for acme/app"
        ),
        example(
            "patchula run --all-repos --config-dir overrides",
            help = "Every repository the token reaches"
        )
    )]
    Run(Run),
    /// Run the API-only GitHub App webhook server.
    #[usage(
        example(
            "patchula serve --settings server.pkl",
            help = "Serve with operator settings"
        ),
        example(
            "patchula serve --settings server.pkl --listen 0.0.0.0:8080",
            help = "Listen on every interface"
        )
    )]
    Serve(Serve),
    /// Manage the lockfile runner's tool store (`PATCHULA_TOOLS_DIR`, or `tools/` in the cache directory).
    #[usage(
        example(
            "patchula tools install",
            help = "Install the pinned toolchains, e.g. in an image"
        ),
        example(
            "patchula tools prune --days 14",
            help = "Drop versions unused for two weeks"
        )
    )]
    Tools(Tools),
    /// Browse dependencies and updates in the terminal; with no repository, apply selected updates to the working tree.
    #[usage(
        example("patchula tui", help = "Pick updates and apply them here"),
        example("patchula tui acme/app", help = "Browse a repository through the API"),
        example(
            "patchula tui --once",
            help = "Print a plain snapshot, for pipes and CI logs"
        )
    )]
    Tui(Tui),
}

#[derive(Args)]
pub struct Auth {
    #[usage(subcommand)]
    pub command: AuthCommand,
}

#[derive(Subcommands)]
pub enum AuthCommand {
    /// Sign in with the server's GitHub App, replacing any stored token.
    Login(AuthServer),
    /// Forget the stored token for a server.
    Logout(AuthServer),
}

#[derive(Args)]
pub struct AuthServer {
    /// Server URL; also PATCHULA_SERVER.
    #[usage(long, value_name = "URL")]
    pub server: Option<String>,
}

#[derive(Args)]
pub struct Serve {
    /// Operator Pkl settings file; also PATCHULA_SERVER_SETTINGS.
    #[usage(long, value_name = "FILE")]
    pub settings: Option<PathBuf>,
    /// Server bind address; also PATCHULA_LISTEN (default 127.0.0.1:8080).
    #[usage(long, value_name = "ADDR")]
    pub listen: Option<std::net::SocketAddr>,
    /// Server cache and queue directory; also PATCHULA_SERVER_CACHE. Default: `patchula/server`
    /// in the user cache directory.
    #[usage(long, value_name = "DIR")]
    pub cache: Option<PathBuf>,
    /// GitHub App ID; also PATCHULA_APP_ID.
    #[usage(long, value_name = "ID")]
    pub app_id: Option<u64>,
    /// PEM private key file; also PATCHULA_APP_PRIVATE_KEY_FILE.
    #[usage(long, value_name = "FILE")]
    pub private_key_file: Option<PathBuf>,
    /// Webhook secret file; also PATCHULA_WEBHOOK_SECRET_FILE.
    #[usage(long, value_name = "FILE")]
    pub webhook_secret_file: Option<PathBuf>,
    /// Graceful server drain deadline; also PATCHULA_SHUTDOWN_TIMEOUT_SECS (default 60).
    #[usage(long, value_name = "SECS")]
    pub shutdown_timeout_secs: Option<u64>,
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
    /// Cache directory, holding the tool store. Default: `patchula` in the user cache directory.
    #[usage(long, value_name = "DIR")]
    pub cache: Option<PathBuf>,
}

#[derive(Args)]
pub struct Prune {
    #[usage(flatten)]
    pub store: ToolStore,
    /// Remove versions unused for at least this many days.
    #[usage(long, value_name = "DAYS", default = "30")]
    pub days: i64,
}

#[derive(Args)]
pub struct Run {
    #[usage(flatten)]
    pub lookup: Lookup,
    /// Print planned branches, files and unified diffs without writing to GitHub.
    #[usage(long)]
    pub dry_run: bool,
    /// For testing: write manifest-only branches for binary `bun.lockb` lockfiles, which can't be regenerated.
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
    /// Repositories as owner/repo, read through the GitHub API; omit for the checkout here.
    #[usage(value_name = "OWNER/REPO")]
    pub repositories: Vec<String>,
    /// Process every repository the token can reach instead of naming them.
    #[usage(long)]
    pub all_repos: bool,
    #[usage(flatten)]
    pub scan: Scan,
    /// Per-repository overrides: `DIR/<owner>/<name>.pkl`, then `DIR/<name>.pkl`; other
    /// repositories use their own `patchula.pkl`, or are skipped without one. Conflicts with `--config`.
    #[usage(long, value_name = "DIR", conflicts = "--config")]
    pub config_dir: Option<PathBuf>,
    /// Run on a Count Patchula server (PATCHULA_SERVER, or the one found for this checkout)
    /// with its credentials and caches; local config, cache and HTTP flags don't apply.
    #[usage(long, conflicts = "--here")]
    pub remote: bool,
    /// Run on this machine, even if you chose to run this checkout on a server.
    #[usage(long)]
    pub here: bool,
}

/// How the Count reads and looks up, for every command that runs on this machine.
#[derive(Args, Clone)]
pub struct Scan {
    /// Cache directory, shared by every checkout. Default: `patchula` in the user cache directory
    /// (`~/Library/Caches` on macOS, else `XDG_CACHE_HOME` or `~/.cache`).
    #[usage(long, value_name = "DIR")]
    pub cache: Option<PathBuf>,
    /// Maximum concurrent repository scans (1–100); HTTP limits are per host.
    #[usage(long, value_name = "N", default = "8")]
    pub concurrency: usize,
    /// Override a host's HTTP limit (1–100); repeat per host. Default: 16.
    #[usage(long, value_name = "HOST=LIMIT")]
    pub host_concurrency: Vec<String>,
    /// Freshness seconds for OCI tag lists without validators; 0 uses upstream policy.
    #[usage(long, value_name = "SECS", default = "3600")]
    pub registry_freshness_secs: u64,
    /// Minimum freshness seconds for Go proxy lists/latest; 0 uses upstream policy.
    #[usage(long, value_name = "SECS", default = "3600")]
    pub go_freshness_secs: u64,
    /// Local Pkl override applied to every selected repository.
    #[usage(long, value_name = "FILE")]
    pub config: Option<PathBuf>,
    /// Use the Count's defaults when the working tree has no `patchula.pkl`, instead of asking.
    /// Conflicts with `--config` and `--config-dir`.
    #[usage(long)]
    pub default_config: bool,
}

#[derive(Args)]
pub struct Apply {
    #[usage(flatten)]
    pub scan: Scan,
    /// Only updates from this manager, such as github-actions; repeat for more.
    #[usage(long, value_name = "MANAGER")]
    pub manager: Vec<String>,
    /// Only dependencies matching this name or package, with config's `glob:`, `regex:` and `!`; repeat for more.
    #[usage(long, value_name = "PATTERN")]
    pub dependency: Vec<String>,
    /// Only updates of this type; repeat for more.
    #[usage(long = "type", value_enum, value_name = "TYPE")]
    pub update_type: Vec<UpdateType>,
    /// Print the unified diff of what would be written, and write nothing.
    #[usage(long, conflicts = "--check")]
    pub dry_run: bool,
    /// Write nothing; exit 3 if any selected update is waiting, or 4 if a lookup failed.
    #[usage(long)]
    pub check: bool,
    /// Also run your own `copier update` for each selected template, before the other updates.
    ///
    /// The template's code runs on this machine as you. The tree must be clean, the Count
    /// never passes `--trust`, and copier asks its questions on the terminal.
    #[usage(long, conflicts = "--dry-run")]
    pub copier: bool,
    /// With `--copier`, take the defaults for questions copier would ask, for use without a terminal.
    #[usage(long)]
    pub copier_defaults: bool,
    #[usage(hide)]
    pub repositories: Vec<String>,
    #[usage(long, hide)]
    pub all_repos: bool,
    #[usage(long, hide)]
    pub remote: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum UpdateType {
    Major,
    Minor,
    Patch,
    Pin,
    Digest,
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
    /// Print the bundled PatchulaConfig.pkl schema as Pkl source.
    Schema,
    /// Print the bundled PatchulaServer.pkl operator schema.
    ServerSchema,
    /// Validate an operator Pkl server settings file without credentials or network.
    CheckServer(CheckServer),
}

#[derive(Args)]
pub struct Check {
    /// Pkl file or repository directory to check.
    pub path: PathBuf,
}

#[derive(Args)]
pub struct CheckServer {
    /// Server settings file to check.
    pub path: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use usage_rs::help::{Style, render_at_styled};

    fn pages<'a>(route: &mut Vec<&'a usage_rs::Command<'a>>, style: Style, out: &mut String) {
        out.push_str(&render_at_styled(App::spec(), route, true, style).unwrap());
        let Some(command) = route.last() else { return };
        for sub in command.subcommands {
            route.push(sub);
            pages(route, style, out);
            route.pop();
        }
    }

    #[test]
    fn coloured_help_uses_the_palette_and_template() {
        let mut pages = String::new();
        let style = Style::COLOURED.palette(PALETTE);
        self::pages(&mut vec![App::command()], style, &mut pages);
        let pages = crate::palette::help(&pages, crate::palette::Depth::True);
        assert!(pages.contains("\x1b[1;38;2;232;90;79mUsage:\x1b[0m"));
        assert!(pages.contains("\x1b[1;38;2;232;90;79mExamples:\x1b[0m"));
        assert!(
            !pages.contains(";36m") && !pages.contains("[36m"),
            "no teal is left"
        );
        assert!(pages.contains("\x1b[1m--dry-run\x1b[0m"));
        assert!(pages.contains("\x1b[2m<MANAGER>\x1b[0m"));
        assert!(pages.contains("\x1b[2;3mRun 'patchula <command> --help'"));
    }

    #[test]
    fn serve_takes_its_flags_and_global_recheck_still_works_for_run() {
        let app = App::parse_from(
            &[
                "serve",
                "--settings",
                "/operator/server.pkl",
                "--recheck",
                "next-run",
            ]
            .map(std::ffi::OsStr::new),
        )
        .unwrap();
        let Some(Command::Serve(serve)) = &app.command else {
            panic!("serve")
        };
        assert_eq!(
            serve.settings.as_deref(),
            Some(std::path::Path::new("/operator/server.pkl"))
        );
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
            "examples/renovate-migration.pkl",
            "--cache",
            "tmp/cache",
            "--concurrency",
            "4",
            "--json",
            "acme/runner",
        ]
        .map(std::ffi::OsStr::new);
        let app = App::parse_from(&words).unwrap();
        assert!(app.json);
        let Some(Command::Lookup(options)) = app.command else {
            panic!("lookup")
        };
        assert_eq!(options.repositories, vec!["acme/runner"]);
        assert_eq!(options.scan.concurrency, 4);
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
        assert_eq!(
            options.scan.host_concurrency,
            ["api.github.com=32", "ghcr.io=8"]
        );
        assert_eq!(options.scan.go_freshness_secs, 0);
    }

    #[test]
    fn apply_filters_repeat() {
        let words = [
            "apply",
            "--type",
            "pin",
            "--type",
            "digest",
            "--manager",
            "github-actions",
            "--dependency",
            "glob:actions/*",
            "--check",
        ]
        .map(std::ffi::OsStr::new);
        let app = App::parse_from(&words).unwrap();
        let Some(Command::Apply(apply)) = app.command else {
            panic!("apply")
        };
        assert_eq!(apply.update_type, [UpdateType::Pin, UpdateType::Digest]);
        assert_eq!(apply.manager, ["github-actions"]);
        assert_eq!(apply.dependency, ["glob:actions/*"]);
        assert!(apply.check && !apply.dry_run);
    }

    #[test]
    fn remote_and_here_are_bare_flags_that_conflict() {
        fn words<'a>(args: &[&'a str]) -> Vec<&'a std::ffi::OsStr> {
            args.iter().map(|arg| std::ffi::OsStr::new(*arg)).collect()
        }
        let app = App::parse_from(&words(&["tui", "--remote", "--once"])).unwrap();
        let Some(Command::Tui(tui)) = app.command else {
            panic!("tui")
        };
        assert!(tui.lookup.remote && tui.once);
        assert!(tui.lookup.repositories.is_empty());
        let app = App::parse_from(&words(&["run", "--remote", "acme/app"])).unwrap();
        let Some(Command::Run(run)) = app.command else {
            panic!("run")
        };
        assert_eq!(run.lookup.repositories, ["acme/app"]);
        assert!(App::parse_from(&words(&["lookup", "--remote", "--here"])).is_err());
        assert!(App::parse_from(&words(&["lookup", "--no-remote"])).is_err());
        assert!(App::parse_from(&words(&["lookup", "--local"])).is_err());
        assert!(App::parse_from(&words(&["lookup", "--server", "https://x.dev"])).is_err());
    }
}
