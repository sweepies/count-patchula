//! Flags/env are operator overrides; secret contents never enter the usage model.
use crate::cli::{App, Serve};
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
/// A secret from a file (`--flag` or the file's env var) or an env var holding
/// the value itself — never both. Reading shares the settings `Secret` rule:
/// strip trailing CR/LF, reject empty, and errors never carry the value. The
/// bytes are kept as they are: a webhook secret or key need not be UTF-8.
fn secret(path: Option<&PathBuf>, file_env: &str, value_env: &str) -> Result<Vec<u8>> {
    // Presence is checked on the raw variable: a secret need not be UTF-8.
    let path = path
        .cloned()
        .or_else(|| std::env::var_os(file_env).map(PathBuf::from));
    let value = std::env::var_os(value_env).is_some();
    if path.is_some() && value {
        return Err(invalid(
            "choose either a secret file or a secret environment value",
        ));
    }
    if path.is_none() && !value {
        return Err(invalid(
            "server requires App private key and webhook secret via env or files",
        ));
    }
    let secret = patchula_config::settings::Secret {
        env: path.is_none().then(|| value_env.to_owned()),
        file: path,
    };
    secret.read_bytes().map_err(|message| invalid(&message))
}
pub async fn run(app: &App, serve: &Serve) -> Result<u8> {
    let settings = patchula_config::settings::Settings::for_server(serve.settings.as_deref())
        .map_err(|e| (2, e))?;
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
        Some(recheck) => Some(recheck),
        None => env("PATCHULA_RECHECK")?
            .map(|value| {
                value
                    .parse::<patchula_words::Recheck>()
                    .map_err(|_| invalid("PATCHULA_RECHECK must be now or next-run"))
            })
            .transpose()?,
    };
    patchula_server::serve(patchula_server::Options {
        settings,
        cache: serve
            .cache
            .clone()
            .or(env("PATCHULA_SERVER_CACHE")?.map(PathBuf::from))
            .map(|dir| crate::cache_dir(Some(&dir)))
            .unwrap_or_else(|| crate::cache_dir(None).map(|dir| dir.join("server")))
            .map_err(|e| (2, e))?,
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
