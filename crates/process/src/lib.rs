//! Runs one external command with an explicit environment, a time limit, a
//! bounded output and a process group that dies with it.
#![forbid(unsafe_code)]
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::{
    collections::BTreeMap, ffi::OsString, future::Future, path::PathBuf, pin::Pin, process::Stdio,
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt};

/// Deliberately not Debug: the environment may contain authorization.
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<OsString, OsString>,
    pub timeout: Duration,
    pub output_limit: usize,
    pub complete_output: bool,
}
pub trait Executor: Send + Sync {
    fn run(
        &self,
        command: CommandSpec,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + '_>>;
}
pub struct SystemExecutor;
async fn drain(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
) -> Result<(Vec<u8>, bool), String> {
    let mut result = Vec::new();
    let mut truncated = false;
    let mut buf = [0; 8192];
    loop {
        let len = reader.read(&mut buf).await.map_err(|e| e.to_string())?;
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
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + '_>> {
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
            let mut child = command
                .spawn()
                .map_err(|e| format!("cannot start mise: {e}"))?;
            let pid = child.id();
            let stdout = child.stdout.take().ok_or("missing stdout")?;
            let stderr = child.stderr.take().ok_or("missing stderr")?;
            let result = tokio::time::timeout(spec.timeout, async {
                let (status, out, err) = tokio::join!(
                    child.wait(),
                    drain(stdout, spec.output_limit),
                    drain(stderr, 4096)
                );
                Ok::<_, String>((status.map_err(|e| e.to_string())?, out?, err?))
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
                    Err(format!(
                        "tool exceeded {}s time limit",
                        spec.timeout.as_secs()
                    ))
                }
                Ok(result) => {
                    let (status, (out, truncated), (err, _)) = result?;
                    if !status.success() {
                        let mut message = format!(
                            "exit {status}: {} {}",
                            String::from_utf8_lossy(&out[..out.len().min(4096)]),
                            String::from_utf8_lossy(&err)
                        );
                        // Credentials passed through the environment never reach errors.
                        if let Some(token) = spec
                            .env
                            .get(std::ffi::OsStr::new("MISE_GITHUB_TOKEN"))
                            .and_then(|s| s.to_str())
                            .filter(|token| !token.is_empty())
                        {
                            message = message.replace(token, "[redacted]");
                        }
                        if let Some(value) = spec
                            .env
                            .get(std::ffi::OsStr::new("GIT_CONFIG_VALUE_0"))
                            .and_then(|s| s.to_str())
                        {
                            message = message.replace(value, "[redacted]");
                            if let Some(encoded) = value.strip_prefix("Authorization: Basic ") {
                                message = message.replace(encoded, "[redacted]");
                                if let Ok(bytes) = STANDARD.decode(encoded)
                                    && let Ok(credentials) = String::from_utf8(bytes)
                                    && let Some((_, token)) = credentials.split_once(':')
                                    && !token.is_empty()
                                {
                                    message = message.replace(token, "[redacted]");
                                }
                            }
                        }
                        Err(message)
                    } else if truncated && spec.complete_output {
                        Err("command output exceeded size limit".into())
                    } else {
                        Ok(out)
                    }
                }
            }
        })
    }
}
