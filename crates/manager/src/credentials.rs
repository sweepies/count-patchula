//! Credentials committed to the repository, refused as hygiene: a
//! repository must not carry its own registry credentials — they belong
//! in the operator's `registries` settings. It is not the safety boundary
//! for the shared `public` cache partition: a run reaches
//! `public` only when the extraction decided every file it reads is
//! public-only, so a credential a text scan would miss cannot smuggle a
//! private fetch there either way.
//!
//! The checks are the settings keys each tool documents for credentials,
//! plus a URL carrying a user and password in the fields that name a
//! registry, index or source URL, read from the parsed values so escapes
//! are decoded. Everything else — CI workflows, scripts, environment
//! values, git dependency URLs, checksums — is out of scope: no check
//! refuses a repository for what its tools never read as a credential.
//!
//! Everything returned names what was found, never the value.

/// Whether a file's text is checked for committed credentials.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CredentialCheck {
    Scan,
    /// Its format can hold nothing but names, versions and hashes.
    Skip,
    /// Nobody decided, so it is scanned. Every file type the lockfile runner
    /// materializes must have a decision, which a test in `patchula-update`
    /// walks the materialized list to enforce.
    #[default]
    Undecided,
}

/// Why `text` committed at `path` refuses the repository, if it does: the
/// kind of credential found, such as "a `_authToken` in .npmrc" or "a
/// password in a registry URL". Never the credential itself.
pub fn committed_credentials(path: &str, text: &str) -> Option<String> {
    if crate::roles::credential_check(path) == CredentialCheck::Skip {
        return None;
    }
    keyed(path, text)
}

/// The report reason for a file `committed_credentials` refused.
pub fn credential_reason(path: &str, kind: &str) -> String {
    format!(
        "{path} holds a credential in repository config ({kind}); move it to the operator's registries settings"
    )
}

/// The credential check a file's text gets, which the file-role taxonomy
/// assigns: `eco_roles` gives a file one exactly when it may hold a
/// committed credential, so `keyed` cannot drift from the taxonomy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CredentialScan {
    /// `.npmrc`: auth keys and registry URLs.
    Npmrc,
    /// `pnpm-workspace.yaml`: auth keys.
    PnpmWorkspace,
    /// `.yarnrc.yml`: auth keys and registry server URLs.
    Yarnrc,
    /// `composer.json`: auth settings and repository URLs.
    Composer,
    /// `poetry.toml`: auth settings.
    Poetry,
    /// `pyproject.toml` or `uv.toml`: index URLs.
    Python,
    /// `pip.conf` or `pip.ini`: index URLs.
    Pip,
    /// `Gemfile`: source URLs.
    Gemfile,
    /// `nuget.config`: credentials and source URLs.
    Nuget,
    /// A Cargo configuration: registry tokens and index URLs.
    Cargo,
}

impl CredentialScan {
    /// Run the check on `text`; an unparseable file is left to the tool.
    fn check(self, path: &str, text: &str) -> Option<String> {
        match self {
            Self::Npmrc => npmrc(text).or_else(|| npmrc_urls(text)),
            Self::PnpmWorkspace => pnpm_workspace(text),
            Self::Yarnrc => yarnrc(text).or_else(|| yarnrc_urls(text)),
            Self::Composer => composer(text).or_else(|| composer_urls(text)),
            Self::Poetry => poetry(text),
            Self::Python => poetry(text).or_else(|| python_urls(path, text)),
            Self::Pip => pip_urls(text),
            Self::Gemfile => gemfile_urls(text),
            Self::Nuget => nuget(text).or_else(|| nuget_urls(text)),
            Self::Cargo => cargo(text).or_else(|| cargo_urls(text)),
        }
    }
}

/// The credential kinds the settings file at `path` can hold, checked with
/// the parser of its format. An unparseable file is left to the tool.
fn keyed(path: &str, text: &str) -> Option<String> {
    patchula_words::Manager::ALL
        .iter()
        .find_map(|manager| crate::roles::eco_roles(*manager, path).credential_scan)
        .and_then(|scan| scan.check(path, text))
}

/// Whether `url` carries a committed password: a userinfo whose password
/// is neither empty nor a `$VAR`, `${VAR}`, `${{ expression }}` or
/// `%VAR%` reference, which the environment or CI resolves at run time.
/// A user alone is not a secret (`https://user@bitbucket.org` names an
/// account, nothing more), and neither is an empty password.
fn passworded(url: &str) -> bool {
    let url = url.trim().trim_matches(['"', '\'']);
    let Some(authority) = url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split(['/', '?', '#']).next())
    else {
        return false;
    };
    let Some((userinfo, _)) = authority.rsplit_once('@') else {
        return false;
    };
    userinfo
        .split(':')
        .nth(1)
        .is_some_and(|password| !password.is_empty() && !password.starts_with(['$', '%']))
}

/// Whether any of `urls` carries a committed password.
fn credentialed(urls: &[&str]) -> Option<String> {
    urls.iter()
        .any(|url| passworded(url))
        .then(|| "a password in a registry URL".to_string())
}

/// `.npmrc` auth lines with a literal value: `//host/:_authToken=`,
/// `:_auth=` or `:_password=`. `${VAR}` references stay allowed: the
/// value lives in the environment, not the file. `username` and `email`
/// are contact details, not credentials, and an empty value sets nothing.
fn npmrc(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        // The key npm reads is after the last ':', as in `//host/:_authToken`.
        let key = key
            .rsplit(':')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let value = value.trim().trim_matches(['"', '\'']);
        if matches!(key.as_str(), "_authtoken" | "_auth" | "_password")
            && !value.is_empty()
            && !value.contains("${")
        {
            return Some(format!("an {key} credential in .npmrc"));
        }
    }
    None
}

/// `.npmrc` `registry` and `@scope:registry` values carrying a committed
/// password.
fn npmrc_urls(text: &str) -> Option<String> {
    let urls: Vec<&str> = text
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with(['#', ';']) {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            (key == "registry"
                || key
                    .strip_suffix(":registry")
                    .is_some_and(|scope| scope.starts_with('@') && scope.len() > 1))
            .then(|| value.trim().trim_matches(['"', '\'']))
        })
        .collect();
    credentialed(&urls)
}

/// `pnpm-workspace.yaml` auth keys, anywhere in the file, with the same
/// rules as `.npmrc`: literal values refuse, `${VAR}` references, empty
/// values and `username`/`email` stay allowed.
fn pnpm_workspace(text: &str) -> Option<String> {
    let mut pairs = Vec::new();
    yaml_pairs(text, &mut pairs)?;
    for (key, value) in pairs {
        let key = key.to_ascii_lowercase();
        let name = if key.ends_with("_authtoken") {
            "_authtoken"
        } else if key.ends_with("_auth") {
            "_auth"
        } else if key.ends_with("_password") {
            "_password"
        } else {
            continue;
        };
        if literal(&value) && value.as_str().is_none_or(|v| !v.is_empty()) {
            return Some(format!("a {name} credential in pnpm-workspace.yaml"));
        }
    }
    None
}

/// `npmAuthToken` or `npmAuthIdent` anywhere Yarn reads them: top level,
/// under `npmScopes` or `npmRegistries`. `${VAR}` references stay allowed.
fn yarnrc(text: &str) -> Option<String> {
    let mut pairs = Vec::new();
    yaml_pairs(text, &mut pairs)?;
    pairs.into_iter().find_map(|(key, value)| {
        (matches!(key.as_str(), "npmAuthToken" | "npmAuthIdent") && literal(&value))
            .then(|| format!("a {key} credential in .yarnrc.yml"))
    })
}

/// `.yarnrc.yml` `npmRegistryServer` values carrying a committed password.
fn yarnrc_urls(text: &str) -> Option<String> {
    let mut pairs = Vec::new();
    yaml_pairs(text, &mut pairs)?;
    let urls: Vec<&str> = pairs
        .iter()
        .filter(|(key, _)| key == "npmRegistryServer")
        .filter_map(|(_, value)| value.as_str())
        .collect();
    credentialed(&urls)
}

/// The auth settings Composer documents for `config` in `composer.json`,
/// refused when set to anything non-empty.
fn composer(text: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(text).ok()?;
    let config = value.get("config")?;
    [
        "http-basic",
        "bearer",
        "github-oauth",
        "gitlab-oauth",
        "gitlab-token",
        "bitbucket-oauth",
        "forgejo-oauth",
        "forgejo-token",
        "gitea-oauth",
        "gitea-token",
    ]
    .iter()
    .find_map(|key| {
        let value = config.get(key)?;
        let empty = value.is_null()
            || value.as_str().is_some_and(str::is_empty)
            || value.as_object().is_some_and(|o| o.is_empty())
            || value.as_array().is_some_and(|a| a.is_empty());
        (!empty).then(|| format!("a Composer {key} credential"))
    })
}

/// `repositories` URLs in `composer.json` carrying a committed password.
fn composer_urls(text: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(text).ok()?;
    let repositories = value.get("repositories")?;
    let entries: Vec<&serde_json::Value> = match repositories {
        serde_json::Value::Array(entries) => entries.iter().collect(),
        serde_json::Value::Object(entries) => entries.values().collect(),
        _ => Vec::new(),
    };
    let urls: Vec<&str> = entries
        .iter()
        .filter_map(|entry| entry.get("url").and_then(|url| url.as_str()))
        .collect();
    credentialed(&urls)
}

/// `http-basic` or `pypi-token` settings with literal values, as
/// `poetry config` writes them into `poetry.toml`.
fn poetry(text: &str) -> Option<String> {
    fn valued(item: &toml_edit::Item) -> bool {
        if let Some(table) = item.as_table_like() {
            table.iter().any(|(_, value)| valued(value))
        } else if let Some(array) = item.as_array() {
            array
                .iter()
                .any(|value| value.as_str().is_some_and(|v| !v.is_empty()))
        } else {
            item.as_str().is_some_and(|value| !value.is_empty())
        }
    }
    let document = text.parse::<toml_edit::DocumentMut>().ok()?;
    ["http-basic", "pypi-token"].iter().find_map(|key| {
        document
            .get(key)
            .filter(|item| valued(item))
            .map(|_| format!("a Poetry {key} credential"))
    })
}

/// The index and source URLs `pyproject.toml` or `uv.toml` name, checked
/// for committed passwords.
fn python_urls(path: &str, text: &str) -> Option<String> {
    let leaf = crate::roles::leaf(path);
    let document = text.parse::<toml_edit::DocumentMut>().ok()?;
    let mut urls: Vec<String> = Vec::new();
    let mut collect = |item: Option<&toml_edit::Item>| {
        if let Some(value) = item.and_then(toml_edit::Item::as_str) {
            urls.push(value.to_owned());
        } else if let Some(array) = item.and_then(toml_edit::Item::as_array) {
            urls.extend(
                array
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_owned)),
            );
        }
    };
    let uv = if leaf == "uv.toml" {
        Some(document.as_item())
    } else {
        document.get("tool").and_then(|tool| tool.get("uv"))
    };
    if let Some(uv) = uv {
        collect(uv.get("index-url"));
        collect(uv.get("extra-index-url"));
        collect(uv.get("find-links"));
        if let Some(index) = uv
            .get("index")
            .and_then(toml_edit::Item::as_array_of_tables)
        {
            for entry in index {
                collect(entry.get("url"));
            }
        }
        if let Some(pip) = uv.get("pip") {
            collect(pip.get("index-url"));
            collect(pip.get("extra-index-url"));
            collect(pip.get("find-links"));
        }
    }
    if leaf == "pyproject.toml"
        && let Some(sources) = document
            .get("tool")
            .and_then(|tool| tool.get("poetry"))
            .and_then(|poetry| poetry.get("source"))
    {
        let tables: Vec<&dyn toml_edit::TableLike> =
            if let Some(tables) = sources.as_array_of_tables() {
                tables
                    .iter()
                    .map(|t| t as &dyn toml_edit::TableLike)
                    .collect()
            } else if let Some(array) = sources.as_array() {
                array
                    .iter()
                    .filter_map(|value| value.as_inline_table())
                    .map(|t| t as &dyn toml_edit::TableLike)
                    .collect()
            } else {
                Vec::new()
            };
        for source in tables {
            collect(source.get("url"));
        }
    }
    let refs: Vec<&str> = urls.iter().map(String::as_str).collect();
    credentialed(&refs)
}

/// `index-url`, `extra-index-url` and `find-links` settings in
/// `pip.conf`/`pip.ini` carrying a committed password.
fn pip_urls(text: &str) -> Option<String> {
    let urls: Vec<&str> = text
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with(['#', ';']) {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            matches!(
                key.trim().to_ascii_lowercase().as_str(),
                "index-url" | "extra-index-url" | "find-links"
            )
            .then(|| value.trim())
        })
        .collect();
    credentialed(&urls)
}

/// `Gemfile` `source` lines carrying a committed password, read as
/// Bundler's DSL reads them so escapes never hide a value.
fn gemfile_urls(text: &str) -> Option<String> {
    let urls = crate::bundler::source_urls(text);
    let refs: Vec<&str> = urls.iter().map(String::as_str).collect();
    credentialed(&refs)
}

/// A `nuget.config` with `packageSourceCredentials` `Password` or
/// `ClearTextPassword` entries. `%VAR%` references stay allowed: the
/// value lives in the environment. Element and attribute names compare
/// without case, as NuGet reads them.
fn nuget(text: &str) -> Option<String> {
    let document = roxmltree::Document::parse(text).ok()?;
    let named = |node: roxmltree::Node, name: &str| {
        node.is_element() && node.tag_name().name().eq_ignore_ascii_case(name)
    };
    let attribute = |node: roxmltree::Node, name: &str| {
        node.attributes()
            .find(|a| a.name().eq_ignore_ascii_case(name))
            .map(|a| a.value().to_owned())
    };
    for credentials in document
        .descendants()
        .filter(|node| named(*node, "packageSourceCredentials"))
    {
        for add in credentials.descendants().filter(|node| named(*node, "add")) {
            let key = attribute(add, "key").unwrap_or_default();
            let literal = attribute(add, "value")
                .is_some_and(|value| !value.is_empty() && !value.starts_with('%'));
            let name = if key.eq_ignore_ascii_case("password") {
                "Password"
            } else if key.eq_ignore_ascii_case("cleartextpassword") {
                "ClearTextPassword"
            } else {
                continue;
            };
            if literal {
                return Some(format!("a NuGet {name} credential"));
            }
        }
    }
    None
}

/// `packageSources` URLs in `nuget.config` carrying a committed password.
fn nuget_urls(text: &str) -> Option<String> {
    let document = roxmltree::Document::parse(text).ok()?;
    let urls: Vec<String> = document
        .descendants()
        .filter(|node| {
            node.is_element()
                && node
                    .tag_name()
                    .name()
                    .eq_ignore_ascii_case("packageSources")
        })
        .flat_map(|sources| sources.descendants())
        .filter(|node| node.is_element() && node.tag_name().name().eq_ignore_ascii_case("add"))
        .filter_map(|add| {
            add.attributes()
                .find(|a| a.name().eq_ignore_ascii_case("value"))
                .map(|a| a.value().to_owned())
        })
        .collect();
    let refs: Vec<&str> = urls.iter().map(String::as_str).collect();
    credentialed(&refs)
}

/// A `token` under `[registry]` or `[registries.*]` in `.cargo/config.toml`
/// or `.cargo/config`.
fn cargo(text: &str) -> Option<String> {
    let document = text.parse::<toml_edit::DocumentMut>().ok()?;
    let token = |table: &dyn toml_edit::TableLike| {
        table
            .get("token")
            .and_then(toml_edit::Item::as_str)
            .is_some_and(|token| !token.is_empty())
    };
    let registry_token = document
        .get("registry")
        .and_then(toml_edit::Item::as_table_like)
        .is_some_and(token);
    let registries_token = document
        .get("registries")
        .and_then(toml_edit::Item::as_table_like)
        .is_some_and(|registries| {
            registries
                .iter()
                .any(|(_, registry)| registry.as_table_like().is_some_and(token))
        });
    (registry_token || registries_token).then(|| "a Cargo registry token".to_string())
}

/// `index` URLs under `[registries.*]` carrying a committed password.
fn cargo_urls(text: &str) -> Option<String> {
    let document = text.parse::<toml_edit::DocumentMut>().ok()?;
    let registries = document
        .get("registries")
        .and_then(toml_edit::Item::as_table_like)?;
    let urls: Vec<String> = registries
        .iter()
        .filter_map(|(_, registry)| registry.get("index"))
        .filter_map(|index| index.as_str().map(str::to_owned))
        .collect();
    let refs: Vec<&str> = urls.iter().map(String::as_str).collect();
    credentialed(&refs)
}

/// Every key-value pair in a YAML document, recursively, or None when it
/// does not parse.
fn yaml_pairs(text: &str, pairs: &mut Vec<(String, serde_yaml_ng::Value)>) -> Option<()> {
    fn walk(value: &serde_yaml_ng::Value, pairs: &mut Vec<(String, serde_yaml_ng::Value)>) {
        match value {
            serde_yaml_ng::Value::Mapping(map) => {
                for (key, value) in map {
                    if let Some(key) = key.as_str() {
                        pairs.push((key.to_owned(), value.clone()));
                    }
                    walk(value, pairs);
                }
            }
            serde_yaml_ng::Value::Sequence(items) => items.iter().for_each(|v| walk(v, pairs)),
            _ => {}
        }
    }
    let document = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(text).ok()?;
    walk(&document, pairs);
    Some(())
}

/// A literal scalar value, which `${VAR}` references are not.
fn literal(value: &serde_yaml_ng::Value) -> bool {
    !matches!(
        value,
        serde_yaml_ng::Value::Null
            | serde_yaml_ng::Value::Mapping(_)
            | serde_yaml_ng::Value::Sequence(_)
    ) && !value.as_str().is_some_and(|v| v.contains("${"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roles::credential_scope;

    fn refused(path: &str, text: &str) -> Option<String> {
        committed_credentials(path, text)
    }

    /// The refusal a file in `credential_scope` with `text` produces.
    fn scoped_refusal(path: &str, text: &str) -> Option<String> {
        credential_scope(path)
            .then(|| committed_credentials(path, text))
            .flatten()
    }

    #[test]
    fn registry_url_fields_refuse_passwords_but_not_users() {
        // Credentials are built at runtime so the fixture scan finds
        // none written in the source.
        let pw = "p".to_owned() + "w";
        for (path, text, expect) in [
            (
                ".npmrc",
                format!("registry=https://ci:{pw}@npm.acme.dev/\n"),
                true,
            ),
            (".npmrc", "@acme:registry=https://npm.acme.dev/\n".into(), false),
            (
                ".npmrc",
                format!("@acme:registry=https://ci:{pw}@npm.acme.dev/\n"),
                true,
            ),
            (
                ".npmrc",
                "registry=https://ci:${TOKEN}@npm.acme.dev/\n".into(),
                false,
            ),
            (
                ".yarnrc.yml",
                format!("npmRegistryServer: \"https://ci:{pw}@npm.acme.dev/\"\n"),
                true,
            ),
            (
                ".yarnrc.yml",
                "npmRegistryServer: \"https://npm.acme.dev/\"\n".into(),
                false,
            ),
            (
                "composer.json",
                format!(
                    "{{\"repositories\":[{{\"type\":\"composer\",\"url\":\"https://ci:{pw}@packagist.acme.dev\"}}]}}"
                ),
                true,
            ),
            (
                "composer.json",
                r#"{"repositories":[{"type":"vcs","url":"https://ci@git.acme.dev/r"}]}"#.into(),
                false,
            ),
            (
                "nuget.config",
                format!("<configuration><packageSources><add key=\"feed\" value=\"https://ci:{pw}@nuget.acme.dev/v3/index.json\"/></packageSources></configuration>"),
                true,
            ),
            (
                "nuget.config",
                "<configuration><packageSources><add key=\"feed\" value=\"https://nuget.acme.dev/v3/index.json\"/></packageSources></configuration>".into(),
                false,
            ),
            (
                "pyproject.toml",
                format!("[[tool.poetry.source]]\nname = \"acme\"\nurl = \"https://ci:{pw}@pypi.acme.dev/simple\"\n"),
                true,
            ),
            (
                "pyproject.toml",
                "[tool.uv]\nindex-url = \"https://pypi.org/simple\"\n".into(),
                false,
            ),
            (
                "uv.toml",
                format!("index-url = \"https://ci:{pw}@pypi.acme.dev/simple\"\n"),
                true,
            ),
            (
                "pip.conf",
                format!("[global]\nindex-url = https://ci:{pw}@pypi.acme.dev/simple\n"),
                true,
            ),
            (
                "Gemfile",
                format!("source \"https://ci:{pw}@gems.acme.dev\"\ngem \"rails\"\n"),
                true,
            ),
            (
                "Gemfile",
                "source \"https://rubygems.org\"\ngem \"rails\"\n".into(),
                false,
            ),
            (
                ".cargo/config.toml",
                format!("[registries.acme]\nindex = \"sparse+https://ci:{pw}@index.acme.dev/\"\n"),
                true,
            ),
            (
                ".cargo/config.toml",
                "[registries.acme]\nindex = \"sparse+https://index.acme.dev/\"\n".into(),
                false,
            ),
            // An escaped password is still a password once parsed.
            (
                "composer.json",
                format!(
                    "{{\"repositories\":[{{\"type\":\"composer\",\"url\":\"https://ci:{}@packagist.acme.dev\"}}]}}",
                    "p%40".to_owned() + "ss"
                ),
                true,
            ),
        ] {
            assert_eq!(refused(path, &text).is_some(), expect, "{path}: {text:?}");
        }
    }

    #[test]
    fn non_registry_fields_are_never_refused() {
        // Test DSNs, user-only userinfo and CI files carry nothing a tool
        // reads as a registry credential; their passwords are built at
        // runtime so the fixture scan finds none written.
        let pass = "pa".to_owned() + "ss";
        for (path, text) in [
            (
                "package.json",
                format!(
                    "{{\"scripts\":{{\"test\":\"postgres://user:{pass}@localhost/db mocha\"}}}}"
                ),
            ),
            (
                "pyproject.toml",
                format!(
                    "[tool.pytest.ini_options]\nenv = [\"DATABASE_URL=postgres://user:{pass}@localhost/db\"]\n"
                ),
            ),
            (
                "composer.json",
                format!(
                    "{{\"scripts\":{{\"test\":\"postgres://user:{pass}@localhost/db phpunit\"}}}}"
                ),
            ),
            (".npmrc", "cache=https://user@bitbucket.org\n".into()),
            (
                "x/.github/workflows/ci.yml",
                format!("run: postgres://user:{pass}@localhost/db cargo test"),
            ),
        ] {
            assert_eq!(refused(path, &text), None, "{path}: {text:?}");
        }
        // And the scan is narrower still: none of these files is in scope.
        for path in [
            "package.json",
            ".github/workflows/ci.yml",
            ".forgejo/workflows/integration.yml",
            ".gitlab-ci.yml",
            "Dockerfile",
            "compose.yaml",
            "chart/values.yaml",
            "main.tf",
            "README.md",
            "requirements.txt",
            "docs/dev-requirements.in",
            ".bundle/config",
            "go.mod",
            "go.work",
            "mise.toml",
            ".mise.toml",
            "package-lock.json",
            "pnpm-lock.yaml",
            "yarn.lock",
            "bun.lock",
            "aube-lock.yaml",
            "npm-shrinkwrap.json",
            "Cargo.toml",
            "Cargo.lock",
            "Gemfile.lock",
            "composer.lock",
            "uv.lock",
            "poetry.lock",
            "Directory.Packages.props",
            "app/app.csproj",
        ] {
            assert!(!credential_scope(path), "{path} should be out of scope");
        }
    }

    #[test]
    fn npmrc_refuses_literal_auth_but_allows_env_references() {
        for (line, expect) in [
            ("//npm.acme.dev/:_authToken=t0ken", true),
            ("//npm.acme.dev/:_auth=e30=", true),
            ("//npm.acme.dev/:_password=pw", true),
            ("_authToken=t0ken", true),
            ("//npm.acme.dev/:_authToken=${NPM_TOKEN}", false),
            ("//npm.acme.dev/:_auth=${BASE64}", false),
            ("_password=${PW}", false),
            ("registry=https://npm.acme.dev/", false),
            ("@acme:registry=https://npm.acme.dev/scoped", false),
            ("always-auth=true", false),
            ("certfile=/run/cert.pem", false),
        ] {
            let file = format!("{line}\n");
            assert_eq!(refused(".npmrc", &file).is_some(), expect, "{line}");
            assert_eq!(refused("app/.npmrc", &file).is_some(), expect, "{line}");
        }
        // `username` and `email` beside an environment `_password` are
        // contact details, not credentials; empty values set nothing.
        for text in [
            "//npm.acme.dev/:username=ci\n//npm.acme.dev/:_password=${PW}\n",
            "username=ci\n_password=${PW}\n",
            "//npm.acme.dev/:email=ci@acme.dev\n//npm.acme.dev/:_password=${PW}\n",
            "username=ci\nemail=ci@acme.dev\n",
            "_authToken=\n",
            "_authToken=\"\"\n",
        ] {
            assert!(refused(".npmrc", text).is_none(), "{text:?}");
        }
    }

    #[test]
    fn pnpm_workspace_refuses_auth_keys() {
        let cases = [
            ("_authToken: t0ken\n", true),
            ("registry:_auth: e30=\n", true),
            ("_authToken: ${NPM_TOKEN}\n", false),
            ("catalog:\n  zod: 3.23.8\n", false),
            ("username: ci\n_password: ${PW}\n", false),
            ("packages:\n  - app\n", false),
        ];
        for (text, expect) in cases {
            assert_eq!(
                refused("pnpm-workspace.yaml", text).is_some(),
                expect,
                "{text:?}"
            );
        }
    }

    #[test]
    fn yarnrc_refuses_literal_auth_at_any_depth() {
        let cases = [
            ("npmAuthToken: t0ken\n", true),
            ("npmAuthIdent: ci:pw\n", true),
            ("npmAuthToken: \"${NPM_TOKEN}\"\n", false),
            ("npmScopes:\n  acme:\n    npmAuthToken: t0ken\n", true),
            (
                "npmRegistries:\n  https://npm.acme.dev:\n    npmAuthIdent: ci:pw\n",
                true,
            ),
            ("npmRegistryServer: https://npm.acme.dev\n", false),
            (
                "npmAuthToken: \"${NPM_TOKEN}\"\nnpmScopes:\n  acme:\n    npmRegistryServer: https://npm.acme.dev\n",
                false,
            ),
        ];
        for (text, expect) in cases {
            assert_eq!(refused(".yarnrc.yml", text).is_some(), expect, "{text:?}");
        }
    }

    #[test]
    fn composer_json_refuses_config_auth() {
        let cases = [
            (
                r#"{"config":{"http-basic":{"npm.acme.dev":{"username":"ci","password":"pw"}}}}"#,
                true,
            ),
            (r#"{"config":{"bearer":{"npm.acme.dev":"t0ken"}}}"#, true),
            (
                r#"{"config":{"github-oauth":{"github.com":"t0ken"}}}"#,
                true,
            ),
            (
                r#"{"config":{"gitlab-oauth":{"gitlab.com":"t0ken"}}}"#,
                true,
            ),
            (
                r#"{"config":{"gitlab-token":{"gitlab.com":"t0ken"}}}"#,
                true,
            ),
            (
                r#"{"config":{"forgejo-token":{"forgejo.acme.dev":"t0ken"}}}"#,
                true,
            ),
            (
                r#"{"config":{"bitbucket-oauth":{"bitbucket.org":{"consumer-key":"k","consumer-secret":"s"}}}}"#,
                true,
            ),
            (r#"{"config":{"http-basic":{}}}"#, false),
            (r#"{"config":{"http-basic":""}}"#, false),
            (
                r#"{"repositories":[{"type":"composer","url":"https://packagist.acme.dev"}]}"#,
                false,
            ),
        ];
        for (text, expect) in cases {
            assert_eq!(refused("composer.json", text).is_some(), expect, "{text}");
        }
    }

    #[test]
    fn nuget_config_refuses_literal_source_credentials() {
        let cases = [
            (
                "<configuration><packageSourceCredentials><feed><add key=\"Username\" value=\"ci\"/><add key=\"ClearTextPassword\" value=\"pw\"/></feed></packageSourceCredentials></configuration>",
                true,
            ),
            (
                "<configuration><packageSourceCredentials><feed><add key=\"Password\" value=\"pw\"/></feed></packageSourceCredentials></configuration>",
                true,
            ),
            // An environment reference resolves at run time.
            (
                "<configuration><packageSourceCredentials><feed><add key=\"Username\" value=\"ci\"/><add key=\"Password\" value=\"%NUGET_PASSWORD%\"/></feed></packageSourceCredentials></configuration>",
                false,
            ),
            (
                "<configuration><packageSourceCredentials><feed><add key=\"Username\" value=\"ci\"/></feed></packageSourceCredentials></configuration>",
                false,
            ),
            (
                "<configuration><packageSources><add key=\"feed\" value=\"https://nuget.acme.dev/v3/index.json\"/></packageSources></configuration>",
                false,
            ),
            ("not xml at all", false),
        ];
        for (text, expect) in cases {
            for path in ["nuget.config", "NuGet.Config", "app/NUGET.CONFIG"] {
                assert_eq!(refused(path, text).is_some(), expect, "{path}: {text}");
            }
        }
    }

    #[test]
    fn poetry_and_pyproject_refuse_http_basic_and_pypi_token() {
        let cases = [
            (
                "[http-basic.acme]\nusername = \"ci\"\npassword = \"pw\"\n",
                true,
            ),
            ("[pypi-token]\npypi = \"t0ken\"\n", true),
            ("[http-basic]\n", false),
            ("[virtualenvs]\ncreate = true\n", false),
            ("not toml [[[", false),
        ];
        for (text, expect) in cases {
            for path in ["poetry.toml", "pyproject.toml"] {
                assert_eq!(refused(path, text).is_some(), expect, "{path}: {text:?}");
            }
        }
    }

    #[test]
    fn cargo_config_refuses_registry_tokens() {
        let cases = [
            ("[registry]\ntoken = \"t0ken\"\n", true),
            (
                "[registries.acme]\nindex = \"sparse+https://index.acme.dev/\"\ntoken = \"t0ken\"\n",
                true,
            ),
            (
                "[registries.acme]\nindex = \"sparse+https://index.acme.dev/\"\n",
                false,
            ),
            (
                "[registries.acme]\ncredential-provider = \"cargo:token\"\n",
                false,
            ),
        ];
        for (text, expect) in cases {
            for path in [
                ".cargo/config.toml",
                ".cargo/config",
                "app/.cargo/config.toml",
            ] {
                assert_eq!(refused(path, text).is_some(), expect, "{path}: {text:?}");
            }
        }
    }

    #[test]
    fn checksum_files_are_never_scanned() {
        let text = format!(
            "module https://ci:{}@proxy.acme.dev/x h1:abc=\n",
            "p".to_owned() + "w"
        );
        for path in [
            "go.sum",
            "go.work.sum",
            ".terraform.lock.hcl",
            "packages.lock.json",
        ] {
            assert!(refused(path, &text).is_none(), "{path}");
        }
    }

    /// The repository scan checks only the settings files whose keys a
    /// tool or lookup reads as credentials.
    #[test]
    fn only_credential_settings_are_scanned() {
        let url = format!(
            "https://{}:{}@registry.acme.test/x",
            "ci",
            "s3".to_owned() + "cret"
        );
        for path in [
            ".npmrc",
            ".yarnrc.yml",
            "pnpm-workspace.yaml",
            "pyproject.toml",
            "uv.toml",
            "poetry.toml",
            "pip.conf",
            "app/pip.ini",
            "composer.json",
            "nuget.config",
            "NuGet.Config",
            "Gemfile",
            ".cargo/config.toml",
        ] {
            assert!(credential_scope(path), "{path} should be in scope");
        }
        // A credentialed URL in a field that names a registry refuses.
        assert!(scoped_refusal(".npmrc", &format!("registry={url}\n")).is_some());
        // The same URL in a non-registry field does not.
        assert_eq!(
            scoped_refusal(
                "composer.json",
                &format!("{{\"scripts\":{{\"t\":\"{url}\"}}}}")
            ),
            None
        );
    }
}
