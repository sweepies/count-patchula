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
/// `lookup`, `tui --once` and `apply` add 4 = a lookup failed; `apply` adds 3 = updates still waiting;
/// `run` adds 5 = a repository changed during the run; run again.
#[derive(Cli)]
#[usage(
    bin = "patchula",
    version,
    completion,
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
        "patchula remote run",
        help = "Run this checkout's repository on its Count server"
    ),
    example(
        "patchula serve --settings settings.pkl",
        help = "Run the GitHub App webhook server"
    )
)]
pub struct App {
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

/// `--type`'s value: an update kind, listed in rank order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpdateType(pub patchula_words::Update);
impl usage_rs::spec::ValueEnum for UpdateType {
    const CHOICES: &'static [&'static str] = patchula_words::Update::WORDS;

    fn from_choice(value: &str) -> Option<Self> {
        value.parse().ok().map(Self)
    }
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
    /// Inspect the cache directory's database.
    #[usage(example(
        "patchula cache schema",
        help = "Print the cache schema this binary supports"
    ))]
    Cache(Cache),
    /// Print or install shell completion scripts.
    #[usage(
        example(
            "patchula completion fish --install",
            help = "Install fish completions"
        ),
        example("patchula completion zsh", help = "Print the zsh completion script")
    )]
    Completion(usage_rs::completion::Completion),
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
            "patchula config check-settings settings.pkl",
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
        example("patchula lookup --json", help = "The same report, as JSON")
    )]
    Lookup(Lookup),
    /// Run commands on a Count Patchula server with its credentials and warm caches.
    #[usage(
        example(
            "patchula remote lookup",
            help = "Look up this checkout's repository on its server"
        ),
        example(
            "patchula remote run",
            help = "Open or update pull requests on the server"
        ),
        example(
            "patchula remote run --dry-run acme/app",
            help = "Plan pull requests for acme/app on the server"
        ),
        example(
            "patchula remote tui",
            help = "Browse updates from the server in the terminal"
        ),
        example(
            "patchula remote login --server https://patchula.example.com",
            help = "Sign in through the server's GitHub App"
        )
    )]
    Remote(Remote),
    /// Create or update dependency branches and pull requests on GitHub with verified API commits.
    ///
    /// Reads GitHub rather than the working tree, even with `--dry-run`. With no repository,
    /// runs this checkout's GitHub repository; `--dir` with `--dry-run` plans from a local
    /// directory instead.
    ///
    /// Failed lookups are reported but the run still exits 0, since it keeps the pull requests
    /// it could plan; it holds off auto-merge and closing pull requests for that repository.
    #[usage(
        example(
            "patchula run --dry-run acme/app",
            help = "Plan branches and diffs for acme/app, writing nothing"
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
            "patchula serve --settings settings.pkl",
            help = "Serve with operator settings"
        ),
        example(
            "patchula serve --settings settings.pkl --listen 0.0.0.0:8080",
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
pub struct Remote {
    /// Server URL; also PATCHULA_SERVER. Omit to discover the server advertised by the repository.
    #[usage(long, value_name = "URL", global)]
    pub server: Option<String>,
    #[usage(subcommand)]
    pub command: RemoteCommand,
}

#[derive(Subcommands)]
pub enum RemoteCommand {
    /// Sign in with the server's GitHub App, replacing any stored token.
    Login,
    /// Forget the stored token for a server and any remembered repositories using it.
    Logout,
    /// Report available updates from a Count Patchula server without writing.
    Lookup(RemoteLookup),
    /// Create or update dependency branches and pull requests on a Count Patchula server.
    Run(RemoteRun),
    /// Browse dependencies and updates from a Count Patchula server in the terminal.
    Tui(RemoteTui),
}

#[derive(Args)]
pub struct RemoteLookup {
    /// Repositories as owner/repo; omit for this checkout's GitHub repository.
    #[usage(value_name = "OWNER/REPO")]
    pub repositories: Vec<String>,
}

#[derive(Args)]
pub struct RemoteRun {
    /// Repositories as owner/repo; omit for this checkout's GitHub repository.
    #[usage(value_name = "OWNER/REPO")]
    pub repositories: Vec<String>,
    /// Print planned branches, files and unified diffs without writing to GitHub.
    #[usage(long)]
    pub dry_run: bool,
}

#[derive(Args)]
pub struct RemoteTui {
    /// Repositories as owner/repo; omit for this checkout's GitHub repository.
    #[usage(value_name = "OWNER/REPO")]
    pub repositories: Vec<String>,
    /// Print a plain-text snapshot of the updates and exit with `lookup`'s exit code, for pipes and CI logs.
    #[usage(long)]
    pub once: bool,
}

#[derive(Args)]
pub struct Cache {
    #[usage(subcommand)]
    pub command: CacheCommand,
}

#[derive(Subcommands)]
pub enum CacheCommand {
    /// Print the cache database schema this binary supports; it refuses a cache from a newer one.
    Schema,
}

#[derive(Args)]
pub struct Serve {
    /// Operator settings file; also PATCHULA_SETTINGS.
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
    /// When a ticked checkbox says it takes effect; also PATCHULA_RECHECK. Default: now.
    #[usage(long, choices("now", "next-run"), value_name = "WHEN")]
    pub recheck: Option<patchula_words::Recheck>,
}

#[derive(Args)]
pub struct Tui {
    #[usage(flatten)]
    pub lookup: Lookup,
    /// Print a plain-text snapshot of the updates and exit with `lookup`'s exit code, for pipes and CI logs.
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
    /// Repositories as owner/repo, read through the GitHub API; omit for this checkout's GitHub repository.
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
    /// Plan branches and diffs from a local directory instead of GitHub; requires `--dry-run`.
    #[usage(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,
    /// When a ticked checkbox says it takes effect; also PATCHULA_RECHECK. Default: next-run.
    #[usage(long, choices("now", "next-run"), value_name = "WHEN")]
    pub recheck: Option<patchula_words::Recheck>,
    /// Print planned branches, files and unified diffs without writing to GitHub.
    #[usage(long)]
    pub dry_run: bool,
    /// For testing: write manifest-only branches for binary `bun.lockb` lockfiles, which can't be regenerated.
    #[usage(long)]
    pub allow_lockfile_pending: bool,
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
    /// Use the Count's defaults when the working tree has no `patchula.pkl`, instead of asking.
    /// Conflicts with `--config` and `--config-dir`.
    #[usage(long)]
    pub default_config: bool,
}

/// How the Count reads and looks up on this machine.
#[derive(Args, Clone)]
pub struct Scan {
    /// Cache directory, shared by every checkout. Default: `patchula` in the user cache directory
    /// (`~/Library/Caches` on macOS, else `XDG_CACHE_HOME` or `~/.cache`).
    #[usage(long, value_name = "DIR")]
    pub cache: Option<PathBuf>,
    /// Operator settings file; also PATCHULA_SETTINGS. Default: `patchula/settings.pkl` in the
    /// user config directory (`XDG_CONFIG_HOME` or `~/.config`), when it exists.
    #[usage(long, value_name = "FILE")]
    pub settings: Option<PathBuf>,
    /// Maximum concurrent repository scans (1–100), overriding the settings file; HTTP limits
    /// are per host. Default: 8.
    #[usage(long, value_name = "N")]
    pub concurrency: Option<usize>,
    /// Local Pkl override applied to every selected repository.
    #[usage(long, value_name = "FILE")]
    pub config: Option<PathBuf>,
}

#[derive(Args)]
pub struct Apply {
    #[usage(flatten)]
    pub scan: Scan,
    /// Use the Count's defaults when the working tree has no `patchula.pkl`, instead of asking.
    /// Conflicts with `--config`.
    #[usage(long)]
    pub default_config: bool,
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
    /// Print the bundled PatchulaSettings.pkl operator schema.
    SettingsSchema,
    /// Validate an operator settings file without credentials or network.
    CheckSettings(CheckSettings),
}

#[derive(Args)]
pub struct Check {
    /// Pkl file or repository directory to check.
    pub path: PathBuf,
}

#[derive(Args)]
pub struct CheckSettings {
    /// Settings file to check.
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
    fn serve_and_run_take_recheck_while_other_commands_reject_it() {
        let app = App::parse_from(
            &[
                "serve",
                "--settings",
                "/operator/settings.pkl",
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
            Some(std::path::Path::new("/operator/settings.pkl"))
        );
        assert_eq!(serve.recheck, Some(patchula_words::Recheck::NextRun));
        let app =
            App::parse_from(&["run", "--recheck", "now", "owner/repo"].map(std::ffi::OsStr::new))
                .unwrap();
        let Some(Command::Run(run)) = &app.command else {
            panic!("run")
        };
        assert_eq!(run.recheck, Some(patchula_words::Recheck::Now));
        assert!(
            App::parse_from(&["lookup", "--recheck", "now"].map(std::ffi::OsStr::new)).is_err()
        );
        assert!(
            App::parse_from(&["remote", "run", "--recheck", "now"].map(std::ffi::OsStr::new))
                .is_err()
        );
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
        assert_eq!(options.scan.concurrency, Some(4));
        let app = App::parse_from(&[std::ffi::OsStr::new("lookup")]).unwrap();
        let Some(Command::Lookup(options)) = app.command else {
            panic!("lookup")
        };
        assert!(options.repositories.is_empty());
    }

    #[test]
    fn settings_file_and_concurrency_are_optional() {
        let words = [
            "lookup",
            "--settings",
            "/operator/settings.pkl",
            "owner/repo",
        ]
        .map(std::ffi::OsStr::new);
        let app = App::parse_from(&words).unwrap();
        let Some(Command::Lookup(options)) = app.command else {
            panic!("lookup")
        };
        assert_eq!(options.repositories, ["owner/repo"]);
        assert_eq!(
            options.scan.settings.as_deref(),
            Some(std::path::Path::new("/operator/settings.pkl"))
        );
        assert_eq!(options.scan.concurrency, None);
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
        assert_eq!(
            apply.update_type,
            [
                UpdateType(patchula_words::Update::Pin),
                UpdateType(patchula_words::Update::Digest)
            ]
        );
        assert_eq!(apply.manager, ["github-actions"]);
        assert_eq!(apply.dependency, ["glob:actions/*"]);
        assert!(apply.check && !apply.dry_run);
    }

    #[test]
    fn remote_subcommands_accept_server_before_or_after_and_reject_local_flags() {
        fn words<'a>(args: &[&'a str]) -> Vec<&'a std::ffi::OsStr> {
            args.iter().map(|arg| std::ffi::OsStr::new(*arg)).collect()
        }
        let app = App::parse_from(&words(&[
            "remote",
            "--server",
            "https://x.dev",
            "tui",
            "--once",
        ]))
        .unwrap();
        let Some(Command::Remote(remote)) = app.command else {
            panic!("remote")
        };
        assert_eq!(remote.server.as_deref(), Some("https://x.dev"));
        let RemoteCommand::Tui(tui) = remote.command else {
            panic!("remote tui")
        };
        assert!(tui.once && tui.repositories.is_empty());

        let app = App::parse_from(&words(&[
            "remote",
            "run",
            "--dry-run",
            "--server",
            "https://x.dev",
            "acme/app",
        ]))
        .unwrap();
        let Some(Command::Remote(remote)) = app.command else {
            panic!("remote")
        };
        assert_eq!(remote.server.as_deref(), Some("https://x.dev"));
        let RemoteCommand::Run(run) = remote.command else {
            panic!("remote run")
        };
        assert!(run.dry_run && run.repositories == ["acme/app"]);

        for rejected in [
            &["lookup", "--remote"][..],
            &["lookup", "--here"],
            &["lookup", "--no-remote"],
            &["lookup", "--local"],
            &["lookup", "--server", "https://x.dev"],
            &["run", "--remote"],
            &["run", "--here"],
            &["run", "--default-config"],
            &["tui", "--remote"],
            &["tui", "--here"],
            &["apply", "--remote"],
            &["auth", "login"],
            &["remote", "lookup", "--config", "x.pkl", "acme/app"],
            &["remote", "lookup", "--config-dir", "policy", "acme/app"],
            &["remote", "lookup", "--all-repos"],
            &["remote", "lookup", "--cache", "dir"],
            &["remote", "lookup", "--settings", "s.pkl"],
            &["remote", "lookup", "--concurrency", "4"],
            &["remote", "lookup", "--default-config"],
            &["remote", "run", "--allow-lockfile-pending", "acme/app"],
            &["remote", "run", "--dir", "."],
        ] {
            assert!(App::parse_from(&words(rejected)).is_err(), "{rejected:?}");
        }
    }
}
