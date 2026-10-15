//! Command-line entry point for rustovate.
#![forbid(unsafe_code)]

mod cli;

use cli::{App, Command, ConfigCommand};
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

async fn run(app: &App) -> Result<u8, (u8, miette::Report)> {
    match &app.command {
        Command::Config(config) => match &config.command {
            ConfigCommand::Schema => write!(io::stdout().lock(), "{}", rustovate_config::SCHEMA)
                .map(|_| 0)
                .map_err(|error| (1, miette::miette!("{error}"))),
            ConfigCommand::Check(check) => {
                let config = rustovate_config::load(&check.path).map_err(|error| (2, error))?;
                if app.verbose > 0 && !app.json {
                    eprintln!(
                        "rustovate: {} managers, {} rules, {} extractors",
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
        },
        Command::Lookup(options) => lookup(app, options).await,
        Command::Run => Err((1, miette::miette!("run: not implemented yet (Phase 1)"))),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let app = App::parse();
    // Only our explicitly sanitized events reach stderr. Dependency HTTP debug
    // logs can contain headers even when the user requests maximum verbosity.
    use tracing_subscriber::{filter::Targets, prelude::*};
    let level = std::env::var("RUSTOVATE_LOG")
        .ok()
        .and_then(|value| value.parse::<tracing::level_filters::LevelFilter>().ok())
        .unwrap_or(if app.verbose >= 2 {
            tracing::level_filters::LevelFilter::DEBUG
        } else {
            tracing::level_filters::LevelFilter::WARN
        });
    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(io::stderr)
                .with_ansi(false)
                .with_filter(Targets::new().with_target("rustovate", level)),
        )
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
    match run(&app).await {
        Ok(code) => ExitCode::from(code),
        Err((code, error)) => {
            if app.json {
                println!(
                    "{}",
                    serde_json::json!({"schemaVersion": 1, "error": format!("{error:#}"), "exitCode": code})
                );
            } else {
                eprintln!("{error:?}");
                if app.verbose > 0 {
                    eprintln!("rustovate: exiting with status {code}");
                }
            }
            ExitCode::from(code)
        }
    }
}

async fn lookup(app: &App, options: &cli::Lookup) -> Result<u8, (u8, miette::Report)> {
    use rustovate_engine::{Engine, Options};
    use rustovate_report::Status;
    use std::sync::Arc;
    if options.concurrency == 0 || options.concurrency > 100 {
        return Err((2, miette::miette!("concurrency must be 1..100")));
    }
    let mut http_options = rustovate_http::Options {
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
    let config = options
        .config
        .as_ref()
        .map(|path| rustovate_config::load(path))
        .transpose()
        .map_err(|e| (2, e))?;
    let token = std::env::var("RUSTOVATE_TOKEN")
        .or_else(|_| std::env::var("GITHUB_TOKEN"))
        .or_else(|_| std::env::var("GH_TOKEN"))
        .map_err(|_| {
            (
                1,
                miette::miette!("lookup requires RUSTOVATE_TOKEN, GITHUB_TOKEN or GH_TOKEN"),
            )
        })?;
    if let Some(parent) = options.cache.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| (1, miette::miette!("cannot create cache directory: {e}")))?;
    }
    let cache = rustovate_cache::Cache::open(&options.cache, Default::default())
        .await
        .map_err(|e| (1, miette::miette!("{e}")))?;
    let transport =
        Arc::new(rustovate_http::Network::new().map_err(|e| (1, miette::miette!("{e}")))?);
    let mut engine =
        Engine::with_http_options(&token, cache, options.concurrency, transport, http_options)
            .map_err(|e| (1, miette::miette!("{e}")))?;
    match (
        std::env::var("RUSTOVATE_DOCKERHUB_USERNAME").ok(),
        std::env::var("RUSTOVATE_DOCKERHUB_TOKEN").ok(),
    ) {
        (None, None) => {}
        (Some(username), Some(token)) if !username.is_empty() && !token.is_empty() => {
            engine = engine.with_dockerhub_credentials(username, token);
        }
        _ => {
            return Err((
                2,
                miette::miette!(
                    "set both RUSTOVATE_DOCKERHUB_USERNAME and RUSTOVATE_DOCKERHUB_TOKEN to nonempty values"
                ),
            ));
        }
    }
    let report = engine
        .lookup(Options {
            repositories: options.repositories.clone(),
            config,
            config_dir: options.config_dir.clone(),
        })
        .await
        .map_err(|e| {
            let code = if matches!(e, rustovate_engine::Error::Configuration(_)) {
                2
            } else {
                1
            };
            (code, miette::miette!("{e}"))
        })?;
    if app.json {
        serde_json::to_writer(io::stdout().lock(), &report)
            .map_err(|e| (1, miette::miette!("cannot write report: {e}")))?;
        println!();
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
            "rustovate: {} repos, {updates} proposals; {} requests, {} 304s, {} cache hits, {} extraction hits, {} unique lookups",
            report.repositories.len(),
            stats.requests,
            stats.not_modified,
            stats.cache_hits,
            stats.extraction_hits,
            stats.lookups
        );
        eprintln!(
            "rustovate timings: {}",
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
