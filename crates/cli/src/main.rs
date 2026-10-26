//! Command-line entry point for patchula.
#![forbid(unsafe_code)]

mod apply;
mod cli;
mod copier;
mod output;
mod palette;
mod progress;
mod remote;
mod server;
mod tui;

use cli::{App, AuthCommand, Command, ConfigCommand, ToolsCommand};
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

/// Environment variables the CLI reads. Their meanings are in
/// docs/src/content/docs/reference/environment.md.
const TOKEN_VARS: [&str; 3] = ["PATCHULA_TOKEN", "GITHUB_TOKEN", "GH_TOKEN"];
/// The GitHub CLI host whose token is the last fallback; the Count only talks
/// to github.com, whatever host `gh` defaults to.
const GH_HOST: &str = "github.com";
const GH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const TOKEN_HELP: &str = "lookup/run requires PATCHULA_TOKEN, GITHUB_TOKEN or GH_TOKEN, or a GitHub CLI signed in with `gh auth login`";
const LOG_VAR: &str = "PATCHULA_LOG";
/// A lookup or apply finished, but some dependencies' lookups failed.
const LOOKUP_FAILED: u8 = 4;
const RECHECK_VAR: &str = "PATCHULA_RECHECK";
const DOCKERHUB_USERNAME_VAR: &str = "PATCHULA_DOCKERHUB_USERNAME";
const DOCKERHUB_TOKEN_VAR: &str = "PATCHULA_DOCKERHUB_TOKEN";
const MISE_DATA_VAR: &str = "MISE_DATA_DIR";
const XDG_DATA_VAR: &str = "XDG_DATA_HOME";
const HOME_VAR: &str = "HOME";
const XDG_CACHE_VAR: &str = "XDG_CACHE_HOME";

// Only the token and transport are replaced in offline CLI tests. Dispatch,
// configuration, disk-cache opening and engine construction stay on this path.
#[derive(Default)]
struct Services {
    #[cfg(test)]
    token: Option<String>,
    #[cfg(test)]
    transport: Option<std::sync::Arc<dyn patchula_http::Transport>>,
    #[cfg(test)]
    config_dir: Option<std::path::PathBuf>,
    /// Answers the server prompt; tests are interactive only when it is set.
    #[cfg(test)]
    answer: Option<bool>,
    /// Stands in for PATCHULA_SERVER.
    #[cfg(test)]
    server: Option<String>,
    /// The key pressed at a one-key question; tests are interactive when it is set.
    #[cfg(test)]
    key: Option<char>,
    /// Stands in for the current directory.
    #[cfg(test)]
    dir: Option<std::path::PathBuf>,
}
impl Services {
    fn current_dir(&self) -> io::Result<std::path::PathBuf> {
        #[cfg(test)]
        if let Some(dir) = &self.dir {
            return Ok(dir.clone());
        }
        std::env::current_dir()
    }
    async fn token(&self) -> Option<String> {
        #[cfg(test)]
        {
            self.token.clone()
        }
        #[cfg(not(test))]
        {
            resolve_token(
                |name| std::env::var(name).ok(),
                &patchula_process::SystemExecutor,
            )
            .await
        }
    }
    fn transport(
        &self,
    ) -> Result<std::sync::Arc<dyn patchula_http::Transport>, patchula_http::Error> {
        #[cfg(test)]
        if let Some(transport) = &self.transport {
            return Ok(transport.clone());
        }
        Ok(std::sync::Arc::new(patchula_http::Network::new()?))
    }
    /// Whether a person is at the terminal to answer a question.
    fn interactive(&self) -> bool {
        #[cfg(test)]
        {
            self.answer.is_some() || self.key.is_some()
        }
        #[cfg(not(test))]
        {
            io::stdin().is_terminal() && io::stderr().is_terminal()
        }
    }
    /// A yes/no question on stderr; anything but "n" or "no" is yes.
    fn ask(&self, question: &str) -> io::Result<bool> {
        eprint!("{question}");
        #[cfg(test)]
        if let Some(answer) = self.answer {
            eprintln!();
            return Ok(answer);
        }
        let mut line = String::default();
        io::stdin().read_line(&mut line)?;
        Ok(!matches!(
            line.trim().to_ascii_lowercase().as_str(),
            "n" | "no"
        ))
    }
    /// A one-key question on stderr, asked until a key `pick` accepts. Enter
    /// is `'\n'`; Esc and Ctrl-C are `'q'`.
    fn pick<T>(&self, question: &str, pick: impl Fn(char) -> Option<T>) -> io::Result<T> {
        eprint!("{question}");
        #[cfg(test)]
        if let Some(key) = self.key {
            eprintln!();
            return pick(key).ok_or_else(|| io::Error::other(format!("unexpected key {key:?}")));
        }
        use ratatui::crossterm::{
            event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
            terminal,
        };
        io::stderr().flush()?;
        terminal::enable_raw_mode()?;
        let answer = loop {
            let key = match event::read() {
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => key,
                Ok(_) => continue,
                Err(error) => break Err(error),
            };
            let key = match key.code {
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => 'q',
                KeyCode::Esc => 'q',
                KeyCode::Enter => '\n',
                KeyCode::Char(key) => key,
                _ => continue,
            };
            if let Some(answer) = pick(key) {
                if key.is_ascii_graphic() {
                    eprint!("{key}");
                }
                break Ok(answer);
            }
        };
        terminal::disable_raw_mode()?;
        eprintln!();
        answer
    }
    /// For `--remote`, which may reach a `patchula serve` on loopback without TLS.
    fn remote_transport(
        &self,
    ) -> Result<std::sync::Arc<dyn patchula_http::Transport>, patchula_http::Error> {
        #[cfg(test)]
        if let Some(transport) = &self.transport {
            return Ok(transport.clone());
        }
        Ok(std::sync::Arc::new(
            patchula_http::Network::with_loopback_http()?,
        ))
    }
}

/// The first non-empty token variable, else the token of a GitHub CLI signed in
/// to github.com. A missing, signed-out, failing or hung `gh` gives nothing,
/// and its output never reaches an error or a log.
async fn resolve_token(
    var: impl Fn(&str) -> Option<String>,
    executor: &dyn patchula_process::Executor,
) -> Option<String> {
    if let Some(token) = TOKEN_VARS
        .iter()
        .filter_map(|name| var(name))
        .find(|token| !token.is_empty())
    {
        return Some(token);
    }
    let output = executor
        .run(patchula_process::CommandSpec {
            program: "gh".into(),
            args: ["auth", "token", "--hostname", GH_HOST]
                .map(String::from)
                .into(),
            cwd: std::env::temp_dir(),
            env: std::env::vars_os().collect(),
            timeout: GH_TIMEOUT,
            output_limit: 4096,
            complete_output: true,
        })
        .await
        .ok()?;
    let token = String::from_utf8(output).ok()?.trim().to_owned();
    (!token.is_empty()).then_some(token)
}

async fn run(app: &App, services: &Services) -> Result<u8, (u8, miette::Report)> {
    let command = app
        .command
        .as_ref()
        .ok_or_else(|| (2, miette::miette!("choose a subcommand; see --help")))?;
    match command {
        Command::Apply(options) => {
            apply::command(
                app,
                options,
                services,
                &mut io::stdout(),
                &mut io::stderr(),
                &output::Style::stderr(app.color),
            )
            .await
        }
        Command::Auth(auth) => auth_command(app, auth, services).await,
        Command::Config(config) => config_command(app, config),
        Command::Lookup(options) => batch(app, options, None, services).await,
        Command::Run(options) => batch(app, &options.lookup, Some(options), services).await,
        Command::Tui(tui) => tui_command(app, tui, services).await,
        Command::Tools(tools) => tools_command(app, tools).await,
        Command::Serve(serve) => server::run(app, serve).await,
    }
}

/// `patchula auth login|logout`: manage the token for a `--remote` server.
async fn auth_command(
    app: &App,
    auth: &cli::Auth,
    services: &Services,
) -> Result<u8, (u8, miette::Report)> {
    let (AuthCommand::Login(target) | AuthCommand::Logout(target)) = &auth.command;
    let server = match &target.server {
        Some(server) => remote::parse(server)?,
        None => remote::configured(services)?.ok_or_else(|| {
            (
                2,
                miette::miette!("name the server with --server or {}", remote::SERVER_VAR),
            )
        })?,
    };
    let (done, message) = match auth.command {
        AuthCommand::Login(_) => {
            remote::sign_in(services, &server).await?;
            (true, format!("Signed in to {server}"))
        }
        AuthCommand::Logout(_) => {
            let removed = remote::forget(services, &server)?;
            (
                removed,
                if removed {
                    format!("Signed out of {server}")
                } else {
                    format!("Not signed in to {server}")
                },
            )
        }
    };
    let result = if app.json {
        serde_json::json!({"schemaVersion":1,"server":server,"changed":done}).to_string()
    } else {
        message
    };
    writeln!(io::stdout().lock(), "{result}")
        .map(|_| 0)
        .map_err(|e| (1, miette::miette!("{e}")))
}

/// `patchula config ...`: print or validate configuration and server settings.
fn config_command(app: &App, config: &cli::Config) -> Result<u8, (u8, miette::Report)> {
    match &config.command {
        ConfigCommand::ServerSchema => {
            write!(io::stdout().lock(), "{}", patchula_server::settings::SCHEMA)
                .map(|_| 0)
                .map_err(|e| (1, miette::miette!("{e}")))
        }
        ConfigCommand::CheckServer(check) => {
            patchula_server::settings::Settings::load(&check.path).map_err(|e| (2, e))?;
            writeln!(
                io::stdout().lock(),
                "{}",
                if app.json {
                    serde_json::json!({"schemaVersion":1,"valid":true,"path":check.path})
                        .to_string()
                } else {
                    format!("Valid server settings: {}", check.path.display())
                }
            )
            .map(|_| 0)
            .map_err(|e| (1, miette::miette!("{e}")))
        }
        ConfigCommand::Schema => write!(io::stdout().lock(), "{}", patchula_config::SCHEMA)
            .map(|_| 0)
            .map_err(|error| (1, miette::miette!("{error}"))),
        ConfigCommand::Check(check) => {
            let config = patchula_config::load(&check.path).map_err(|error| (2, error))?;
            if app.verbose > 0 && !app.json {
                eprintln!(
                    "patchula: {} managers, {} rules, {} extractors",
                    config.managers.len(),
                    config.rules.len(),
                    config.extractors.len()
                );
            }
            let result = if app.json {
                serde_json::json!({"schemaVersion": 1, "valid": true, "path": check.path})
                    .to_string()
            } else {
                format!("Valid configuration: {}", check.path.display())
            };
            writeln!(io::stdout().lock(), "{result}")
                .map(|_| 0)
                .map_err(|error| (1, miette::miette!("{error}")))
        }
    }
}

/// `patchula tui`: browse a lookup, and in a working tree, apply chosen updates.
async fn tui_command(
    app: &App,
    tui: &cli::Tui,
    services: &Services,
) -> Result<u8, (u8, miette::Report)> {
    if !tui.once && !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
        return Err((
            2,
            miette::miette!("patchula tui needs a terminal; use --once for a plain snapshot"),
        ));
    }
    let (report, read) = report(app, &tui.lookup, None, services).await?;
    let origin = match read {
        Read::Checkout(checkout) => tui::Origin::Local(Box::new(
            tui::local(&checkout.0, checkout.1).map_err(|e| (2, miette::miette!("{e}")))?,
        )),
        Read::Server => tui::Origin::Server,
        Read::GitHub => tui::Origin::GitHub,
    };
    let model = tui::Model::new(&report, origin);
    if tui.once {
        write!(io::stdout().lock(), "{}", tui::snapshot(&model))
            .map(|_| 0)
            .map_err(|e| (1, miette::miette!("{e}")))
    } else {
        tui::interact(model)
            .map(|_| 0)
            .map_err(|e| (1, miette::miette!("terminal error: {e}")))
    }
}

/// `patchula tools install|prune`: manage the lockfile tool store.
async fn tools_command(app: &App, tools: &cli::Tools) -> Result<u8, (u8, miette::Report)> {
    let (store, days) = match &tools.command {
        ToolsCommand::Install(store) => (store, None),
        ToolsCommand::Prune(prune) => (&prune.store, Some(prune.days)),
    };
    if days.is_some_and(|d| d < 1) {
        return Err((2, miette::miette!("--days must be at least 1")));
    }
    let dir = cache_dir(store.cache.as_deref()).map_err(|e| (1, e))?;
    let mut runner = patchula_update::lockfiles::Runner::new(dir.join("worktrees"), "");
    if let Some(mise_data) = user_mise_data() {
        runner = runner.with_seed(mise_data);
    }
    let (action, specs) = match days {
        None => ("installed", runner.prewarm().await),
        Some(days) => ("pruned", runner.prune(days).await),
    };
    let specs = specs.map_err(|e| (1, miette::miette!("{e}")))?;
    let result = if app.json {
        serde_json::json!({"schemaVersion":1,"store":runner.tools(),action:specs}).to_string()
    } else {
        format!(
            "{} {} tool versions in {}",
            action,
            specs.len(),
            runner.tools().display()
        )
    };
    writeln!(io::stdout().lock(), "{result}")
        .map(|_| 0)
        .map_err(|error| (1, miette::miette!("{error}")))
}

#[tokio::main]
async fn main() -> ExitCode {
    // The embedded entry is the derive's one way to pass a help palette; it
    // returns help, version and parse failures for us to print.
    let argv: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let app = match App::embedded_outcome_paletted(&argv, cli::PALETTE) {
        usage_rs::embedded::Outcome::Parsed(app) => app,
        usage_rs::embedded::Outcome::Exit(exit) => {
            let text = palette::help(&exit.text, palette::depth());
            let _ = if exit.stderr {
                io::stderr().lock().write_all(text.as_bytes())
            } else {
                io::stdout().lock().write_all(text.as_bytes())
            };
            return ExitCode::from(u8::try_from(exit.code).unwrap_or(1));
        }
    };
    let serving = matches!(app.command, Some(Command::Serve(_)));
    // Only our explicitly sanitized events reach stderr. Dependency HTTP debug
    // logs can contain headers even when the user requests maximum verbosity.
    use tracing_subscriber::{filter::Targets, prelude::*};
    let level = std::env::var(LOG_VAR)
        .ok()
        .and_then(|value| value.parse::<tracing::level_filters::LevelFilter>().ok())
        .unwrap_or(if app.verbose >= 2 {
            tracing::level_filters::LevelFilter::DEBUG
        } else if serving {
            tracing::level_filters::LevelFilter::INFO
        } else {
            tracing::level_filters::LevelFilter::WARN
        });
    let layer = if serving {
        tracing_subscriber::fmt::layer()
            .json()
            .with_writer(io::stderr)
            .with_ansi(false)
            .boxed()
    } else {
        tracing_subscriber::fmt::layer()
            .with_writer(io::stderr)
            .with_ansi(false)
            .boxed()
    };
    let _ = tracing_subscriber::registry()
        .with(layer.with_filter(Targets::new().with_target("patchula", level)))
        .try_init();
    let color = output::colored(app.color, io::stderr().is_terminal());
    console::set_colors_enabled_stderr(color);
    let _ = miette::set_hook(Box::new(move |_| {
        Box::new(
            miette::MietteHandlerOpts::new()
                .color(color)
                .terminal_links(false)
                .build(),
        )
    }));
    match run(&app, &Services::default()).await {
        Ok(code) => ExitCode::from(code),
        Err((code, error)) => {
            if serving {
                let diagnostic = if error.source_code().is_some() {
                    format!("{error:?}")
                } else {
                    format!("{error:#}")
                };
                tracing::error!(target: "patchula_server", error = %diagnostic, exit_code = code, "server stopped");
            } else if app.json {
                println!(
                    "{}",
                    serde_json::json!({"schemaVersion": 1, "error": format!("{error:#}"), "exitCode": code})
                );
            } else {
                eprintln!("{error:?}");
                if app.verbose > 0 {
                    eprintln!("patchula: exiting with status {code}");
                }
            }
            ExitCode::from(code)
        }
    }
}

async fn batch(
    app: &App,
    options: &cli::Lookup,
    run: Option<&cli::Run>,
    services: &Services,
) -> Result<u8, (u8, miette::Report)> {
    use patchula_report::Status;
    let (report, _) = report(app, options, run, services).await?;
    let write = |text: &str| -> Result<(), (u8, miette::Report)> {
        write!(io::stdout().lock(), "{text}").map_err(|e| (1, miette::miette!("{e}")))
    };
    if app.json {
        write(&format!(
            "{}\n",
            serde_json::to_string(&report)
                .map_err(|e| (1, miette::miette!("cannot write report: {e}")))?
        ))?;
    } else {
        write(&output::report(&report, &output::Style::stdout(app.color)))?;
    }
    if app.verbose > 0
        && let Some(stats) = &report.stats
    {
        eprintln!(
            "patchula: {} requests, {} 304s, {} cache hits, {} extraction hits, {} unique lookups",
            stats.requests,
            stats.not_modified,
            stats.cache_hits,
            stats.extraction_hits,
            stats.lookups
        );
        eprintln!(
            "patchula timings: {}",
            stats
                .phase_micros
                .iter()
                .map(|(phase, micros)| format!("{phase}={:.2}ms", *micros as f64 / 1000.0))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let errors = report
        .repositories
        .iter()
        .filter(|repo| repo.status == Status::Error)
        .count();
    let failures: usize = report
        .repositories
        .iter()
        .map(|repo| repo.lookup_failures.len())
        .sum();
    Ok(if errors > 0 {
        1
    } else if failures > 0 && run.is_none() {
        LOOKUP_FAILED
    } else {
        0
    })
}

/// A lookup or run on a server. Only the repositories and the mode travel;
/// the server applies each repository's own `patchula.pkl`.
async fn remote_report(
    services: &Services,
    server: &str,
    repositories: Vec<String>,
    run: Option<&cli::Run>,
) -> Result<patchula_report::Report, (u8, miette::Report)> {
    let mode = match run {
        None => remote::Mode::Lookup,
        Some(run) if run.dry_run => remote::Mode::DryRun,
        Some(_) => remote::Mode::Run,
    };
    remote::report(services, server, repositories, mode).await
}

/// HTTP settings from the lookup flags.
fn http_options(options: &cli::Scan) -> Result<patchula_http::Options, (u8, miette::Report)> {
    let mut http_options = patchula_http::Options {
        registry_freshness_secs: options.registry_freshness_secs,
        go_freshness_secs: options.go_freshness_secs,
        ..Default::default()
    };
    for setting in &options.host_concurrency {
        let (host, limit) = setting
            .split_once('=')
            .and_then(|(host, limit)| limit.parse::<usize>().ok().map(|limit| (host, limit)))
            .ok_or_else(|| {
                (
                    2,
                    miette::miette!("host-concurrency must be HOST=LIMIT (1..100)"),
                )
            })?;
        http_options.host_concurrency.insert(host.to_owned(), limit);
    }
    http_options.validate().map_err(|_| {
        (
            2,
            miette::miette!("host-concurrency requires a lowercase hostname and limit 1..100"),
        )
    })?;
    Ok(http_options)
}

/// The checkbox wording: `--recheck`, then `PATCHULA_RECHECK` for runs.
fn recheck(app: &App, run: bool) -> Result<patchula_engine::Recheck, (u8, miette::Report)> {
    Ok(match app.recheck {
        Some(cli::Recheck::Now) => patchula_engine::Recheck::Now,
        Some(cli::Recheck::NextRun) => patchula_engine::Recheck::NextRun,
        None if run => match std::env::var(RECHECK_VAR).as_deref() {
            Ok("now") => patchula_engine::Recheck::Now,
            Ok("next-run") | Err(std::env::VarError::NotPresent) => {
                patchula_engine::Recheck::NextRun
            }
            _ => {
                return Err((
                    2,
                    miette::miette!("PATCHULA_RECHECK must be now or next-run"),
                ));
            }
        },
        None => patchula_engine::Recheck::NextRun,
    })
}

/// Docker Hub credentials, when both variables are set.
fn dockerhub_credentials() -> Result<Option<(String, String)>, (u8, miette::Report)> {
    match (
        std::env::var(DOCKERHUB_USERNAME_VAR).ok(),
        std::env::var(DOCKERHUB_TOKEN_VAR).ok(),
    ) {
        (None, None) => Ok(None),
        (Some(username), Some(token)) if !username.is_empty() && !token.is_empty() => {
            Ok(Some((username, token)))
        }
        _ => Err((
            2,
            miette::miette!(
                "set both PATCHULA_DOCKERHUB_USERNAME and PATCHULA_DOCKERHUB_TOKEN to nonempty values"
            ),
        )),
    }
}

/// The local cache directory: `--cache`, or `patchula/` in the user cache directory.
fn cache_dir(flag: Option<&std::path::Path>) -> Result<std::path::PathBuf, miette::Report> {
    let dir = match flag {
        Some(dir) => std::path::absolute(dir)
            .map_err(|e| miette::miette!("cannot resolve --cache {}: {e}", dir.display()))?,
        None => user_cache_dir(|name| std::env::var_os(name))
            .ok_or_else(|| {
                miette::miette!(
                    "cannot find a cache directory; set HOME or XDG_CACHE_HOME, or pass --cache DIR"
                )
            })?
            .join("patchula"),
    };
    if dir.is_file() {
        return Err(miette::miette!(
            "--cache takes a directory, but {} is a file",
            dir.display()
        ));
    }
    Ok(dir)
}

/// The platform's user cache directory: `~/Library/Caches` on macOS, else
/// `XDG_CACHE_HOME` when it is absolute, or `~/.cache`.
fn user_cache_dir(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let home = var(HOME_VAR)
        .filter(|home| !home.is_empty())
        .map(PathBuf::from);
    if cfg!(target_os = "macos") {
        return home.map(|home| home.join("Library/Caches"));
    }
    var(XDG_CACHE_VAR)
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| home.map(|home| home.join(".cache")))
}

/// The invoking user's mise data directory, when it has installs. Local runs
/// borrow matching tool versions from it instead of downloading them.
fn user_mise_data() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    std::env::var_os(MISE_DATA_VAR)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os(XDG_DATA_VAR).map(|dir| PathBuf::from(dir).join("mise")))
        .or_else(|| {
            std::env::var_os(HOME_VAR).map(|home| PathBuf::from(home).join(".local/share/mise"))
        })
        .filter(|dir| dir.join("installs").is_dir())
}

/// The repositories a batch names: the ones given, or every one the token can
/// reach with `--all-repos` (an empty list, which the engine discovers).
fn repositories(options: &cli::Lookup) -> Result<Vec<String>, miette::Report> {
    if options.all_repos {
        if !options.repositories.is_empty() {
            return Err(miette::miette!(
                "--all-repos cannot be combined with named repositories"
            ));
        }
        return Ok(Vec::new());
    }
    Ok(options.repositories.clone())
}

/// Where a lookup, run or tui batch reads from.
enum Source {
    /// The working tree in the current directory, uncommitted changes included,
    /// with the config to apply when it is not the tree's own `patchula.pkl`.
    Local(std::path::PathBuf, Option<Box<patchula_config::Config>>),
    /// Repositories read through the GitHub API.
    GitHub(Vec<String>),
    /// A Count Patchula server.
    Remote {
        server: String,
        repositories: Vec<String>,
    },
}

/// Named repositories and `--all-repos` read GitHub. With neither, the checkout
/// here runs on the server chosen for it, or reads its working tree. A real run
/// opens pull requests from what GitHub has, so it reads GitHub instead, once
/// the working tree matches what is pushed.
async fn source(
    app: &App,
    options: &cli::Lookup,
    run: Option<&cli::Run>,
    services: &Services,
) -> Result<Source, (u8, miette::Report)> {
    let usage = |message: String| (2, miette::miette!("{message}"));
    let named = !options.repositories.is_empty() || options.all_repos;
    let writes = run.is_some_and(|run| !run.dry_run);
    let local_only = [
        (options.scan.config.is_some(), "--config"),
        (options.config_dir.is_some(), "--config-dir"),
        (options.scan.default_config, "--default-config"),
        (options.all_repos, "--all-repos"),
        (
            run.is_some_and(|r| r.allow_lockfile_pending),
            "--allow-lockfile-pending",
        ),
        // The server's operator setting decides the checkbox wording.
        (app.recheck.is_some(), "--recheck"),
    ];
    let local_flag = local_only
        .iter()
        .find(|(set, _)| *set)
        .map(|(_, flag)| *flag);
    if options.remote {
        if let Some(flag) = local_flag {
            return Err(usage(format!(
                "{flag} only works on this machine, not with --remote"
            )));
        }
        if named {
            let server = remote::configured(services)?.ok_or_else(|| {
                usage(format!(
                    "--remote with named repositories needs {} set to the server's URL",
                    remote::SERVER_VAR
                ))
            })?;
            return Ok(Source::Remote {
                server,
                repositories: options.repositories.clone(),
            });
        }
        let repository = current_repository().map_err(|e| (2, e))?;
        let server = match remote::configured(services)? {
            Some(server) => server,
            None => remote::find(services, &repository).await?.ok_or_else(|| {
                usage(format!(
                    "--remote found no Count server for {repository}; set {} to the server's URL",
                    remote::SERVER_VAR
                ))
            })?,
        };
        if writes {
            pushed()?;
        }
        return Ok(Source::Remote {
            server,
            repositories: vec![repository],
        });
    }
    if options.scan.default_config {
        if options.scan.config.is_some() || options.config_dir.is_some() {
            return Err(usage(
                "--default-config cannot be combined with --config or --config-dir".into(),
            ));
        }
        if named || writes {
            return Err(usage(
                "--default-config is for the working tree; a repository on GitHub invites the Count with its own patchula.pkl".into(),
            ));
        }
    }
    if named {
        return repositories(options)
            .map(Source::GitHub)
            .map_err(|e| (2, e));
    }
    if !options.here
        && local_flag.is_none()
        && let Ok(repository) = current_repository()
        && let Some(server) = remote::choose(services, &repository).await?
    {
        if writes {
            pushed()?;
        }
        return Ok(Source::Remote {
            server,
            repositories: vec![repository],
        });
    }
    if writes {
        let repository = current_repository().map_err(|e| (2, e))?;
        pushed()?;
        return Ok(Source::GitHub(vec![repository]));
    }
    if options.config_dir.is_some() {
        return Err(usage(
            "--config-dir picks config per repository; name repositories, or use --config for the working tree".into(),
        ));
    }
    let dir = services
        .current_dir()
        .map_err(|e| (2, miette::miette!("cannot read the current directory: {e}")))?;
    let config = match &options.scan.config {
        Some(path) => Some(patchula_config::load(path).map_err(|e| (2, e))?),
        None => missing_config(
            &dir,
            options.scan.default_config,
            services,
            &output::Style::stderr(app.color),
        )?,
    };
    Ok(Source::Local(dir, config.map(Box::new)))
}

/// What to do when the working tree has no `patchula.pkl`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Missing {
    /// Use the Count's defaults for this run only.
    Defaults,
    /// Write the starter `patchula.pkl` and read it.
    Write,
    Quit,
}

impl Missing {
    /// The answer a key gives; Enter takes the defaults.
    fn from_key(key: char) -> Option<Self> {
        match key.to_ascii_lowercase() {
            'd' | '\n' => Some(Self::Defaults),
            'w' => Some(Self::Write),
            'q' | 'n' => Some(Self::Quit),
            _ => None,
        }
    }
}

/// The config for a working tree with no `--config`: none when it has a
/// `patchula.pkl` for the engine to read, else the defaults if
/// `--default-config` or the person at the terminal asks for them. Asked, the
/// person may instead have the starter file written. Without a terminal, it
/// says how to choose.
fn missing_config(
    dir: &std::path::Path,
    default_config: bool,
    services: &Services,
    style: &output::Style,
) -> Result<Option<patchula_config::Config>, (u8, miette::Report)> {
    let found = patchula_config::discover(dir)
        .map_err(|e| (2, miette::miette!("cannot read {}: {e}", dir.display())))?;
    if found.is_some() {
        return Ok(None);
    }
    let defaults = || patchula_config::defaults().map(Some).map_err(|e| (2, e));
    if default_config {
        return defaults();
    }
    let path = dir.join(patchula_config::CONFIG_PATH);
    if !services.interactive() {
        return Err((
            2,
            miette::miette!(
                help = format!(
                    "pass --default-config to use the Count's defaults for this run, or create {} containing `amends \"PatchulaConfig.pkl\"` to invite him for good",
                    path.display()
                ),
                "no repository named, and {} has no patchula.pkl to read",
                dir.display()
            ),
        ));
    }
    let shown = std::env::var_os(HOME_VAR)
        .and_then(|home| dir.strip_prefix(home).ok())
        .map_or_else(
            || dir.display().to_string(),
            |rest| format!("~/{}", rest.display()),
        );
    let choice = |key: &str, text: &str| format!("    {}  {text}\n", style.bold(key));
    let mut question = String::new();
    style.wrap(
        &mut question,
        0,
        &format!(
            "{}The Count finds no patchula.pkl in {shown}, so he has not been invited in.",
            style.bat()
        ),
        output::Paint::Bold,
    );
    question.push_str(&format!(
        "{}\n{}{}{}  {} ",
        style.dim("  He waits at the door. Press a key:"),
        choice(
            "d",
            &format!("use his defaults for this run {}", style.dim("(Enter)"))
        ),
        choice("w", "write a starter patchula.pkl and come in"),
        choice("q", "quit"),
        style.dim("›"),
    ));
    let answer = services
        .pick(&question, Missing::from_key)
        .map_err(|e| (1, miette::miette!("cannot read the answer: {e}")))?;
    match answer {
        Missing::Defaults => {
            eprintln!(
                "{}",
                style.dim("  Using the Count's defaults for this run; --default-config skips this question.\n")
            );
            defaults()
        }
        Missing::Write => {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .and_then(|mut file| file.write_all(patchula_config::STARTER.as_bytes()))
                .map_err(|e| (1, miette::miette!("cannot write {}: {e}", path.display())))?;
            eprintln!(
                "{}",
                style.dim(&format!(
                    "  Wrote {}; the Count is invited, and the file is yours to edit.\n",
                    path.display()
                ))
            );
            Ok(None)
        }
        Missing::Quit => Err((
            2,
            miette::miette!(
                help = "pass --default-config to use the Count's defaults, or add a patchula.pkl",
                "the Count stays outside: {} has no patchula.pkl",
                dir.display()
            ),
        )),
    }
}

/// A real run with no repository opens pull requests from what GitHub has, so
/// the working tree must be clean and at the remote's default branch.
fn pushed() -> Result<(), (u8, miette::Report)> {
    let refuse = |message: String| {
        (
            2,
            miette::miette!(
                help = "use --dry-run to plan from the working tree, or commit and push to the default branch first",
                "{message}"
            ),
        )
    };
    let status = git(&["status", "--porcelain"])
        .ok_or_else(|| refuse("cannot read this checkout's Git status".into()))?;
    if !status.is_empty() {
        return Err(refuse(
            "a run with no repository opens pull requests from what is on GitHub, and this working tree has uncommitted changes".into(),
        ));
    }
    let remote = git_remote().map_err(|e| (2, e))?;
    let default = git(&["symbolic-ref", "--short", &format!("refs/remotes/{remote}/HEAD")])
        .ok_or_else(|| {
            refuse(format!(
                "cannot tell the default branch of {remote}; `git remote set-head {remote} --auto` records it"
            ))
        })?;
    let head = git(&["rev-parse", "HEAD"]);
    if head.is_none() || head != git(&["rev-parse", &default]) {
        return Err(refuse(format!(
            "a run with no repository opens pull requests from {default}, and HEAD is not at {default}"
        )));
    }
    Ok(())
}

/// Runs git in the current directory: its trimmed stdout, or None on failure.
fn git(args: &[&str]) -> Option<String> {
    std::process::Command::new("git")
        .args(args)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

const CHECKOUT_HELP: &str =
    "name a repository as owner/repo, or pass --all-repos for every repository the token can reach";

/// The current checkout's remote: `origin`, or the only remote when there is
/// no `origin`.
fn git_remote() -> Result<String, miette::Report> {
    let Some(remotes) = git(&["remote"]) else {
        return Err(miette::miette!(
            help = CHECKOUT_HELP,
            "no repository named, and the current directory is not a Git checkout"
        ));
    };
    let remotes: Vec<&str> = remotes.lines().collect();
    match remotes.as_slice() {
        _ if remotes.contains(&"origin") => Ok("origin".into()),
        [only] => Ok((*only).to_owned()),
        [] => Err(miette::miette!(
            help = CHECKOUT_HELP,
            "no repository named, and this checkout has no Git remote"
        )),
        _ => Err(miette::miette!(
            help = CHECKOUT_HELP,
            "no repository named, and this checkout has several remotes but no origin"
        )),
    }
}

/// The `owner/name` of the current checkout's GitHub remote.
fn current_repository() -> Result<String, miette::Report> {
    let remote = git_remote()?;
    git(&["remote", "get-url", &remote])
        .as_deref()
        .and_then(github_repository)
        .ok_or_else(|| {
            miette::miette!(
                help = CHECKOUT_HELP,
                "no repository named, and the {remote} remote is not a github.com repository"
            )
        })
}

/// `owner/name` from a github.com remote URL in HTTPS, SSH, Git or scp-like form.
fn github_repository(url: &str) -> Option<String> {
    let host_of = |authority: &str| {
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        host.split_once(':')
            .map_or(host, |(host, _)| host)
            .to_ascii_lowercase()
    };
    let (host, path) = match url.split_once("://") {
        Some((scheme, rest)) => {
            if !matches!(
                scheme,
                "https" | "http" | "ssh" | "git" | "git+ssh" | "ssh+git"
            ) {
                return None;
            }
            let (authority, path) = rest.split_once('/')?;
            (host_of(authority), path)
        }
        None => {
            let (authority, path) = url.split_once(':')?;
            (host_of(authority), path)
        }
    };
    if host != "github.com" && host != "ssh.github.com" {
        return None;
    }
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    (!owner.is_empty() && !name.is_empty() && !name.contains('/'))
        .then(|| format!("{owner}/{name}"))
}

#[cfg(test)]
mod tests;

/// The working tree a batch read, and the config it applied when that was not
/// the tree's own `patchula.pkl`.
type Checkout = (std::path::PathBuf, Option<patchula_config::Config>);

/// What a lookup read: the working tree here, GitHub, or a server.
enum Read {
    Checkout(Box<Checkout>),
    GitHub,
    Server,
}

/// Build the engine from lookup flags and run one lookup or run batch, with the
/// working tree it read, if it read one.
async fn report(
    app: &App,
    options: &cli::Lookup,
    run: Option<&cli::Run>,
    services: &Services,
) -> Result<(patchula_report::Report, Read), (u8, miette::Report)> {
    use patchula_engine::{Engine, Options};
    let scan = &options.scan;
    if scan.concurrency == 0 || scan.concurrency > 100 {
        return Err((2, miette::miette!("concurrency must be 1..100")));
    }
    let (repositories, local, config) = match source(app, options, run, services).await? {
        Source::Remote {
            server,
            repositories,
        } => {
            let report = remote_report(services, &server, repositories, run).await?;
            return Ok((report, Read::Server));
        }
        Source::GitHub(repositories) => {
            let config = options
                .scan
                .config
                .as_ref()
                .map(|path| patchula_config::load(path))
                .transpose()
                .map_err(|e| (2, e))?;
            (repositories, None, config)
        }
        Source::Local(dir, config) => (Vec::new(), Some(dir), config.map(|config| *config)),
    };
    let http_options = http_options(scan)?;
    let recheck = recheck(app, run.is_some())?;
    let spinner = progress::Spinner::start(app.color);
    let token = services
        .token()
        .await
        .ok_or_else(|| (1, miette::miette!("{TOKEN_HELP}")))?;
    let dir = cache_dir(scan.cache.as_deref()).map_err(|e| (1, e))?;
    let cache = patchula_cache::Cache::open_dir(&dir, Default::default())
        .await
        .map_err(|e| {
            (
                1,
                miette::miette!("cannot open the cache in {}: {e}", dir.display()),
            )
        })?;
    let transport = services
        .transport()
        .map_err(|e| (1, miette::miette!("{e}")))?;
    let mut engine =
        Engine::with_http_options(&token, cache, scan.concurrency, transport, http_options)
            .map_err(|e| (1, miette::miette!("{e}")))?
            .with_recheck(recheck)
            .with_advisories();
    if let Some(spinner) = &spinner {
        engine = engine.with_progress(spinner.clone());
    }
    if let Some(mise_data) = user_mise_data() {
        engine = engine.with_tool_seed(mise_data);
    }
    if let Some((username, token)) = dockerhub_credentials()? {
        engine = engine.with_dockerhub_credentials(username, token);
    }
    let options = Options {
        repositories,
        config: config.clone(),
        config_dir: options.config_dir.clone(),
        local: local.clone(),
    };
    let report = if let Some(run) = run {
        engine
            .run(options, run.dry_run, run.allow_lockfile_pending)
            .await
    } else {
        engine.lookup(options).await
    }
    .map_err(|e| {
        let code = if matches!(e, patchula_engine::Error::Configuration(_)) {
            2
        } else {
            1
        };
        (code, miette::miette!("{e}"))
    })?;
    Ok((
        report,
        match local {
            Some(dir) => Read::Checkout(Box::new((dir, config))),
            None => Read::GitHub,
        },
    ))
}
