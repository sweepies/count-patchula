//! The window's one way to the engine: the `patchula` binary. Lookups are
//! `patchula lookup --json` and writes are `patchula apply --json`, so the
//! window holds no credentials, makes no requests and edits no files itself.
use crate::model::Call;
use patchula_report::Report;
use patchula_words::{Manager, Update as UpdateKind};
use serde::Deserialize;
use std::path::PathBuf;

/// The environment variable that names the binary to run instead of the
/// `patchula` on PATH.
pub const PROGRAM_VAR: &str = "PATCHULA_BIN";

/// Where the report comes from, which decides whether updates can be written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The working tree in the current directory.
    Checkout,
    /// Repositories read through the GitHub API.
    GitHub,
    /// This checkout, looked up on a Count Patchula server.
    Server,
    /// A report saved from `patchula lookup --json`.
    File(PathBuf),
    /// The made-up report.
    Demo,
}

/// What the command line asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub program: String,
    /// The working tree to look up and write, when it is not the directory
    /// the window started in.
    pub dir: Option<PathBuf>,
    pub source: Source,
    pub repositories: Vec<String>,
    /// Flags `lookup` and `apply` both take.
    pub scan: Vec<String>,
    /// Flags only `lookup` takes.
    pub lookup: Vec<String>,
}

/// `lookup` and `apply` flags that take a value.
const SCAN_VALUES: [&str; 4] = ["--cache", "--settings", "--concurrency", "--config"];
const DEFAULT_CONFIG: &str = "--default-config";

impl Launch {
    /// Read the arguments Rune left: the window's own flags, then the flags
    /// and repositories of `patchula lookup`.
    pub fn parse(args: &[String], program: Option<String>) -> Result<Self, String> {
        let mut launch = Launch {
            program: program
                .filter(|program| !program.is_empty())
                .unwrap_or_else(|| "patchula".into()),
            dir: None,
            source: Source::Checkout,
            repositories: Vec::new(),
            scan: Vec::new(),
            lookup: Vec::new(),
        };
        let mut file = None;
        let mut demo = false;
        let mut remote = false;
        let mut all = false;
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            let (flag, inline) = match arg.split_once('=') {
                Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_owned())),
                _ => (arg.as_str(), None),
            };
            let mut value = |name: &str| {
                inline
                    .clone()
                    .or_else(|| args.next().cloned())
                    .ok_or_else(|| format!("{name} needs a value"))
            };
            match flag {
                "--demo" => demo = true,
                "--report" => file = Some(PathBuf::from(value("--report")?)),
                "--patchula" => launch.program = value("--patchula")?,
                "--dir" => launch.dir = Some(PathBuf::from(value("--dir")?)),
                DEFAULT_CONFIG => launch.scan.push(flag.to_owned()),
                "--config-dir" => {
                    let dir = value(flag)?;
                    launch.lookup.extend([flag.to_owned(), dir]);
                }
                "--all-repos" => {
                    all = true;
                    launch.lookup.push(flag.to_owned());
                }
                "--remote" => remote = true,
                "--here" => {}
                flag if SCAN_VALUES.contains(&flag) => {
                    let value = value(flag)?;
                    launch.scan.extend([flag.to_owned(), value]);
                }
                flag if flag.starts_with('-') => {
                    return Err(format!(
                        "{flag} is not a flag the window knows; it takes --demo, --dir DIR, --report FILE, --patchula BIN, and the flags and repositories of `patchula lookup`"
                    ));
                }
                repository => launch.repositories.push(repository.to_owned()),
            }
        }
        launch.source = if demo {
            Source::Demo
        } else if let Some(file) = file {
            Source::File(file)
        } else if all || !launch.repositories.is_empty() {
            Source::GitHub
        } else if remote {
            Source::Server
        } else {
            Source::Checkout
        };
        // A working tree is looked up on this machine, where `apply` will
        // look it up again, unless the command line asked for the server.
        launch
            .lookup
            .push(if remote { "--remote" } else { "--here" }.to_owned());
        Ok(launch)
    }

    /// Why the window cannot write these updates, in the TUI's words.
    pub fn read_only(&self) -> Option<&'static str> {
        match self.source {
            Source::Checkout | Source::Demo => None,
            Source::Server => Some(
                "This lookup ran on the server. Start without --remote to look up this working tree on this machine.",
            ),
            Source::GitHub => Some(
                "These repositories come from GitHub. Only a working tree, with no repository named, can be written; `patchula run` opens the pull requests.",
            ),
            Source::File(_) => Some(
                "This is a saved report. Start in a checkout with no --report to write updates.",
            ),
        }
    }

    /// What is being looked at, for the sidebar.
    pub fn place(&self) -> String {
        match &self.source {
            Source::Checkout | Source::Server => self
                .dir
                .as_ref()
                .and_then(|dir| dir.canonicalize().ok())
                .or_else(|| std::env::current_dir().ok())
                .and_then(|dir| {
                    dir.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| "working tree".into()),
            Source::GitHub if self.repositories.is_empty() => "every repository".into(),
            Source::GitHub => self.repositories.join(", "),
            Source::File(path) => path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            ),
            Source::Demo => "castle-web".into(),
        }
    }

    /// One word for where the report comes from.
    pub fn origin(&self) -> &'static str {
        match self.source {
            Source::Checkout => "working tree",
            Source::Server => "on the server",
            Source::GitHub => "GitHub",
            Source::File(_) => "saved report",
            Source::Demo => "demo",
        }
    }

    /// Take the Count's defaults from now on, for a tree with no `patchula.pkl`.
    pub fn use_defaults(&mut self) {
        if !self.scan.iter().any(|flag| flag == DEFAULT_CONFIG) {
            self.scan.push(DEFAULT_CONFIG.to_owned());
        }
    }

    fn lookup_args(&self) -> Vec<String> {
        let mut args = vec![
            "--json".to_owned(),
            "--color".into(),
            "never".into(),
            "lookup".into(),
        ];
        args.extend(self.scan.iter().cloned());
        if matches!(self.source, Source::GitHub) {
            args.extend(self.lookup.iter().filter(|flag| *flag != "--here").cloned());
        } else {
            args.extend(self.lookup.iter().cloned());
        }
        args.extend(self.repositories.iter().cloned());
        args
    }

    fn apply_args(&self, call: &Call, write: bool) -> Vec<String> {
        let mut args = vec![
            "--json".to_owned(),
            "--color".into(),
            "never".into(),
            "apply".into(),
        ];
        args.extend(self.scan.iter().cloned());
        if !write {
            args.push("--dry-run".into());
        }
        args.extend(call.flags());
        args
    }
}

/// A command that did not give a result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub message: String,
    /// The tree has no `patchula.pkl`, so the Count's defaults are on offer.
    pub uninvited: bool,
}

impl Failure {
    fn new(message: impl Into<String>) -> Self {
        let message = message.into();
        Failure {
            uninvited: message.contains("has no patchula.pkl"),
            message,
        }
    }
}

/// Run `program` and read the one JSON document on its stdout. With `--json`
/// an error is a document too: `{"error": …, "exitCode": …}`.
fn run(launch: &Launch, args: &[String]) -> Result<serde_json::Value, Failure> {
    let program = &launch.program;
    let mut command = std::process::Command::new(program);
    if let Some(dir) = &launch.dir {
        command.current_dir(dir);
    }
    let output = command
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Failure::new(format!(
                    "The Count is not at home: there is no `{program}` to run. Install patchula, or name the binary with --patchula or {PROGRAM_VAR}."
                ))
            } else {
                Failure::new(format!("cannot run {program}: {error}"))
            }
        })?;
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|_| {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let said = stderr.trim();
        Failure::new(if said.is_empty() {
            format!(
                "{program} {} said nothing ({})",
                args.join(" "),
                output.status
            )
        } else {
            said.to_owned()
        })
    })?;
    match value.get("error").and_then(serde_json::Value::as_str) {
        Some(error) => Err(Failure::new(error)),
        None => Ok(value),
    }
}

/// The report this launch asks for.
pub fn lookup(launch: &Launch) -> Result<Report, Failure> {
    let json = match &launch.source {
        Source::Demo => return Ok(crate::demo::report()),
        Source::File(path) => std::fs::read(path)
            .map_err(|error| Failure::new(format!("cannot read {}: {error}", path.display())))?,
        Source::Checkout | Source::GitHub | Source::Server => {
            let value = run(launch, &launch.lookup_args())?;
            serde_json::to_vec(&value).map_err(|error| Failure::new(error.to_string()))?
        }
    };
    Report::from_json(&json).map_err(|error| Failure::new(error.to_string()))
}

/// One update in `patchula apply --json`'s answer.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Written {
    pub manager: Manager,
    pub file: String,
    pub name: String,
    pub current: String,
    #[serde(rename = "type")]
    pub kind: UpdateKind,
    pub value: Option<String>,
    pub version: Option<String>,
    pub digest: Option<String>,
    /// Why it could not be written in place, on a skipped update.
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Patch {
    pub path: String,
    pub diff: String,
}

/// What `patchula apply --json` did, or with `--dry-run` would do.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Applied {
    pub updates: Vec<Written>,
    #[serde(default)]
    pub skipped: Vec<Written>,
    #[serde(default)]
    pub files: Vec<Patch>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl Applied {
    /// Fold another call's answer into this one.
    fn extend(&mut self, other: Applied) {
        self.updates.extend(other.updates);
        self.skipped.extend(other.skipped);
        for patch in other.files {
            // Two calls that touch one file each diff it against the tree
            // as it stood, so their diffs are shown one after the other.
            self.files.push(patch);
        }
        for warning in other.warnings {
            if !self.warnings.contains(&warning) {
                self.warnings.push(warning);
            }
        }
    }
}

/// Run each call in turn: the plan when `write` is false, the edit when true.
/// A failure stops the rest, and says how many calls had already run.
pub fn apply(launch: &Launch, calls: &[Call], write: bool) -> Result<Applied, Failure> {
    let mut all = Applied::default();
    for (done, call) in calls.iter().enumerate() {
        let value = run(launch, &launch.apply_args(call, write)).map_err(|failure| {
            if write && done > 0 {
                Failure::new(format!(
                    "{} The first {done} of {} writes were already made.",
                    failure.message,
                    calls.len()
                ))
            } else {
                failure
            }
        })?;
        let applied: Applied = serde_json::from_value(value).map_err(|error| {
            Failure::new(format!("unexpected answer from patchula apply: {error}"))
        })?;
        all.extend(applied);
    }
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Launch, String> {
        Launch::parse(
            &args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
            None,
        )
    }

    #[test]
    fn no_arguments_look_up_the_working_tree_here() {
        let launch = parse(&[]).unwrap();
        assert_eq!(launch.source, Source::Checkout);
        assert_eq!(launch.program, "patchula");
        assert_eq!(
            launch.lookup_args(),
            ["--json", "--color", "never", "lookup", "--here"]
        );
        assert_eq!(launch.read_only(), None);
    }

    #[test]
    fn repositories_and_lookup_flags_pass_through() {
        let launch = parse(&[
            "--cache",
            "/tmp/c",
            "--config=ci.pkl",
            "acme/app",
            "acme/api",
        ])
        .unwrap();
        assert_eq!(launch.source, Source::GitHub);
        assert_eq!(
            launch.lookup_args(),
            [
                "--json", "--color", "never", "lookup", "--cache", "/tmp/c", "--config", "ci.pkl",
                "acme/app", "acme/api"
            ]
        );
        assert!(launch.read_only().is_some());
    }

    #[test]
    fn apply_takes_the_shared_flags_and_the_calls_filters() {
        let mut launch = parse(&["--concurrency", "4"]).unwrap();
        launch.use_defaults();
        launch.use_defaults();
        let call = Call {
            managers: vec![Manager::Cargo],
            kinds: vec![UpdateKind::Patch],
            names: vec!["serde".into()],
        };
        assert_eq!(
            launch.apply_args(&call, false),
            [
                "--json",
                "--color",
                "never",
                "apply",
                "--concurrency",
                "4",
                "--default-config",
                "--dry-run",
                "--manager",
                "cargo",
                "--type",
                "patch",
                "--dependency",
                "regex:^serde$"
            ]
        );
        assert!(
            !launch
                .apply_args(&call, true)
                .contains(&"--dry-run".to_owned())
        );
    }

    #[test]
    fn the_windows_own_flags_choose_the_source() {
        assert_eq!(parse(&["--demo"]).unwrap().source, Source::Demo);
        assert_eq!(
            parse(&["--report", "r.json"]).unwrap().source,
            Source::File("r.json".into())
        );
        assert_eq!(parse(&["--remote"]).unwrap().source, Source::Server);
        assert_eq!(parse(&["--patchula", "/opt/p"]).unwrap().program, "/opt/p");
        assert!(
            parse(&["--frobnicate"])
                .unwrap_err()
                .contains("--frobnicate")
        );
        assert!(parse(&["--report"]).unwrap_err().contains("needs a value"));
    }

    #[test]
    fn a_json_error_is_a_failure_and_a_missing_config_offers_defaults() {
        let failure = Failure::new("no repository named, and /x has no patchula.pkl to read");
        assert!(failure.uninvited);
        assert!(!Failure::new("boom").uninvited);
        let nowhere = parse(&["--patchula", "/nonexistent/patchula"]).unwrap();
        let missing = run(&nowhere, &[]).unwrap_err();
        assert!(
            missing.message.contains("not at home"),
            "{}",
            missing.message
        );
    }

    #[test]
    fn an_apply_answer_parses() {
        let answer = serde_json::json!({
            "schemaVersion": 1, "written": false,
            "updates": [{"manager": "cargo", "file": "Cargo.toml", "name": "serde", "package": "serde",
                "current": "1.0.219", "type": "patch", "value": "1.0.228", "version": "1.0.228", "digest": null}],
            "skipped": [{"manager": "npm", "file": "package.json", "name": "left-pad", "package": "left-pad",
                "current": "1.0.0", "type": "major", "value": null, "version": "2.0.0", "digest": null,
                "reason": "only its lockfile names it"}],
            "files": [{"path": "Cargo.toml", "diff": "--- a/Cargo.toml\n+++ b/Cargo.toml\n"}],
            "filtered": 3, "notices": [], "warnings": [], "lookupFailures": [], "exitCode": 3
        });
        let applied: Applied = serde_json::from_value(answer).unwrap();
        assert_eq!(applied.updates[0].kind, UpdateKind::Patch);
        assert_eq!(
            applied.skipped[0].reason.as_deref(),
            Some("only its lockfile names it")
        );
        assert_eq!(applied.files.len(), 1);
    }

    /// A stand-in for the binary in `dir`: it notes its arguments and answers
    /// `lookup` and `apply` from files in its working directory, which the
    /// launch makes `dir`.
    #[cfg(unix)]
    fn stand_in(dir: &std::path::Path) -> Launch {
        use std::os::unix::fs::PermissionsExt;
        let program = dir.join("patchula");
        std::fs::write(
            &program,
            "#!/bin/sh\necho \"$*\" >> args\ncase \"$4\" in\n  lookup) cat lookup.json ;;\n  apply) cat apply.json ;;\nesac\n",
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        parse(&[
            "--dir",
            dir.to_str().unwrap(),
            "--patchula",
            program.to_str().unwrap(),
        ])
        .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn a_lookup_runs_the_binary_in_the_tree_and_reads_its_report() {
        let dir = tempfile::tempdir().unwrap();
        let launch = stand_in(dir.path());
        let report = crate::demo::report();
        std::fs::write(
            dir.path().join("lookup.json"),
            serde_json::to_vec(&report).unwrap(),
        )
        .unwrap();
        assert_eq!(lookup(&launch).unwrap(), report);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("args")).unwrap(),
            "--json --color never lookup --here\n"
        );

        // With --json the binary's errors are documents on stdout too.
        std::fs::write(
            dir.path().join("lookup.json"),
            r#"{"error":"no repository named, and /x has no patchula.pkl to read","exitCode":2,"schemaVersion":1}"#,
        )
        .unwrap();
        assert!(lookup(&launch).unwrap_err().uninvited);

        // Anything else on stdout is a failure, not a report.
        std::fs::write(dir.path().join("lookup.json"), "The Count vants a word").unwrap();
        let failure = lookup(&launch).unwrap_err();
        assert!(
            failure.message.contains("said nothing"),
            "{}",
            failure.message
        );
    }

    #[cfg(unix)]
    #[test]
    fn apply_runs_each_call_and_folds_the_answers() {
        let dir = tempfile::tempdir().unwrap();
        let launch = stand_in(dir.path());
        std::fs::write(
            dir.path().join("apply.json"),
            r#"{"schemaVersion":1,"written":false,"exitCode":3,"filtered":0,"notices":[],"lookupFailures":[],
                "updates":[{"manager":"npm","file":"package.json","name":"lodash","package":"lodash",
                    "current":"4.17.20","type":"patch","value":"4.17.23","version":"4.17.23","digest":null}],
                "skipped":[],"warnings":["mind the lockfile"],
                "files":[{"path":"package.json","diff":"--- a/package.json\n+++ b/package.json\n"}]}"#,
        )
        .unwrap();
        let call = |kind| Call {
            managers: vec![Manager::Npm],
            kinds: vec![kind],
            names: vec!["lodash".into()],
        };
        let calls = [call(UpdateKind::Patch), call(UpdateKind::Major)];
        let planned = apply(&launch, &calls, false).unwrap();
        assert_eq!(planned.updates.len(), 2);
        assert_eq!(planned.files.len(), 2);
        assert_eq!(planned.warnings, ["mind the lockfile"]);
        apply(&launch, &calls[..1], true).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("args")).unwrap(),
            "--json --color never apply --dry-run --manager npm --type patch --dependency regex:^lodash$\n\
             --json --color never apply --dry-run --manager npm --type major --dependency regex:^lodash$\n\
             --json --color never apply --manager npm --type patch --dependency regex:^lodash$\n"
        );
    }

    /// The real binary on a tree made for the purpose, through everything the
    /// window does with it. It needs the binary and the network, so it runs
    /// only when asked: `PATCHULA_BIN=… cargo test -- --ignored the_real_cli`.
    #[test]
    #[ignore = "needs PATCHULA_BIN and the network"]
    fn the_real_cli_plans_and_writes_what_the_window_picks() {
        let program =
            std::env::var(PROGRAM_VAR).expect("PATCHULA_BIN names the binary to test against");
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("package.json");
        std::fs::write(
            &manifest,
            "{\n  \"name\": \"smoke\",\n  \"dependencies\": {\n    \"lodash\": \"4.17.20\",\n    \"left-pad\": \"1.2.0\"\n  }\n}\n",
        )
        .unwrap();
        let mut launch = parse(&[
            "--dir",
            dir.path().to_str().unwrap(),
            "--patchula",
            &program,
        ])
        .unwrap();
        assert!(
            lookup(&launch).unwrap_err().uninvited,
            "a tree with no patchula.pkl"
        );
        launch.use_defaults();
        let data = crate::model::Data::new(&lookup(&launch).unwrap());
        let lodash = data
            .updates
            .iter()
            .position(|update| update.name == "lodash" && update.kind == UpdateKind::Patch)
            .expect("lodash 4.17.20 has a patch update");
        let picked = std::collections::BTreeSet::from([lodash]);
        let calls = crate::model::calls(&data.updates, &picked);

        let planned = apply(&launch, &calls, false).unwrap();
        assert_eq!(planned.updates.len(), 1, "{planned:?}");
        assert_eq!(planned.updates[0].name, "lodash");
        assert_eq!(planned.files.len(), 1);
        assert!(
            std::fs::read_to_string(&manifest)
                .unwrap()
                .contains("\"lodash\": \"4.17.20\"")
        );

        let wrote = apply(&launch, &calls, true).unwrap();
        let after = std::fs::read_to_string(&manifest).unwrap();
        let version = wrote.updates[0].version.clone().unwrap();
        assert!(
            after.contains(&format!("\"lodash\": \"{version}\"")),
            "{after}"
        );
        assert!(
            after.contains("\"left-pad\": \"1.2.0\""),
            "only the pick is written: {after}"
        );
    }
}
