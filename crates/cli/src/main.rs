//! Command-line entry point for patchula.
#![forbid(unsafe_code)]

mod cli;
mod remote;
mod server;
mod tui;

use cli::{App, AuthCommand, Command, ConfigCommand, ToolsCommand};
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

/// Environment variables the CLI reads. Their meanings are in
/// docs/src/content/docs/reference/environment.md.
const TOKEN_VARS: [&str; 3] = ["PATCHULA_TOKEN", "GITHUB_TOKEN", "GH_TOKEN"];
const LOG_VAR: &str = "PATCHULA_LOG";
const RECHECK_VAR: &str = "PATCHULA_RECHECK";
const DOCKERHUB_USERNAME_VAR: &str = "PATCHULA_DOCKERHUB_USERNAME";
const DOCKERHUB_TOKEN_VAR: &str = "PATCHULA_DOCKERHUB_TOKEN";
const MISE_DATA_VAR: &str = "MISE_DATA_DIR";
const XDG_DATA_VAR: &str = "XDG_DATA_HOME";
const HOME_VAR: &str = "HOME";

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
}
impl Services {
    fn token(&self) -> Result<String, std::env::VarError> {
        #[cfg(test)]
        if let Some(token) = &self.token {
            return Ok(token.clone());
        }
        std::env::var(TOKEN_VARS[0])
            .or_else(|_| std::env::var(TOKEN_VARS[1]))
            .or_else(|_| std::env::var(TOKEN_VARS[2]))
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
            self.answer.is_some()
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
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        Ok(!matches!(
            line.trim().to_ascii_lowercase().as_str(),
            "n" | "no"
        ))
    }
    /// For `--server`, which may be a `patchula serve` on loopback without TLS.
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

async fn run(app: &App, services: &Services) -> Result<u8, (u8, miette::Report)> {
    let command = app
        .command
        .as_ref()
        .ok_or_else(|| (2, miette::miette!("choose a subcommand; see --help")))?;
    match command {
        Command::Auth(auth) => auth_command(app, auth, services).await,
        Command::Config(config) => config_command(app, config),
        Command::Lookup(options) => batch(app, options, None, services).await,
        Command::Run(options) => batch(app, &options.lookup, Some(options), services).await,
        Command::Tui(tui) => tui_command(app, tui, services).await,
        Command::Tools(tools) => tools_command(app, tools).await,
        Command::Serve(serve) => server::run(app, serve).await,
    }
}

/// `patchula auth login|logout`: manage the token for a `--server`.
async fn auth_command(
    app: &App,
    auth: &cli::Auth,
    services: &Services,
) -> Result<u8, (u8, miette::Report)> {
    let (AuthCommand::Login(target) | AuthCommand::Logout(target)) = &auth.command;
    let server = remote::server(target.server.as_deref())?.ok_or_else(|| {
        (
            2,
            miette::miette!("name the server with --server or {}", remote::SERVER_VAR),
        )
    })?;
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

/// `patchula tui`: browse a lookup, and with `--local`, apply chosen updates.
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
    let report = report(app, &tui.lookup, None, services).await?;
    let local = match &tui.lookup.local {
        Some(dir) => {
            let config = tui
                .lookup
                .config
                .as_ref()
                .map(|path| patchula_config::load(path))
                .transpose()
                .map_err(|e| (2, e))?;
            Some(tui::local(dir, config).map_err(|e| (2, miette::miette!("{e}")))?)
        }
        None => None,
    };
    let model = tui::Model::new(&report, local);
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
    let path = if store.cache.is_absolute() {
        store.cache.clone()
    } else {
        std::env::current_dir()
            .unwrap_or_default()
            .join(&store.cache)
    };
    let mut runner = patchula_update::lockfiles::Runner::new(path.with_extension("worktrees"), "");
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
    let app = App::parse();
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
    let color = app.color == cli::Color::Always
        || (app.color == cli::Color::Auto && io::stderr().is_terminal());
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
    let report = report(app, options, run, services).await?;
    if app.json {
        serde_json::to_writer(io::stdout().lock(), &report)
            .map_err(|e| (1, miette::miette!("cannot write report: {e}")))?;
        println!();
    } else if let Some(actions) = &report.actions {
        let mut out = io::stdout().lock();
        for branch in &actions.branches {
            writeln!(
                out,
                "{} {}: {:?}{}",
                branch.repository,
                branch.branch,
                branch.status,
                if branch.lockfile_pending {
                    " [lockfile pending]"
                } else {
                    ""
                }
            )
            .map_err(|e| (1, miette::miette!("{e}")))?;
            if let Some(reason) = &branch.reason {
                writeln!(out, "  {reason}").map_err(|e| (1, miette::miette!("{e}")))?;
            }
            for file in &branch.files {
                writeln!(out, "  {} (extracted blob {})", file.path, file.blob_sha)
                    .map_err(|e| (1, miette::miette!("{e}")))?;
                if actions.dry_run {
                    write!(out, "{}", file.diff).map_err(|e| (1, miette::miette!("{e}")))?;
                }
            }
        }
        for pr in &actions.pull_requests {
            writeln!(
                out,
                "{} {} PR: {:?}{}",
                pr.repository,
                pr.branch,
                pr.status,
                pr.url.as_ref().map(|u| format!(" {u}")).unwrap_or_default()
            )
            .map_err(|e| (1, miette::miette!("{e}")))?;
            if let Some(reason) = &pr.reason {
                writeln!(out, "  {reason}").map_err(|e| (1, miette::miette!("{e}")))?;
            }
        }
    }
    let mut errors = 0;
    let mut updates = 0;
    for repo in &report.repositories {
        let deps: usize = repo
            .package_files
            .iter()
            .map(|f| f.dependencies.len())
            .sum();
        let proposals: usize = repo
            .package_files
            .iter()
            .flat_map(|f| &f.dependencies)
            .map(|d| d.updates.len())
            .sum();
        updates += proposals;
        eprintln!(
            "{}: {:?}, {deps} dependencies, {proposals} proposals{}",
            repo.repository,
            repo.status,
            repo.reason
                .as_ref()
                .map_or(String::new(), |r| format!(" ({r})"))
        );
        for warning in &repo.warnings {
            eprintln!("  warning: {warning}");
        }
        if repo.status == Status::Error {
            errors += 1;
        }
    }
    if let Some(stats) = &report.stats {
        eprintln!(
            "patchula: the Count visited {} repositor{} and found {updates} update{}; {} requests, {} 304s, {} cache hits, {} extraction hits, {} unique lookups",
            report.repositories.len(),
            if report.repositories.len() == 1 {
                "y"
            } else {
                "ies"
            },
            if updates == 1 { "" } else { "s" },
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
    Ok(if errors == 0 { 0 } else { 1 })
}

/// A lookup or run on a server. Only the repositories and the mode travel;
/// the server applies each repository's own `patchula.pkl`.
async fn remote_report(
    app: &App,
    options: &cli::Lookup,
    run: Option<&cli::Run>,
    services: &Services,
    server: &str,
) -> Result<patchula_report::Report, (u8, miette::Report)> {
    let local_only = [
        (options.local.is_some(), "--local"),
        (options.config.is_some(), "--config"),
        (options.config_dir.is_some(), "--config-dir"),
        (options.all_repos, "--all-repos"),
        (
            run.is_some_and(|r| r.allow_lockfile_pending),
            "--allow-lockfile-pending",
        ),
        // The server's operator setting decides the checkbox wording.
        (app.recheck.is_some(), "--recheck"),
    ];
    if let Some((_, flag)) = local_only.iter().find(|(set, _)| *set) {
        return Err((
            2,
            miette::miette!(
                "{flag} only works on this machine, not with --server or {}",
                remote::SERVER_VAR
            ),
        ));
    }
    let repositories = repositories(options).map_err(|e| (2, e))?;
    let mode = match run {
        None => remote::Mode::Lookup,
        Some(run) if run.dry_run => remote::Mode::DryRun,
        Some(_) => remote::Mode::Run,
    };
    remote::report(services, server, repositories, mode).await
}

/// HTTP settings from the lookup flags.
fn http_options(options: &cli::Lookup) -> Result<patchula_http::Options, (u8, miette::Report)> {
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

/// The repositories a batch covers: the ones named, every one the token can
/// reach with `--all-repos` (an empty list, which the engine discovers), or the
/// GitHub repository the current directory's Git remote points at.
fn repositories(options: &cli::Lookup) -> Result<Vec<String>, miette::Report> {
    if options.all_repos {
        if !options.repositories.is_empty() {
            return Err(miette::miette!(
                "--all-repos cannot be combined with named repositories"
            ));
        }
        return Ok(Vec::new());
    }
    if !options.repositories.is_empty() || options.local.is_some() {
        return Ok(options.repositories.clone());
    }
    current_repository().map(|repository| vec![repository])
}

/// The `owner/name` of the current checkout's GitHub remote: `origin`, or the
/// only remote when there is no `origin`.
fn current_repository() -> Result<String, miette::Report> {
    let help = "name a repository as owner/repo, or pass --all-repos for every repository the token can reach";
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let Some(remotes) = git(&["remote"]) else {
        return Err(miette::miette!(
            help = help,
            "no repository named, and the current directory is not a Git checkout"
        ));
    };
    let remotes: Vec<&str> = remotes.lines().collect();
    let remote = match remotes.as_slice() {
        _ if remotes.contains(&"origin") => "origin",
        [only] => only,
        [] => {
            return Err(miette::miette!(
                help = help,
                "no repository named, and this checkout has no Git remote"
            ));
        }
        _ => {
            return Err(miette::miette!(
                help = help,
                "no repository named, and this checkout has several remotes but no origin"
            ));
        }
    };
    git(&["remote", "get-url", remote])
        .as_deref()
        .and_then(github_repository)
        .ok_or_else(|| {
            miette::miette!(
                help = help,
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

/// The server to run on: `--server` or `PATCHULA_SERVER`, else, for a
/// checkout's own repository with no local-only flags, the one its user chose.
async fn server(
    app: &App,
    options: &cli::Lookup,
    run: Option<&cli::Run>,
    services: &Services,
) -> Result<Option<String>, (u8, miette::Report)> {
    if options.no_server {
        return Ok(None);
    }
    if let Some(server) = remote::server(options.server.as_deref())? {
        return Ok(Some(server));
    }
    let local_only = options.local.is_some()
        || options.config.is_some()
        || options.config_dir.is_some()
        || options.all_repos
        || run.is_some_and(|r| r.allow_lockfile_pending)
        || app.recheck.is_some();
    if local_only || !options.repositories.is_empty() {
        return Ok(None);
    }
    match current_repository() {
        Ok(repository) => remote::choose(services, &repository).await,
        // The local run reports the same problem.
        Err(_) => Ok(None),
    }
}

/// Build the engine from lookup flags and run one lookup or run batch.
async fn report(
    app: &App,
    options: &cli::Lookup,
    run: Option<&cli::Run>,
    services: &Services,
) -> Result<patchula_report::Report, (u8, miette::Report)> {
    use patchula_engine::{Engine, Options};
    if options.concurrency == 0 || options.concurrency > 100 {
        return Err((2, miette::miette!("concurrency must be 1..100")));
    }
    if let Some(server) = server(app, options, run, services).await? {
        return remote_report(app, options, run, services, &server).await;
    }
    let repositories = repositories(options).map_err(|e| (2, e))?;
    let http_options = http_options(options)?;
    let recheck = recheck(app, run.is_some())?;
    let config = options
        .config
        .as_ref()
        .map(|path| patchula_config::load(path))
        .transpose()
        .map_err(|e| (2, e))?;
    let token = services.token().map_err(|_| {
        (
            1,
            miette::miette!("lookup/run requires PATCHULA_TOKEN, GITHUB_TOKEN or GH_TOKEN"),
        )
    })?;
    if let Some(parent) = options.cache.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| (1, miette::miette!("cannot create cache directory: {e}")))?;
    }
    let cache = patchula_cache::Cache::open(&options.cache, Default::default())
        .await
        .map_err(|e| (1, miette::miette!("{e}")))?;
    let transport = services
        .transport()
        .map_err(|e| (1, miette::miette!("{e}")))?;
    let mut engine =
        Engine::with_http_options(&token, cache, options.concurrency, transport, http_options)
            .map_err(|e| (1, miette::miette!("{e}")))?
            .with_recheck(recheck);
    if let Some(mise_data) = user_mise_data() {
        engine = engine.with_tool_seed(mise_data);
    }
    if let Some((username, token)) = dockerhub_credentials()? {
        engine = engine.with_dockerhub_credentials(username, token);
    }
    let options = Options {
        repositories,
        config,
        config_dir: options.config_dir.clone(),
        local: options.local.clone(),
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
    Ok(report)
}
