use super::Runner;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt};

/// Deliberately not Debug: the Git-only environment may contain authorization.
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
                        // Only Git receives this value; do not echo it on errors.
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
impl Runner {
    pub(super) fn command(&self, driver: &Path) -> CommandSpec {
        let home = driver.join("home");
        // The caller creates driver; failures creating HOME are reported by the
        // tool without falling back to the caller's home or credential files.
        let _ = std::fs::create_dir_all(&home);
        let env: BTreeMap<OsString, OsString> = [
            ("PATH", OsString::from("/usr/local/bin:/usr/bin:/bin")),
            ("HOME", home.as_os_str().into()),
            ("XDG_CONFIG_HOME", home.join("config").into_os_string()),
            ("XDG_CACHE_HOME", home.join("cache").into_os_string()),
            ("LANG", "C.UTF-8".into()),
            ("CI", "true".into()),
            ("MISE_DATA_DIR", self.mise_data.as_os_str().into()),
            ("MISE_NO_CONFIG", "1".into()),
            ("MISE_NO_ENV", "1".into()),
            ("MISE_NO_HOOKS", "1".into()),
            ("MISE_YES", "1".into()),
            ("MISE_AUTO_INSTALL", "true".into()),
            ("MISE_COLOR", "0".into()),
            ("GIT_CONFIG_NOSYSTEM", "1".into()),
            ("GIT_CONFIG_GLOBAL", "/dev/null".into()),
            ("GIT_TERMINAL_PROMPT", "0".into()),
            ("GIT_ALLOW_PROTOCOL", "https".into()),
            (
                "NPM_CONFIG_USERCONFIG",
                home.join("npm-user.cfg").into_os_string(),
            ),
            (
                "NPM_CONFIG_GLOBALCONFIG",
                home.join("npm-global.cfg").into_os_string(),
            ),
            ("NPM_CONFIG_IGNORE_SCRIPTS", "true".into()),
            (
                "BUN_INSTALL_CACHE_DIR",
                home.join("bun-cache").into_os_string(),
            ),
            ("CARGO_HOME", home.join("cargo").into_os_string()),
            ("GOPATH", home.join("go").into_os_string()),
            ("GOWORK", "off".into()),
            ("GOTOOLCHAIN", "local".into()),
            ("GOENV", "off".into()),
            ("TF_IN_AUTOMATION", "1".into()),
            ("TF_INPUT", "0".into()),
            ("TF_CLI_CONFIG_FILE", "/dev/null".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v))
        .collect();
        CommandSpec {
            program: self.mise.clone(),
            args: vec!["--no-config".into()],
            cwd: driver.into(),
            env,
            timeout: Duration::from_secs(180),
            output_limit: 4096,
            complete_output: false,
        }
    }
}
