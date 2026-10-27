//! The repository's mise tasks that need more than a line or two. Each command
//! is named after its mise task, and mise is the way to run it: `mise run
//! cache:prune` runs `cargo xtask cache:prune` with the task's tools.

mod cache;
mod ci;
mod conformance;
mod corpus;
mod registry;
mod release;
mod replay;
mod stress;

use serde_json::Value;
use std::process::{Command, ExitCode, Stdio};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// A command's name, entry point and argument synopsis.
type Task = (&'static str, fn(Args) -> Result, &'static str);

const COMMANDS: &[Task] = &[
    (
        "cache:prune",
        cache::prune,
        "[--ref <ref>] [--all] [--drop-others] [prefix]...",
    ),
    ("ci", ci::all, "[--log-dir <dir>]"),
    ("ci:changes", ci::changes, ""),
    (
        "conformance:inspect",
        conformance::inspect,
        "<triage> [manager|warnings] [limit]",
    ),
    (
        "conformance:logs",
        conformance::logs,
        "<log> <repository> [manager]",
    ),
    (
        "corpus:report",
        corpus::report,
        "--cache <dir> [--out <file>] [--summary <file>]",
    ),
    ("registry:update", registry::update, ""),
    ("release:package", release::package, "<target>..."),
    (
        "server:replay",
        replay::replay,
        "<payload> <event> <delivery> [url]",
    ),
    (
        "test:stress",
        stress::stress,
        "[--runs <runs>] [--filter <filter>] [--log-dir <dir>]",
    ),
];

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let name = args.next().unwrap_or_default();
    let Some((name, command, usage)) = COMMANDS.iter().find(|(n, ..)| *n == name) else {
        let names: Vec<_> = COMMANDS.iter().map(|(n, ..)| *n).collect();
        eprintln!("usage: cargo xtask <{}>", names.join("|"));
        return ExitCode::from(2);
    };
    let args: Vec<String> = args.collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: mise run {name} {usage}");
        return ExitCode::SUCCESS;
    }
    match command(Args { rest: args, usage }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{name}: {error}");
            ExitCode::FAILURE
        }
    }
}

/// A command's arguments: options are taken out by name, and whatever remains
/// must be positional.
pub struct Args {
    rest: Vec<String>,
    usage: &'static str,
}

impl Args {
    fn flag(&mut self, name: &str) -> bool {
        let before = self.rest.len();
        self.rest.retain(|a| a != name);
        self.rest.len() != before
    }

    fn value(&mut self, name: &str) -> Result<Option<String>> {
        let prefix = format!("{name}=");
        let mut found = None;
        let mut index = 0;
        while index < self.rest.len() {
            if self.rest[index] == name {
                if index + 1 == self.rest.len() {
                    return Err(format!("{name} needs a value").into());
                }
                found = Some(self.rest.remove(index + 1));
                self.rest.remove(index);
            } else if let Some(value) = self.rest[index].strip_prefix(&prefix) {
                found = Some(value.to_owned());
                self.rest.remove(index);
            } else {
                index += 1;
            }
        }
        Ok(found)
    }

    fn positional(self, min: usize, max: usize) -> Result<Vec<String>> {
        if let Some(option) = self.rest.iter().find(|a| a.starts_with("--")) {
            return Err(format!("unknown option {option}; usage: {}", self.usage).into());
        }
        if self.rest.len() < min || self.rest.len() > max {
            return Err(format!("usage: {}", self.usage).into());
        }
        Ok(self.rest)
    }
}

fn describe(command: &Command) -> String {
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Runs a command with inherited output and fails unless it succeeds.
fn run(command: &mut Command) -> Result {
    let status = command
        .status()
        .map_err(|e| format!("could not start {}: {e}", describe(command)))?;
    if !status.success() {
        return Err(format!("{} failed: {status}", describe(command)).into());
    }
    Ok(())
}

/// Runs a command and returns its standard output; its errors go to ours.
fn output(command: &mut Command) -> Result<String> {
    let result = command
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("could not start {}: {e}", describe(command)))?;
    if !result.status.success() {
        return Err(format!("{} failed: {}", describe(command), result.status).into());
    }
    Ok(String::from_utf8(result.stdout)?)
}

/// A JSON value as jq's string interpolation prints it: strings bare, the
/// rest as JSON.
fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
