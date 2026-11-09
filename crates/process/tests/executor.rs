//! The executor against real commands: nothing is inherited, declared secrets
//! stay out of errors, and a runaway command is stopped.
use patchula_process::{CommandSpec, Executor, Failure, SystemExecutor};
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf, time::Duration};

fn spec(program: &str, args: &[&str], env: &[(&str, &str)]) -> CommandSpec {
    CommandSpec {
        program: PathBuf::from(program),
        args: args.iter().map(|a| (*a).to_owned()).collect(),
        cwd: std::env::temp_dir(),
        env: env
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect::<BTreeMap<_, _>>(),
        secrets: Vec::new(),
        timeout: Duration::from_secs(10),
        output_limit: 1 << 16,
        complete_output: true,
    }
}

#[tokio::test]
async fn only_the_given_environment_reaches_the_command() {
    // The test process has PATH and HOME; the command must see neither.
    assert!(std::env::var_os("PATH").is_some());
    let out = SystemExecutor
        .run(spec("/usr/bin/env", &[], &[("ONLY", "1")]))
        .await
        .unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "ONLY=1\n");
}

#[tokio::test]
async fn a_declared_secret_never_appears_in_an_error() {
    let token = "token-value-for-the-test";
    let mut command = spec(
        "/bin/sh",
        &["-c", "echo \"failed with $MISE_GITHUB_TOKEN\" >&2; exit 3"],
        &[("MISE_GITHUB_TOKEN", token)],
    );
    command.secrets = vec![token.into(), String::new()];
    let error = SystemExecutor.run(command).await.unwrap_err();
    assert!(matches!(error, Failure::Exited { .. }), "{error}");
    let text = error.to_string();
    assert!(text.contains("failed with [redacted]"), "{text}");
    assert!(!text.contains(token), "{text}");
}

#[tokio::test]
async fn a_command_past_its_limit_is_killed_with_its_children() {
    let mut command = spec("/bin/sh", &["-c", "sleep 30 & wait"], &[]);
    command.timeout = Duration::from_millis(200);
    let started = std::time::Instant::now();
    let error = SystemExecutor.run(command).await.unwrap_err();
    assert!(matches!(error, Failure::TimedOut { .. }), "{error}");
    assert!(error.to_string().contains("time limit"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn output_past_the_limit_is_an_error_when_it_must_be_complete() {
    let mut command = spec("/bin/sh", &["-c", "head -c 4096 /dev/zero"], &[]);
    command.output_limit = 1024;
    let error = SystemExecutor.run(command).await.unwrap_err();
    assert!(matches!(error, Failure::OutputTooLarge), "{error}");
    assert_eq!(error.to_string(), "command output exceeded size limit");
}

#[tokio::test]
async fn a_nix_access_token_never_appears_in_an_error() {
    let token = "nix-token-for-the-test";
    let config =
        format!("experimental-features = nix-command flakes\naccess-tokens = github.com={token}\n");
    let mut command = spec(
        "/bin/sh",
        &["-c", "echo \"failed with $NIX_CONFIG\" >&2; exit 1"],
        &[("NIX_CONFIG", &config)],
    );
    command.secrets = vec![token.into()];
    let error = SystemExecutor.run(command).await.unwrap_err();
    let text = error.to_string();
    assert!(text.contains("github.com=[redacted]"), "{text}");
    assert!(!text.contains(token), "{text}");
}

#[tokio::test]
async fn failures_display_their_parts() {
    let spawn = Failure::Spawn {
        program: PathBuf::from("gh"),
        message: "No such file or directory".into(),
    };
    assert_eq!(
        spawn.to_string(),
        "cannot start gh: No such file or directory"
    );
    let exited = Failure::Exited {
        status: "exit status: 101".into(),
        stdout: "out".into(),
        stderr: "err".into(),
    };
    assert_eq!(exited.to_string(), "exit status: 101: out err");
    assert!(std::error::Error::source(&exited).is_none());
}
