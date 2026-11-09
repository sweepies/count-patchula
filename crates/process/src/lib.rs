//! Runs one external command with an explicit environment, a time limit, a
//! bounded output and a process group that dies with it.
#![forbid(unsafe_code)]
use std::{
    collections::BTreeMap, error::Error, ffi::OsString, fmt, future::Future, path::PathBuf,
    pin::Pin, process::Stdio, time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt};

/// Bytes of each stream a failed command keeps for its `Failure`.
const FAILURE_STREAM_LIMIT: usize = 4096;

/// Deliberately not Debug: the environment may contain authorization, and
/// `secrets` the values a failure must never repeat.
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<OsString, OsString>,
    /// Every secret the run was given, in every form it was delivered:
    /// env values, file contents, encodings. Empty strings are ignored.
    pub secrets: Vec<String>,
    pub timeout: Duration,
    pub output_limit: usize,
    pub complete_output: bool,
}
pub trait Executor: Send + Sync {
    fn run(
        &self,
        command: CommandSpec,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, Failure>> + Send + '_>>;
}

/// How a command failed. Output is already redacted for every declared
/// secret and bounded to [`FAILURE_STREAM_LIMIT`] bytes a stream.
#[derive(Debug)]
pub enum Failure {
    /// The program could not be started.
    Spawn {
        program: PathBuf,
        message: String,
    },
    TimedOut {
        limit: Duration,
    },
    /// The program ran and failed.
    Exited {
        status: String,
        stdout: String,
        stderr: String,
    },
    OutputTooLarge,
    Io(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { program, message } => {
                write!(f, "cannot start {}: {message}", program.display())
            }
            Self::TimedOut { limit } => {
                write!(f, "tool exceeded {}s time limit", limit.as_secs())
            }
            Self::Exited {
                status,
                stdout,
                stderr,
            } => write!(f, "{status}: {stdout} {stderr}"),
            Self::OutputTooLarge => f.write_str("command output exceeded size limit"),
            Self::Io(message) => f.write_str(message),
        }
    }
}
impl Error for Failure {}

/// `text` with every declared secret replaced; longer secrets first so an
/// overlapping pair cannot leave half of the longer one behind.
fn redact(text: &str, secrets: &[String]) -> String {
    let mut secrets: Vec<&str> = secrets
        .iter()
        .map(String::as_str)
        .filter(|secret| !secret.is_empty())
        .collect();
    if secrets.is_empty() || text.is_empty() {
        return text.to_owned();
    }
    secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    let mut redacted = text.to_owned();
    for secret in secrets {
        redacted = redacted.replace(secret, "[redacted]");
    }
    redacted
}

pub struct SystemExecutor;
async fn drain(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
) -> Result<(Vec<u8>, bool), Failure> {
    let mut result = Vec::new();
    let mut truncated = false;
    let mut buf = [0; 8192];
    loop {
        let len = reader
            .read(&mut buf)
            .await
            .map_err(|e| Failure::Io(e.to_string()))?;
        if len == 0 {
            break;
        }
        let keep = len.min(limit.saturating_sub(result.len()));
        result.extend_from_slice(&buf[..keep]);
        truncated |= keep < len;
    }
    Ok((result, truncated))
}
impl Executor for SystemExecutor {
    fn run(
        &self,
        spec: CommandSpec,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, Failure>> + Send + '_>> {
        Box::pin(async move {
            let mut command = tokio::process::Command::new(&spec.program);
            command
                .args(&spec.args)
                .current_dir(&spec.cwd)
                .env_clear()
                .envs(&spec.env)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            #[cfg(unix)]
            {
                command.process_group(0);
            }
            let mut child = command.spawn().map_err(|e| Failure::Spawn {
                program: spec.program.clone(),
                message: e.to_string(),
            })?;
            let pid = child.id();
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| Failure::Io("missing stdout".into()))?;
            let stderr = child
                .stderr
                .take()
                .ok_or_else(|| Failure::Io("missing stderr".into()))?;
            let result = tokio::time::timeout(spec.timeout, async {
                let (status, out, err) = tokio::join!(
                    child.wait(),
                    drain(stdout, spec.output_limit),
                    drain(stderr, FAILURE_STREAM_LIMIT)
                );
                Ok::<_, Failure>((status.map_err(|e| Failure::Io(e.to_string()))?, out?, err?))
            })
            .await;
            match result {
                Err(_) => {
                    #[cfg(unix)]
                    if let Some(pid) = pid
                        .and_then(|pid| i32::try_from(pid).ok())
                        .and_then(rustix::process::Pid::from_raw)
                    {
                        let _ =
                            rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
                    }
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    Err(Failure::TimedOut {
                        limit: spec.timeout,
                    })
                }
                Ok(result) => {
                    let (status, (out, truncated), (err, _)) = result?;
                    if !status.success() {
                        Err(Failure::Exited {
                            status: status.to_string(),
                            stdout: redact(
                                &String::from_utf8_lossy(
                                    &out[..out.len().min(FAILURE_STREAM_LIMIT)],
                                ),
                                &spec.secrets,
                            ),
                            stderr: redact(&String::from_utf8_lossy(&err), &spec.secrets),
                        })
                    } else if truncated && spec.complete_output {
                        Err(Failure::OutputTooLarge)
                    } else {
                        Ok(out)
                    }
                }
            }
        })
    }
}
