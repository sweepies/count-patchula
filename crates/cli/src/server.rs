//! Flags/env are operator overrides; secret contents never enter the usage model.
use crate::cli::{App, Recheck, Serve};
use std::{path::PathBuf, time::Duration};

type Result<T> = std::result::Result<T, (u8, miette::Report)>;
fn invalid(message: &str) -> (u8, miette::Report) {
    (2, miette::miette!("{message}"))
}
fn env(name: &str) -> Result<Option<String>> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(_) => Err(invalid("server environment value is not valid UTF-8")),
    }
}
fn secret(path: Option<&PathBuf>, file_env: &str, value_env: &str) -> Result<Vec<u8>> {
    let path = path.cloned().or(env(file_env)?.map(PathBuf::from));
    let value = env(value_env)?;
    if path.is_some() && value.is_some() {
        return Err(invalid(
            "choose either a secret file or a secret environment value",
        ));
    }
    let bytes = if let Some(path) = path {
        std::fs::read(path).map_err(|_| invalid("cannot read server secret file"))?
    } else {
        value
            .ok_or_else(|| {
                invalid("server requires App private key and webhook secret via env or files")
            })?
            .into_bytes()
    };
    // Strip terminal line endings from secret files, preserving other secret bytes.
    let mut bytes = bytes;
    while bytes.last().is_some_and(|b| matches!(b, b'\r' | b'\n')) {
        bytes.pop();
    }
    if bytes.is_empty() {
        return Err(invalid("server secret must be nonempty"));
    }
    Ok(bytes)
}
pub async fn run(app: &App, serve: &Serve) -> Result<u8> {
    let settings = serve
        .settings
        .clone()
        .or(env("PATCHULA_SERVER_SETTINGS")?.map(PathBuf::from))
        .ok_or_else(|| invalid("serve requires --settings or PATCHULA_SERVER_SETTINGS"))?;
    let listen = serve.listen.map(Ok).unwrap_or_else(|| {
        env("PATCHULA_LISTEN")?
            .unwrap_or_else(|| "127.0.0.1:8080".into())
            .parse()
            .map_err(|_| invalid("invalid server listen address"))
    })?;
    let app_id = serve.app_id.map(Ok).unwrap_or_else(|| {
        env("PATCHULA_APP_ID")?
            .ok_or_else(|| invalid("server requires --app-id or PATCHULA_APP_ID"))?
            .parse()
            .map_err(|_| invalid("invalid App ID"))
    })?;
    let timeout = serve.shutdown_timeout_secs.map(Ok).unwrap_or_else(|| {
        env("PATCHULA_SHUTDOWN_TIMEOUT_SECS")?
            .unwrap_or_else(|| "60".into())
            .parse::<u64>()
            .map_err(|_| invalid("invalid shutdown timeout"))
    })?;
    if timeout == 0 || timeout > 3600 {
        return Err(invalid("shutdown timeout must be 1..3600 seconds"));
    }
    let recheck = match app.recheck {
        Some(Recheck::Now) => Some("now".into()),
        Some(Recheck::NextRun) => Some("next-run".into()),
        None => env("PATCHULA_RECHECK")?,
    };
    patchula_server::serve(patchula_server::Options {
        settings,
        cache: serve
            .cache
            .clone()
            .or(env("PATCHULA_SERVER_CACHE")?.map(PathBuf::from))
            .unwrap_or_else(|| ".patchula-server.sqlite".into()),
        listen,
        app_id,
        private_key: secret(
            serve.private_key_file.as_ref(),
            "PATCHULA_APP_PRIVATE_KEY_FILE",
            "PATCHULA_APP_PRIVATE_KEY",
        )?,
        webhook_secret: secret(
            serve.webhook_secret_file.as_ref(),
            "PATCHULA_WEBHOOK_SECRET_FILE",
            "PATCHULA_WEBHOOK_SECRET",
        )?,
        recheck,
        shutdown_timeout: Duration::from_secs(timeout),
    })
    .await
    .map(|()| 0)
    .map_err(|e| (1, e))
}
