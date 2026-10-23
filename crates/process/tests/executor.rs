//! The executor against real commands: nothing is inherited, credentials stay
//! out of errors, and a runaway command is stopped.
use patchula_process::{CommandSpec, Executor, SystemExecutor};
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
async fn a_github_token_never_appears_in_an_error() {
    let token = "token-value-for-the-test";
    let error = SystemExecutor
        .run(spec(
            "/bin/sh",
            &["-c", "echo \"failed with $MISE_GITHUB_TOKEN\" >&2; exit 3"],
            &[("MISE_GITHUB_TOKEN", token)],
        ))
        .await
        .unwrap_err();
    assert!(error.contains("failed with [redacted]"), "{error}");
    assert!(!error.contains(token), "{error}");
}

#[tokio::test]
async fn a_command_past_its_limit_is_killed_with_its_children() {
    let mut command = spec("/bin/sh", &["-c", "sleep 30 & wait"], &[]);
    command.timeout = Duration::from_millis(200);
    let started = std::time::Instant::now();
    let error = SystemExecutor.run(command).await.unwrap_err();
    assert!(error.contains("time limit"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn output_past_the_limit_is_an_error_when_it_must_be_complete() {
    let mut command = spec("/bin/sh", &["-c", "head -c 4096 /dev/zero"], &[]);
    command.output_limit = 1024;
    let error = SystemExecutor.run(command).await.unwrap_err();
    assert!(error.contains("size limit"), "{error}");
}
