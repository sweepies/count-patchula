use crate::{Error, Result};
use std::{
    fs::File,
    io::Read,
    process::{Command, Stdio},
};

pub const RENOVATE_VERSION: &str = "44.155.6";

/// Install the executable with Bun, execute with mise's Node 24. Never inspect
/// installed source/config/fixtures. All clones, caches and raw logs are temporary.
pub fn run_renovate(repositories: &[String]) -> Result<String> {
    if repositories.is_empty()
        || repositories.iter().any(|repo| {
            let parts: Vec<_> = repo.split('/').collect();
            parts.len() != 2
                || parts.iter().any(|p| {
                    p.is_empty()
                        || !p
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                })
        })
    {
        return Err(Error::Input(
            "provide owner/repo arguments (without options)".into(),
        ));
    }
    if std::env::var("RENOVATE_TOKEN")
        .ok()
        .is_none_or(|token| token.trim().is_empty())
    {
        return Err(Error::Input(
            "live comparison requires RENOVATE_TOKEN from the environment".into(),
        ));
    }
    let root = tempfile::tempdir()?;
    let install = root.path().join("tool");
    std::fs::create_dir(&install)?;
    std::fs::write(
        install.join("package.json"),
        format!(r#"{{"private":true,"dependencies":{{"renovate":"{RENOVATE_VERSION}"}}}}"#),
    )?;
    let config = root.path().join("config.json");
    std::fs::write(&config, "{}")?;
    let result = Command::new("mise")
        .args(["exec", "--", "bun", "install", "--ignore-scripts"])
        .arg("--cwd")
        .arg(&install)
        .env_remove("RENOVATE_TOKEN")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !result.success() {
        return Err(Error::Input("Bun could not install the pinned Renovate executable; check mise install and registry access".into()));
    }
    let log_path = root.path().join("stdout.jsonl");
    let stderr_path = root.path().join("stderr.jsonl");
    let result = Command::new("mise")
        .args(["exec", "--", "node"])
        .arg(install.join("node_modules/renovate/dist/renovate.js"))
        .arg("--platform=github")
        .arg("--autodiscover=false")
        .arg("--onboarding=false")
        .args(repositories)
        .env("LOG_LEVEL", "debug")
        .env("LOG_FORMAT", "json")
        .env("RENOVATE_DRY_RUN", "lookup")
        .env("RENOVATE_BASE_DIR", root.path().join("work"))
        .env("RENOVATE_CACHE_DIR", root.path().join("cache"))
        .env("RENOVATE_CONFIG_FILE", config)
        .stdout(File::create(&log_path)?)
        .stderr(File::create(&stderr_path)?)
        .status()?;
    if !result.success() {
        return Err(Error::Input(format!(
            "Renovate {RENOVATE_VERSION} exited with {result}; raw output discarded (it may contain credentials). Capture a successful JSON lookup log elsewhere and use --renovate-log"
        )));
    }
    let mut logs = String::new();
    File::open(log_path)?.read_to_string(&mut logs)?;
    logs.push('\n');
    File::open(stderr_path)?.read_to_string(&mut logs)?;
    // TempDir drops before the caller receives normalized data, including errors.
    Ok(logs)
}
