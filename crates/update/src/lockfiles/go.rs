//! `go.sum` through `go get`/`go mod tidy` (or `go mod download`), with Go
//! from mise and credentials for Go and Git in `~/.netrc`, gone when the
//! run finishes.
use super::*;

pub(super) static DESCRIPTOR: Tool = Tool {
    steers: &[],
    tools: &["go"],
    versions: true,
    install: Install::Plain,
    refresh: Refresh::Manifests(&[Manager::Go]),
    output: Output::Default,
};

/// The Go to run: the newest of the pin and the edited `go.mod`'s `go` and
/// `toolchain` directives. With GOTOOLCHAIN=local, an older Go refuses a
/// module that requires a newer one, which an update itself may introduce.
pub(super) fn go_version(cwd: &Path, pinned: &str) -> Result<String, String> {
    use patchula_versioning::{Ecosystem as Scheme, Versioning};
    let text = std::fs::read_to_string(cwd.join("go.mod")).map_err(|e| e.to_string())?;
    let mut best = pinned.to_string();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let version = match (words.next(), words.next()) {
            (Some("go"), Some(v)) => v,
            (Some("toolchain"), Some(v)) => v.strip_prefix("go").unwrap_or(v),
            _ => continue,
        };
        // Only plain release versions select a toolchain.
        if !version.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
            continue;
        }
        if let (Ok(candidate), Ok(current)) =
            (Scheme::Loose.parse(version), Scheme::Loose.parse(&best))
            && Scheme::Loose.order(&candidate, &current).is_gt()
        {
            best = version.to_string();
        }
    }
    Ok(best)
}

impl Runner {
    /// Regenerate the go.sum of one directory: each targeted job's module
    /// moves with `go get`, then the module graph is tidied or downloaded.
    pub(super) async fn go_locks(
        &self,
        context: context::Context<'_>,
        jobs: &[&Job],
    ) -> Result<(), Error> {
        let context::Context {
            driver,
            root,
            directory,
            config,
            versions,
            ..
        } = context;
        let cwd = root.join(directory);
        let go = go_version(&cwd, &context::version(versions, "go")?)?;
        // Credentials for Go and Git in `~/.netrc`, gone when Go
        // finishes, and their hosts private to Go.
        let (netrc, hosts, entries) = self.netrc();
        let credentials = credentials::scopes(entries.iter().copied());
        let secrets = credentials::secrets(entries.iter().copied());
        let held = !hosts.is_empty();
        let mut _netrc = context::Private::default();
        let mut env = Vec::new();
        if held {
            let home = driver.join("home");
            std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
            _netrc.write(&home.join(".netrc"), &netrc)?;
            env.push(("GOPRIVATE".to_string(), hosts.join(",")));
        }
        let mut calls = Vec::new();
        for job in jobs {
            if let Some(package) = job.package() {
                calls.push(vec![
                    "-C".into(),
                    cwd.to_string_lossy().into_owned(),
                    "get".into(),
                    format!("{package}@{}", job.target().ok_or("missing Go target")?),
                ]);
            }
        }
        calls.push(vec![
            "-C".into(),
            cwd.to_string_lossy().into_owned(),
            "mod".into(),
            if config.lockfiles.tidy_go {
                "tidy".into()
            } else {
                "download".into()
            },
        ]);
        // What a Go run may fetch is bounded by GOPROXY, not
        // by file verdicts. In the `public` partition it is
        // `https://proxy.golang.org` alone — never `direct`,
        // which could fetch anonymously from a host only this
        // server's network reaches, such as a cluster-internal
        // Git host, and leave what it got in a cache every
        // installation shares; GOSUMDB keeps its default. A
        // run that fails there — an internal module the proxy
        // cannot serve — is rerun in the installation
        // partition with Go's environment as it was (GOPROXY
        // unset), so repositories with internal modules keep
        // working. A credentialed run goes straight to its
        // credential partition with that environment.
        let mut attempts = vec![(false, Vec::new())];
        if credentials.is_empty() {
            attempts.insert(
                0,
                (
                    true,
                    vec![("GOPROXY".into(), "https://proxy.golang.org".into())],
                ),
            );
        }
        for (public, extra) in attempts {
            let mut failed = None;
            for args in &calls {
                let mut run_env = env.clone();
                run_env.extend(extra.iter().cloned());
                let result = self
                    .held_tool(
                        driver,
                        Invocation {
                            descriptor: &DESCRIPTOR,
                            tools: vec![("go".into(), go.clone())],
                            bin: caches::Bin::Go,
                            cwd: None,
                            env: run_env,
                            credentials: credentials.clone(),
                            secrets: secrets.clone(),
                            public,
                            args: args.clone(),
                        },
                    )
                    .await;
                if let Err(error) = result {
                    failed = Some(error);
                    break;
                }
            }
            match failed {
                None => break,
                // The last attempt's error stands; only the
                // `public` attempt retries.
                Some(error) if !public => return Err(error),
                Some(_) => {}
            }
        }
        Ok(())
    }
}
