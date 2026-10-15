use std::path::PathBuf;
use usage_rs::{Args, Cli, Subcommands, ValueEnum};

/// A fast dependency update bot for GitHub.
///
/// Exit codes: 0 = success; 1 = runtime error; 2 = usage or configuration error.
///
/// No interactive prompts are used. Future prompts require both stdin and stdout
/// to be TTYs and use demand.
#[derive(Cli)]
#[usage(bin = "rustovate", version)]
pub struct App {
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
    pub command: Command,
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
    /// Run dependency updates (not implemented yet, Phase 1).
    Run,
}

#[derive(Args)]
pub struct Lookup {
    /// Repositories as owner/repo; omit to discover all accessible repositories.
    pub repositories: Vec<String>,
    /// Shared SQLite cache path.
    #[usage(long, default = ".rustovate-cache.sqlite")]
    pub cache: PathBuf,
    /// Maximum concurrent repository and HTTP reads (1–100).
    #[usage(long, default = "8")]
    pub concurrency: usize,
    /// Local Pkl override applied to every selected repository.
    #[usage(long)]
    pub config: Option<PathBuf>,
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
    /// Print the bundled Rustovate.pkl schema as Pkl source.
    Schema,
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
        let Command::Lookup(options) = app.command else {
            panic!("lookup")
        };
        assert_eq!(options.repositories, vec!["sweepies/renovate-runner"]);
        assert_eq!(options.concurrency, 4);
        let app = App::parse_from(&[std::ffi::OsStr::new("lookup")]).unwrap();
        let Command::Lookup(options) = app.command else {
            panic!("lookup")
        };
        assert!(options.repositories.is_empty());
    }
}
