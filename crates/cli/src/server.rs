//! Flags/env are operator overrides; secret contents never enter the usage model.
use crate::cli::Serve;
use std::{path::PathBuf, time::Duration};

type Result<T> = std::result::Result<T, (u8, miette::Report)>;
fn env(name: &str) -> Result<Option<String>> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(_) => Err(crate::usage("server environment value is not valid UTF-8")),
    }
}
/// A secret from a file (`--flag` or the file's env var) or an env var holding
/// the value itself — never both. Reading shares the settings `Secret` rule:
/// strip trailing CR/LF, reject empty, and errors never carry the value. The
/// bytes are kept as they are: a webhook secret or key need not be UTF-8.
fn secret(path: Option<&PathBuf>, file_env: &str, value_env: &str) -> Result<Vec<u8>> {
    secret_with(path, file_env, value_env, |name| std::env::var_os(name))
}
/// `secret` with the environment lookup passed in, so tests need not touch
/// the process environment.
fn secret_with(
    path: Option<&PathBuf>,
    file_env: &str,
    value_env: &str,
    env: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Result<Vec<u8>> {
    // Presence is checked on the raw variable: a secret need not be UTF-8.
    let path = path.cloned().or_else(|| env(file_env).map(PathBuf::from));
    let value = env(value_env).is_some();
    if path.is_some() && value {
        return Err(crate::usage(
            "choose either a secret file or a secret environment value",
        ));
    }
    if path.is_none() && !value {
        return Err(crate::usage(
            "server requires App private key and webhook secret via env or files",
        ));
    }
    let secret = match path {
        Some(path) => patchula_config::settings::Secret::File(path),
        None => patchula_config::settings::Secret::Env(value_env.to_owned()),
    };
    secret.read_bytes().map_err(crate::usage)
}
pub async fn run(serve: &Serve) -> Result<u8> {
    let settings = patchula_config::settings::Settings::for_server(serve.settings.as_deref())
        .map_err(|e| (crate::USAGE, e))?;
    let listen = serve.listen.map(Ok).unwrap_or_else(|| {
        env("PATCHULA_LISTEN")?
            .unwrap_or_else(|| "127.0.0.1:8080".into())
            .parse()
            .map_err(|_| crate::usage("invalid server listen address"))
    })?;
    let app_id = serve.app_id.map(Ok).unwrap_or_else(|| {
        env("PATCHULA_APP_ID")?
            .ok_or_else(|| crate::usage("server requires --app-id or PATCHULA_APP_ID"))?
            .parse()
            .map_err(|_| crate::usage("invalid App ID"))
    })?;
    let timeout = serve.shutdown_timeout_secs.map(Ok).unwrap_or_else(|| {
        env("PATCHULA_SHUTDOWN_TIMEOUT_SECS")?
            .unwrap_or_else(|| "60".into())
            .parse::<u64>()
            .map_err(|_| crate::usage("invalid shutdown timeout"))
    })?;
    if timeout == 0 || timeout > 3600 {
        return Err(crate::usage("shutdown timeout must be 1..3600 seconds"));
    }
    let recheck = match serve.recheck {
        Some(recheck) => Some(recheck),
        None => env("PATCHULA_RECHECK")?
            .map(|value| crate::parse_recheck(&value))
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
            .map_err(|e| (crate::USAGE, e))?,
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
    .map_err(|e| (crate::FAILED, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_file_keeps_non_utf8_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("key");
        std::fs::write(&key, [0xff, 0xfe, b'\n']).unwrap();
        assert_eq!(
            secret_with(Some(&key), "KEY_FILE", "KEY", |_| None).unwrap(),
            [0xff, 0xfe]
        );
    }

    #[test]
    fn a_secret_needs_a_file_or_a_value() {
        let (code, error) =
            secret_with(None, "PATCHULA_KEY_FILE", "PATCHULA_KEY", |_| None).unwrap_err();
        assert_eq!(code, 2);
        assert!(
            error
                .to_string()
                .contains("requires App private key and webhook secret")
        );
    }

    /// Flag validation runs before the secrets are read, so a `--recheck`
    /// flag is accepted and the run still stops on the missing secret files.
    #[tokio::test]
    async fn serve_takes_the_recheck_flag_before_reading_secrets() {
        let app = crate::cli::App::parse_from(
            &[
                "serve",
                "--settings",
                "/tmp/patchula-no-such-settings.pkl",
                "--listen",
                "127.0.0.1:0",
                "--app-id",
                "1",
                "--cache",
                "/tmp/patchula-no-such-cache",
                "--shutdown-timeout-secs",
                "1",
                "--private-key-file",
                "/tmp/patchula-no-such-key",
                "--webhook-secret-file",
                "/tmp/patchula-no-such-secret",
                "--recheck",
                "now",
            ]
            .map(std::ffi::OsStr::new),
        )
        .unwrap();
        let Some(crate::cli::Command::Serve(serve)) = &app.command else {
            panic!("serve")
        };
        let result = run(serve).await;
        assert!(matches!(result, Err((crate::USAGE, _))), "{result:?}");
    }
}
