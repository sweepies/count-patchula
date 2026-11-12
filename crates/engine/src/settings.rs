//! Operator settings to client options: the conversions that need
//! `patchula-http` and `patchula-github` types, kept out of `patchula-config`,
//! which stays plain data plus client-free validation. `check` runs the
//! settings rules those types own — registry prefixes, mirrors, host
//! concurrency — so `config check-settings`, server startup and reload, and
//! `Engine::from_settings` all reject the same files.
use patchula_config::settings::{RegistryAuth, Settings};
use patchula_github::Options as GithubOptions;
use patchula_http::{Auth, Credential, Options as HttpOptions};

/// The settings rules `patchula-config` cannot check without the HTTP
/// client's types: registry prefixes and their mirrors, the tools-prefix
/// rules, and `http.hostConcurrency`. An error is the settings field it is
/// about and the message, so a caller holding the file can point at it.
pub fn check(settings: &Settings) -> Result<(), (&'static str, &'static str)> {
    let mut prefixes = std::collections::BTreeSet::new();
    let mut mirrored = std::collections::BTreeSet::new();
    let mut entries = Vec::new();
    for (prefix, registry) in &settings.registries {
        let Some(normalised) = Credential::new(prefix, Auth::Withheld) else {
            return Err((
                "registries",
                "registries keys must be https URL prefixes without a user, query or fragment",
            ));
        };
        if !prefixes.insert(normalised.prefix().to_owned()) {
            return Err((
                "registries",
                "registries keys must differ by more than a trailing slash",
            ));
        }
        if let Some(public) = &registry.mirror_of {
            let Some(mirror) = normalised.clone().mirroring(public) else {
                return Err((
                    "mirrorOf",
                    "mirrorOf must be an https URL prefix without a user, query or fragment, other than the entry's own",
                ));
            };
            if !mirrored.insert(mirror.mirror_of().unwrap_or_default().to_owned()) {
                return Err(("mirrorOf", "mirrorOf must differ between registries"));
            }
        }
        // An `.npmrc` key ends at `=` and a comment starts at `;`.
        if registry.tools && normalised.prefix().contains(['=', ';']) {
            return Err(("tools", "tools entries need a prefix without = or ;"));
        }
        entries.push((normalised, registry.tools));
    }
    // npm, aube and Bun give a URL the credentials of the nearest
    // enclosing prefix they are told about, and are told only about tools
    // entries. An entry within a tools entry, which lookups would prefer
    // or keep from an installation, would get its credentials there.
    if entries.iter().any(|(outer, tools)| {
        *tools
            && entries
                .iter()
                .any(|(inner, _)| inner.prefix() != outer.prefix() && outer.encloses(inner))
    }) {
        return Err((
            "tools",
            "tools entries cannot hold other registries under their prefix",
        ));
    }
    http_options(settings).validate().map_err(|_| {
        (
            "http.hostConcurrency",
            "hostConcurrency needs lowercase hostnames and limits 1..100",
        )
    })?;
    Ok(())
}

/// The HTTP client settings.
pub fn http_options(settings: &Settings) -> HttpOptions {
    HttpOptions {
        default_host_concurrency: settings.http.default_host_concurrency,
        host_concurrency: settings.http.host_concurrency.clone(),
        registry_freshness_secs: settings.http.registry_freshness_secs,
        go_freshness_secs: settings.http.go_freshness_secs,
        advisory_freshness_secs: settings.http.advisory_freshness_secs,
        ..Default::default()
    }
}

/// The registry credentials `installation` may use, or every entry's for
/// the CLI, with their secrets read now. An entry the installation may not
/// use, or whose secret cannot be read, is withheld: its prefix still
/// claims its requests, which carry nothing, so a broader entry never
/// stands in. Unreadable secrets also come back by prefix and reason,
/// never the value.
pub fn credentials(
    settings: &Settings,
    installation: Option<u64>,
) -> (Vec<Credential>, Vec<(String, String)>) {
    let mut credentials = Vec::new();
    let mut failed = Vec::new();
    for (prefix, registry) in &settings.registries {
        // Settings built without `check` can hold a bad prefix: it is
        // reported, never a panic.
        let Some(withheld) = Credential::new(prefix, Auth::Withheld) else {
            failed.push((prefix.clone(), "not an https URL prefix".to_owned()));
            continue;
        };
        if let Some(id) = installation
            && !registry.allowed_installations.is_empty()
            && !registry.allowed_installations.contains(&id)
        {
            credentials.push(withheld);
            continue;
        }
        // A mirror whose secret cannot be read still stands in for its
        // public registry, without credentials: there is no fallback.
        let mirror = |entry: Credential| match &registry.mirror_of {
            Some(public) => entry.mirroring(public),
            None => Some(entry),
        };
        let auth = match (registry.auth, &registry.secret) {
            (None, _) => Ok(Auth::Anonymous),
            (Some(_), None) => Err("secret is missing".to_owned()),
            (Some(kind), Some(secret)) => secret.read().map(|secret| match kind {
                RegistryAuth::Basic => Auth::Basic {
                    username: registry.username.clone().unwrap_or_default(),
                    password: secret,
                },
                RegistryAuth::Bearer => Auth::Bearer { token: secret },
            }),
        };
        match auth.and_then(|auth| {
            Credential::new(prefix, auth)
                .map(|entry| {
                    if registry.tools {
                        entry.for_tools()
                    } else {
                        entry
                    }
                })
                .and_then(mirror)
                .ok_or_else(|| "secret cannot be sent in a header".to_owned())
        }) {
            Ok(credential) => credentials.push(credential),
            Err(reason) => {
                credentials.extend(mirror(withheld));
                failed.push((prefix.clone(), reason));
            }
        }
    }
    (credentials, failed)
}

/// The installation GitHub client settings. It keeps the client's own
/// lower default unless `http.hostConcurrency` sets api.github.com.
pub fn github_options(settings: &Settings) -> GithubOptions {
    let defaults = GithubOptions::default();
    GithubOptions {
        concurrency: settings
            .http
            .host_concurrency
            .get("api.github.com")
            .copied()
            .unwrap_or(defaults.concurrency),
        ..defaults
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A settings file is rejected when load fails or the engine-side rules
    /// fail; the two halves together are what `config check-settings` runs.
    fn rejected(path: &std::path::Path, body: &str) -> bool {
        std::fs::write(path, format!("amends \"PatchulaSettings.pkl\"\n{body}\n")).unwrap();
        match Settings::load(path) {
            Err(_) => true,
            Ok(settings) => check(&settings).is_err(),
        }
    }
    /// The field `check` names for a file that loads but breaks an
    /// engine-side rule, so callers can point at it in the source.
    fn field(path: &std::path::Path, body: &str) -> &'static str {
        std::fs::write(path, format!("amends \"PatchulaSettings.pkl\"\n{body}\n")).unwrap();
        let settings = Settings::load(path).unwrap();
        check(&settings).unwrap_err().0
    }

    #[test]
    fn http_settings_are_validated_where_the_client_options_are_built() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.pkl");
        std::fs::write(
            &path,
            "amends \"PatchulaSettings.pkl\"\nhttp {\n  hostConcurrency { [\"api.github.com\"] = 8 }\n  goFreshnessSecs = 0\n}\n",
        )
        .unwrap();
        let settings = Settings::load(&path).unwrap();
        let http = http_options(&settings);
        assert_eq!(http.host_limit("api.github.com"), 8);
        assert_eq!(http.host_limit("registry.npmjs.org"), 16);
        assert_eq!(http.go_freshness_secs, 0);
        assert!(check(&settings).is_ok());
        for bad in [
            "http { hostConcurrency { [\"API.github.com\"] = 8 } }",
            "http { hostConcurrency { [\"api.github.com\"] = 0 } }",
        ] {
            assert!(rejected(&path, bad), "{bad}");
        }
    }

    #[test]
    fn registry_entries_are_validated_and_their_secrets_read_per_installation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.pkl");
        let secret = dir.path().join("token");
        std::fs::write(&secret, "s3cret\n").unwrap();
        let body = format!(
            r#"registries {{
  ["https://acme.jfrog.io/artifactory/api/helm/charts"] {{
    auth = "basic"
    username = "ci"
    secret {{ file = "{}" }}
    allowedInstallations {{ 7 }}
  }}
  ["https://ghcr.io"] {{
    auth = "bearer"
    secret {{ env = "PATCHULA_TEST_UNSET_REGISTRY_SECRET" }}
  }}
}}"#,
            secret.display()
        );
        std::fs::write(&path, format!("amends \"PatchulaSettings.pkl\"\n{body}\n")).unwrap();
        let settings = Settings::load(&path).unwrap();
        let (cli, failed) = credentials(&settings, None);
        assert_eq!(cli.len(), 2);
        assert_eq!(cli[1].auth, Auth::Withheld);
        assert_eq!(
            cli[0].prefix(),
            "https://acme.jfrog.io/artifactory/api/helm/charts"
        );
        assert_eq!(
            cli[0].auth,
            Auth::Basic {
                username: "ci".into(),
                password: "s3cret".into()
            }
        );
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0, "https://ghcr.io");
        assert!(failed[0].1.contains("PATCHULA_TEST_UNSET_REGISTRY_SECRET"));
        assert!(matches!(
            credentials(&settings, Some(7)).0[0].auth,
            Auth::Basic { .. }
        ));
        // Another installation gets the prefix withheld, not a broader entry.
        assert!(
            credentials(&settings, Some(8))
                .0
                .iter()
                .all(|entry| entry.auth == Auth::Withheld)
        );
        for bad in [
            r#"registries { ["http://acme.jfrog.io"] { auth = "bearer"; secret { env = "T" } } }"#,
            r#"registries { ["https://acme.jfrog.io/?a=b"] { auth = "bearer"; secret { env = "T" } } }"#,
            r#"registries { ["https://acme.jfrog.io"] { auth = "basic"; secret { env = "T" } } }"#,
            r#"registries { ["https://acme.jfrog.io"] { auth = "bearer"; username = "ci"; secret { env = "T" } } }"#,
            r#"registries { ["https://acme.jfrog.io"] { auth = "bearer"; secret { } } }"#,
            r#"registries { ["https://acme.jfrog.io"] { auth = "bearer"; secret { env = "T"; file = "/t" } } }"#,
            r#"registries { ["https://acme.jfrog.io"] { auth = "token"; secret { env = "T" } } }"#,
            r#"registries { ["https://acme.jfrog.io"] { auth = "bearer"; secret { env = "T" } } ["https://acme.jfrog.io/"] { auth = "bearer"; secret { env = "U" } } }"#,
        ] {
            assert!(rejected(&path, &bad.replace("; ", "\n")), "{bad}");
        }
    }

    #[test]
    fn credentials_report_prefixes_and_secrets_they_cannot_use() {
        // `check` refuses these; `credentials` still runs on settings built
        // without it and reports rather than panics.
        let registry = |auth| patchula_config::settings::Registry {
            auth,
            username: None,
            secret: None,
            mirror_of: None,
            allowed_installations: Vec::new(),
            tools: false,
        };
        let mut settings = Settings::default();
        settings.registries.insert(
            "not-an-https-prefix".into(),
            registry(Some(RegistryAuth::Bearer)),
        );
        settings.registries.insert(
            "https://reg.example".into(),
            registry(Some(RegistryAuth::Bearer)),
        );
        let (entries, failed) = credentials(&settings, None);
        assert!(failed.iter().any(|(prefix, reason)| {
            prefix == "not-an-https-prefix" && reason == "not an https URL prefix"
        }));
        assert!(failed.iter().any(|(prefix, reason)| {
            prefix == "https://reg.example" && reason == "secret is missing"
        }));
        // The bad prefix claims nothing; the missing secret leaves its
        // prefix withheld rather than falling through to a broader entry.
        assert!(
            entries
                .iter()
                .all(|entry| entry.prefix() != "not-an-https-prefix")
        );
        assert!(entries.iter().any(|entry| entry.auth == Auth::Withheld));
    }

    #[test]
    fn mirrors_need_no_credentials_and_keep_standing_in_when_a_secret_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.pkl");
        let body = r#"registries {
  ["https://nexus.acme.dev/repository/npm-proxy"] {
    mirrorOf = "https://registry.npmjs.org"
  }
  ["https://acme.jfrog.io/api/pypi/pypi-remote/simple"] {
    auth = "bearer"
    secret { env = "PATCHULA_TEST_UNSET_MIRROR_SECRET" }
    mirrorOf = "https://pypi.org/simple"
  }
}"#;
        std::fs::write(&path, format!("amends \"PatchulaSettings.pkl\"\n{body}\n")).unwrap();
        let settings = Settings::load(&path).unwrap();
        let (entries, failed) = credentials(&settings, None);
        assert_eq!(failed.len(), 1);
        let [pypi, npm] = &entries[..] else {
            panic!("two entries")
        };
        assert_eq!(npm.auth, Auth::Anonymous);
        assert_eq!(npm.mirror_of(), Some("https://registry.npmjs.org"));
        assert_eq!(pypi.auth, Auth::Withheld);
        assert_eq!(pypi.mirror_of(), Some("https://pypi.org/simple"));
        for bad in [
            r#"registries { ["https://nexus.acme.dev"] { } }"#,
            r#"registries { ["https://nexus.acme.dev"] { auth = "bearer" } }"#,
            r#"registries { ["https://nexus.acme.dev"] { secret { env = "T" }; mirrorOf = "https://registry.npmjs.org" } }"#,
            r#"registries { ["https://nexus.acme.dev"] { mirrorOf = "https://nexus.acme.dev/" } }"#,
            r#"registries { ["https://nexus.acme.dev"] { mirrorOf = "http://registry.npmjs.org" } }"#,
            r#"registries { ["https://nexus.acme.dev/a"] { mirrorOf = "https://registry.npmjs.org" } ["https://nexus.acme.dev/b"] { mirrorOf = "https://registry.npmjs.org/" } }"#,
        ] {
            assert!(rejected(&path, &bad.replace("; ", "\n")), "{bad}");
        }
    }

    #[test]
    fn tools_take_an_entrys_credentials_only_when_it_allows_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.pkl");
        let token = dir.path().join("token");
        std::fs::write(&token, "t0ken\n").unwrap();
        let body = r#"registries {
  ["https://npm.acme.dev"] {
    auth = "bearer"
    secret { file = "TOKEN" }
    tools = true
  }
  ["https://npm.other.dev"] {
    auth = "bearer"
    secret { file = "TOKEN" }
  }
}"#
        .replace("TOKEN", &token.to_string_lossy());
        std::fs::write(&path, format!("amends \"PatchulaSettings.pkl\"\n{body}\n")).unwrap();
        let settings = Settings::load(&path).unwrap();
        let (entries, _) = credentials(&settings, None);
        let tools: Vec<_> = entries
            .iter()
            .filter(|e| e.tools())
            .map(|e| e.prefix())
            .collect();
        assert_eq!(tools, ["https://npm.acme.dev"]);
        for bad in [
            r#"registries { ["https://nexus.acme.dev"] { mirrorOf = "https://registry.npmjs.org"; tools = true } }"#,
            r#"registries { ["https://nexus.acme.dev"] { auth = "bearer"; secret { env = "T" }; mirrorOf = "https://registry.npmjs.org"; tools = true } }"#,
            // An entry under a tools entry, for another installation or not.
            r#"registries { ["https://nexus.acme.dev"] { auth = "bearer"; secret { env = "T" }; tools = true }; ["https://nexus.acme.dev/repository/b"] { auth = "bearer"; secret { env = "U" }; allowedInstallations { 2 } } }"#,
            r#"registries { ["https://nexus.acme.dev"] { auth = "bearer"; secret { env = "T" }; tools = true }; ["https://nexus.acme.dev/npm-proxy"] { mirrorOf = "https://registry.npmjs.org" } }"#,
            r#"registries { ["https://nexus.acme.dev/a=b"] { auth = "bearer"; secret { env = "T" }; tools = true } }"#,
        ] {
            assert!(rejected(&path, &bad.replace("; ", "\n")), "{bad}");
        }
    }

    #[test]
    fn check_names_the_field_each_rule_is_about() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.pkl");
        // A registry prefix that is not an https URL prefix.
        assert_eq!(
            field(
                &path,
                r#"registries { ["http://acme.jfrog.io"] { auth = "bearer"; secret { env = "T" } } }"#
            ),
            "registries"
        );
        // Two keys that normalise to the same prefix.
        assert_eq!(
            field(
                &path,
                r#"registries { ["https://acme.jfrog.io"] { auth = "bearer"; secret { env = "T" } } ["https://acme.jfrog.io/"] { auth = "bearer"; secret { env = "U" } } }"#
            ),
            "registries"
        );
        // A mirror that is not an https URL prefix.
        assert_eq!(
            field(
                &path,
                r#"registries { ["https://nexus.acme.dev"] { mirrorOf = "http://registry.npmjs.org" } }"#
            ),
            "mirrorOf"
        );
        // Two entries mirroring the same public registry.
        assert_eq!(
            field(
                &path,
                r#"registries { ["https://nexus.acme.dev/a"] { mirrorOf = "https://registry.npmjs.org" } ["https://nexus.acme.dev/b"] { mirrorOf = "https://registry.npmjs.org/" } }"#
            ),
            "mirrorOf"
        );
        // A tools prefix an .npmrc key cannot carry.
        assert_eq!(
            field(
                &path,
                r#"registries { ["https://nexus.acme.dev/a=b"] { auth = "bearer"; secret { env = "T" }; tools = true } }"#
            ),
            "tools"
        );
        // A registry nested under a tools entry.
        assert_eq!(
            field(
                &path,
                r#"registries { ["https://nexus.acme.dev"] { auth = "bearer"; secret { env = "T" }; tools = true } ["https://nexus.acme.dev/npm-proxy"] { mirrorOf = "https://registry.npmjs.org" } }"#
            ),
            "tools"
        );
        // Host concurrency the HTTP client refuses.
        assert_eq!(
            field(
                &path,
                r#"http { hostConcurrency { ["API.github.com"] = 8 } }"#
            ),
            "http.hostConcurrency"
        );
        assert_eq!(
            field(
                &path,
                r#"http { hostConcurrency { ["api.github.com"] = 0 } }"#
            ),
            "http.hostConcurrency"
        );
    }
}
