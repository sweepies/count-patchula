//! Command-line entry point for rustovate.
#![forbid(unsafe_code)]

mod cli;

use cli::{App, Command, ConfigCommand};
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let app = App::parse();
    let result = match app.command {
        Command::Config(config) => match config.command {
            ConfigCommand::Schema => write!(io::stdout().lock(), "{}", rustovate_config::SCHEMA)
                .map_err(|error| (1, error.to_string())),
            ConfigCommand::Check(check) => Err((
                2,
                format!("{}: Pkl validation follows in SWE-83", check.path.display()),
            )),
        },
        Command::Lookup => Err((1, "lookup: not implemented yet (Phase 1)".into())),
        Command::Run => Err((1, "run: not implemented yet (Phase 1)".into())),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, message)) => {
            if app.json {
                println!(
                    "{}",
                    serde_json::json!({"schemaVersion": 1, "error": message, "exitCode": code})
                );
            } else {
                let color = app.color == cli::Color::Always
                    || (app.color == cli::Color::Auto && io::stderr().is_terminal());
                let name = if color {
                    "\x1b[31mrustovate\x1b[0m"
                } else {
                    "rustovate"
                };
                eprintln!("{name}: {message}");
                if app.verbose > 0 {
                    eprintln!("rustovate: exiting with status {code}");
                }
            }
            ExitCode::from(code)
        }
    }
}
