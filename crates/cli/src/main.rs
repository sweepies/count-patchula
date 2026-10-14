//! Command-line entry point for rustovate.
#![forbid(unsafe_code)]

mod cli;

use cli::{App, Command, ConfigCommand};
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

fn run(app: &App) -> Result<(), (u8, miette::Report)> {
    match &app.command {
        Command::Config(config) => match &config.command {
            ConfigCommand::Schema => write!(io::stdout().lock(), "{}", rustovate_config::SCHEMA)
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
                    .map_err(|error| (1, miette::miette!("{error}")))
            }
        },
        Command::Lookup => Err((1, miette::miette!("lookup: not implemented yet (Phase 1)"))),
        Command::Run => Err((1, miette::miette!("run: not implemented yet (Phase 1)"))),
    }
}

fn main() -> ExitCode {
    let app = App::parse();
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
    match run(&app) {
        Ok(()) => ExitCode::SUCCESS,
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
