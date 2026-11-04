//! Credentials committed to the repository. A repository that holds its own
//! registry credentials is refused outright: its packages could come from a
//! registry meant for another installation, and lockfile tools would fetch
//! them into the shared `public` cache partition. The files a lockfile tool
//! or a package-registry lookup reads — [`credential_scope`] — are scanned
//! for a URL carrying a user or password, and the settings files each tool
//! reads credentials from are checked against that tool's own documented
//! settings. The checks fail closed; a file of a structured format that
//! does not parse is refused only when the URL rule also matches, and is
//! otherwise left to the tool.
//!
//! Everything returned names what was found, never the value.

/// Whether a committed credential in `path` could steer a package tool or a
/// registry lookup: `path`'s format is a registry settings file, a package
/// manifest or a lockfile a lockfile tool or lookup reads. Everything else —
/// CI workflows, Dockerfiles, Compose, Kubernetes and Helm manifests,
/// Terraform — is out of scope: no tool the Count runs reads it and no
/// lookup follows it, so a credential there is the repository's own
/// business. The engine scans only these files; the lockfile runner's
/// backstop scans every file it materializes, since those are exactly what
/// its tools read.
pub fn credential_scope(path: &str) -> bool {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    crate::nuget::config_file(path)
        || crate::nuget::msbuild_file(path)
        || crate::cargo::config_file(path)
        || crate::mise::config_file(path)
        || crate::go::work_file(path)
        || crate::bundle_config(path)
        || matches!(
            leaf,
            // Node registry settings, manifests and lockfiles.
            ".npmrc"
                | ".yarnrc.yml"
                | "pnpm-workspace.yaml"
                | "package.json"
                | "package-lock.json"
                | "npm-shrinkwrap.json"
                | "aube-lock.yaml"
                | "pnpm-lock.yaml"
                | "yarn.lock"
                | "bun.lock"
                // Python settings and lockfiles. pip requirements files are
                // not read for index URLs, so they stay out.
                | "pyproject.toml"
                | "uv.toml"
                | "poetry.toml"
                | "pip.conf"
                | "pip.ini"
                | "uv.lock"
                | "poetry.lock"
                // Composer.
                | "composer.json"
                | "composer.lock"
                // Bundler.
                | "Gemfile"
                | "Gemfile.lock"
                // Cargo.
                | "Cargo.toml"
                | "Cargo.lock"
                // Go.
                | "go.mod"
        )
}

/// Whether `path`'s text is checked for committed credentials, as a Boolean:
/// `true` scans it, `false` skips it because its format can hold nothing
/// but names, versions and hashes. `None` means nobody decided; files like
/// that are scanned anyway. Every file type the lockfile runner
/// materializes must have a decision, which a test in `patchula-update`
/// walks the materialized list to enforce.
pub fn credential_check(path: &str) -> Option<bool> {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    Some(match leaf {
        "go.sum"
        | "go.work.sum"
        | ".terraform.lock.hcl"
        | "packages.lock.json"
        | ".python-version"
        | ".tool-versions" => false,
        "package.json"
        | "package-lock.json"
        | "npm-shrinkwrap.json"
        | "aube-lock.yaml"
        | "pnpm-lock.yaml"
        | "yarn.lock"
        | "bun.lock"
        | "Cargo.toml"
        | "Cargo.lock"
        | "go.mod"
        | "go.work"
        | "mise.lock"
        | "mise.toml"
        | ".mise.toml"
        | "pyproject.toml"
        | "gradle-wrapper.properties"
        | "Gemfile"
        | "Gemfile.lock"
        | "Directory.Packages.props"
        | "Directory.Build.props"
        | "Directory.Build.targets"
        | "dotnet-tools.json"
        | "global.json"
        | "uv.lock"
        | "poetry.lock"
        | "composer.json"
        | "composer.lock"
        | "flake.lock"
        | "flake.nix"
        | "apm.lock.yaml"
        | "apm.yml"
        | "pnpm-workspace.yaml"
        | ".yarnrc.yml"
        | ".npmrc"
        | "uv.toml"
        | "poetry.toml" => true,
        _ if leaf.eq_ignore_ascii_case("nuget.config")
            || crate::cargo::config_file(path)
            || crate::bundle_config(path)
            || [
                ".tf", ".go", ".rs", ".csproj", ".fsproj", ".vbproj", ".sln", ".slnx", ".gemspec",
            ]
            .iter()
            .any(|extension| path.ends_with(extension)) =>
        {
            true
        }
        _ => return None,
    })
}

/// Why `text` committed at `path` refuses the repository, if it does: the
/// kind of credential found, such as "a URL with a password in it" or "an
/// `_authToken` in .npmrc". Never the credential itself.
pub fn committed_credentials(path: &str, text: &str) -> Option<String> {
    if credential_check(path) == Some(false) {
        return None;
    }
    keyed(path, text).or_else(|| credentialed_url(text))
}

/// The report reason for a file `committed_credentials` refused.
pub fn credential_reason(path: &str, kind: &str) -> String {
    format!(
        "{path} holds a credential in repository config ({kind}); move it to the operator's registries settings"
    )
}

/// The credential kinds the settings file at `path` can hold, checked with
/// the parser of its format. An unparseable file is left to the URL rule.
fn keyed(path: &str, text: &str) -> Option<String> {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    match leaf {
        ".npmrc" => npmrc(text),
        "pnpm-workspace.yaml" => pnpm_workspace(text),
        ".yarnrc.yml" => yarnrc(text),
        "composer.json" => composer(text),
        "poetry.toml" | "pyproject.toml" => poetry(text),
        _ if leaf.eq_ignore_ascii_case("nuget.config") => nuget(text),
        _ if crate::cargo::config_file(path) => cargo(text),
        _ if crate::bundle_config(path) => bundler(text),
        _ => None,
    }
}

/// A URL in `text` whose authority carries a user or a password, in the
/// order they appear. A password refuses in any scheme, a user alone in any
/// scheme but SSH's: `ssh://git@host` names a service account, not a secret.
/// A `$VAR`, `${VAR}` or `${{ expression }}` userinfo is resolved by the
/// environment or CI at run time and stays allowed for the same reason.
fn credentialed_url(text: &str) -> Option<String> {
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let scheme_start = rest[..at]
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.'))
            .map_or(0, |i| i + 1);
        let scheme = &rest[scheme_start..at];
        rest = &rest[at + 3..];
        // The authority ends at the path, query or fragment, or a delimiter.
        let authority = rest
            .split(|c: char| {
                c.is_whitespace()
                    || matches!(
                        c,
                        '/' | '?'
                            | '#'
                            | '"'
                            | '\''
                            | '<'
                            | '>'
                            | '\\'
                            | '`'
                            | '|'
                            | '('
                            | ')'
                            | '['
                            | ']'
                            | '{'
                            | '}'
                    )
            })
            .next()
            .unwrap_or_default();
        if !scheme.starts_with(|c: char| c.is_ascii_alphabetic()) {
            continue;
        }
        // The user ends at the last `@`; earlier ones belong to it.
        let Some((userinfo, _)) = authority.rsplit_once('@') else {
            continue;
        };
        if userinfo.contains(':') {
            // A `$VAR`, `${VAR}` or `${{ expression }}` placeholder is
            // resolved by the environment or CI at run time; the credential
            // is not committed.
            if userinfo
                .split(':')
                .nth(1)
                .is_some_and(|password| password.starts_with('$'))
            {
                continue;
            }
            return Some("a URL with a password in it".into());
        }
        if userinfo.starts_with('$') {
            continue;
        }
        let ssh =
            scheme.eq_ignore_ascii_case("ssh") || scheme.to_ascii_lowercase().ends_with("+ssh");
        if !ssh {
            return Some("a URL with a user in it".into());
        }
    }
    None
}

/// `.npmrc` auth lines with a literal value: `//host/:_authToken=`,
/// `:_auth=`, `:_password=`, or `username`/`email` paired with a `_password`
/// key. `${VAR}` references stay allowed: the value lives in the
/// environment, not the file.
fn npmrc(text: &str) -> Option<String> {
    let mut password = false;
    let mut user = false;
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
        let literal = !value.contains("${");
        match key.as_str() {
            "_authtoken" | "_auth" | "_password" if literal => {
                return Some(format!("an {key} credential in .npmrc"));
            }
            "_password" => password = true,
            "username" | "email" if literal => user = true,
            _ => {}
        }
    }
    // `npm adduser` records username and email beside a `_password` scoped
    // to the registry; the pair refuses whatever form the password takes.
    (password && user).then(|| "a username in .npmrc beside a _password".to_string())
}

/// `pnpm-workspace.yaml` auth keys, anywhere in the file, with the same
/// rules as `.npmrc`.
fn pnpm_workspace(text: &str) -> Option<String> {
    let mut pairs = Vec::new();
    yaml_pairs(text, &mut pairs)?;
    let mut password = false;
    let mut user = false;
    for (key, value) in pairs {
        let key = key.to_ascii_lowercase();
        let literal = literal(&value);
        if key.ends_with("_authtoken") || key.ends_with("_auth") {
            if literal {
                return Some(format!("an {key} credential in pnpm-workspace.yaml"));
            }
        } else if key.ends_with("_password") {
            if literal {
                return Some(format!("a {key} credential in pnpm-workspace.yaml"));
            }
            password = true;
        } else if matches!(key.as_str(), "username" | "email") && literal {
            user = true;
        }
    }
    (password && user).then(|| "a username in pnpm-workspace.yaml beside a _password".to_string())
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

/// A `nuget.config` with `packageSourceCredentials` `Password` or
/// `ClearTextPassword` entries, or `apikeys` entries. Element and attribute
/// names compare without case, as NuGet reads them.
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
            if key.eq_ignore_ascii_case("password") || key.eq_ignore_ascii_case("cleartextpassword")
            {
                return Some(format!("a NuGet {key} credential"));
            }
        }
    }
    if document
        .descendants()
        .filter(|node| named(*node, "apikeys"))
        .any(|keys| keys.descendants().any(|node| named(node, "add")))
    {
        return Some("a NuGet API key".into());
    }
    None
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

/// A `BUNDLE_<HOST>` credential key in `.bundle/config`: Bundler folds host
/// dots and dashes into `__` and `___`, so a `BUNDLE_*__*` key holds
/// `user:password` for the host it names. The Count never reads the file —
/// it runs Bundler with `BUNDLE_APP_CONFIG` elsewhere — but the engine
/// reads it for its sources and a committed one is refused all the same.
fn bundler(text: &str) -> Option<String> {
    let value = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(text).ok()?;
    value
        .as_mapping()?
        .iter()
        .filter_map(|(key, value)| Some((key.as_str()?, value)))
        .find_map(|(key, value)| {
            (key.starts_with("BUNDLE_")
                && key.contains("__")
                && value.as_str().is_some_and(|v| !v.is_empty()))
            .then(|| "a Bundler credential key".to_string())
        })
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
    fn urls_with_a_user_or_password_are_refused() {
        for (text, expect) in [
            ("resolved \"https://ci:s3cret@npm.acme.dev/x.tgz\"\n", true), // betterleaks:allow
            ("tarball = \"https://ci@npm.acme.dev/x.tgz\"\n", true),
            ("remote: https://gems.acme.dev\n", false),
            ("source \"https://user:pw@gems.acme.dev\"\n", true), // betterleaks:allow
            ("index = \"sparse+https://user:pw@index.acme.dev/\"\n", true), // betterleaks:allow
            ("url = \"git+https://user:pw@git.acme.dev/r.git\"\n", true), // betterleaks:allow
            ("url = \"registry+https://user@index.acme.dev/\"\n", true), // betterleaks:allow
            // SSH users are service accounts, not secrets.
            ("url = \"ssh://git@git.acme.dev/r.git\"\n", false),
            ("url = \"git+ssh://git@git.acme.dev/r.git\"\n", false),
            // Any password in any scheme is refused, SSH included.
            ("url = \"ssh://git:pw@git.acme.dev/r.git\"\n", true), // betterleaks:allow
            (
                "source \"ssh://git@git.acme.dev/r.git\" # git@ is fine\n",
                false,
            ),
            // An `@` in the path, query or fragment is not userinfo.
            (
                "resolved \"https://npm.acme.dev/@scope/x/-/x-1.0.0.tgz\"\n",
                false,
            ),
            (
                "source = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
                false,
            ),
            // SCP-style Git addresses carry no scheme.
            ("dep = \"git@git.acme.dev:org/repo.git\"\n", false),
            // An environment or CI placeholder is not a committed credential.
            (
                "clone = \"https://gitlab-bot:$DEPLOY_TOKEN@gitlab.com/acme/x.git\"\n",
                false,
            ),
            (
                "url = \"https://x:${{ secrets.TOKEN }}@git.acme.dev/x\"\n",
                false,
            ),
            ("url = \"https://$API_TOKEN@npm.acme.dev/x.tgz\"\n", false),
            // A command substitution reads its secret at run time too.
            (
                "url = \"https://ci:$(cat /run/secrets/token)@npm.acme.dev/x\"\n",
                false,
            ),
            ("url = \"https://ci:pa$ss@npm.acme.dev/x.tgz\"\n", true),
        ] {
            for path in [
                "yarn.lock",
                "Gemfile.lock",
                "package.json",
                "go.mod",
                "x.rs",
            ] {
                assert_eq!(refused(path, text).is_some(), expect, "{path}: {text:?}");
            }
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
        // A literal username or email paired with a _password refuses, even
        // when the password is an environment reference.
        for pair in [
            "//npm.acme.dev/:username=ci\n//npm.acme.dev/:_password=${PW}\n",
            "username=ci\n_password=${PW}\n",
            "//npm.acme.dev/:email=ci@acme.dev\n//npm.acme.dev/:_password=${PW}\n",
        ] {
            assert!(refused(".npmrc", pair).is_some(), "{pair:?}");
        }
        // Without a _password they are only contact fields.
        assert!(refused(".npmrc", "username=ci\nemail=ci@acme.dev\n").is_none());
    }

    #[test]
    fn pnpm_workspace_refuses_auth_keys() {
        let cases = [
            ("_authToken: t0ken\n", true),
            ("registry:_auth: e30=\n", true),
            ("_authToken: ${NPM_TOKEN}\n", false),
            ("catalog:\n  zod: 3.23.8\n", false),
            ("username: ci\n_password: ${PW}\n", true),
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
                r#"{"config":{"bitbucket-oauth":{"bitbucket.org":{"consumer-key":"k","consumer-secret":"s"}}}}"#,
                true,
            ),
            (r#"{"config":{"http-basic":{}}}"#, false),
            (r#"{"config":{"http-basic":""}}"#, false),
            (
                r#"{"repositories":[{"type":"composer","url":"https://packagist.acme.dev"}]}"#,
                false,
            ),
            (
                r#"{"repositories":[{"type":"vcs","url":"https://ci:pw@git.acme.dev/r"}]}"#, // betterleaks:allow
                true,
            ),
        ];
        for (text, expect) in cases {
            assert_eq!(refused("composer.json", text).is_some(), expect, "{text}");
        }
    }

    #[test]
    fn nuget_config_refuses_source_credentials_and_apikeys() {
        let cases = [
            (
                "<configuration><packageSourceCredentials><feed><add key=\"Username\" value=\"ci\"/><add key=\"ClearTextPassword\" value=\"pw\"/></feed></packageSourceCredentials></configuration>",
                true,
            ),
            (
                "<configuration><packageSourceCredentials><feed><add key=\"Password\" value=\"pw\"/></feed></packageSourceCredentials></configuration>",
                true,
            ),
            (
                "<configuration><packageSourceCredentials><feed><add key=\"Username\" value=\"ci\"/></feed></packageSourceCredentials></configuration>",
                false,
            ),
            (
                "<configuration><apikeys><add key=\"https://nuget.acme.dev\" value=\"key\"/></apikeys></configuration>",
                true,
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
        // Index URLs answer to the URL rule instead.
        let index = "[[tool.poetry.source]]\nname = \"acme\"\nurl = \"https://ci:pw@pypi.acme.dev/simple\"\n"; // betterleaks:allow
        assert!(refused("pyproject.toml", index).is_some());
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
    fn bundle_config_refuses_host_credential_keys() {
        let cases = [
            ("BUNDLE_GEMS__ACME__DEV: ci:pw\n", true),
            ("BUNDLE_PATH: vendor/bundle\n", false),
            ("BUNDLE_GEM__MIT: true\n", false),
        ];
        for (text, expect) in cases {
            for path in [".bundle/config", "app/.bundle/config"] {
                assert_eq!(refused(path, text).is_some(), expect, "{path}: {text:?}");
            }
        }
    }

    #[test]
    fn checksum_files_are_never_scanned() {
        let text = "module https://ci:pw@proxy.acme.dev/x h1:abc=\n"; // betterleaks:allow
        for path in [
            "go.sum",
            "go.work.sum",
            ".terraform.lock.hcl",
            "packages.lock.json",
        ] {
            assert!(refused(path, text).is_none(), "{path}");
        }
    }

    /// The repository scan checks only files a lockfile tool or a
    /// package-registry lookup reads. A credentialed URL anywhere else — a
    /// CI workflow, a Dockerfile, a Helm values file — is the repository's
    /// own business.
    #[test]
    fn only_tool_and_lookup_files_are_scanned() {
        // Built at runtime so no credential-shaped string sits in the tree.
        let url = format!(
            "https://{}:{}@registry.acme.test/x",
            "ci",
            "s3".to_owned() + "cret"
        );
        for path in [
            ".github/workflows/ci.yml",
            ".forgejo/workflows/integration.yml",
            ".gitlab-ci.yml",
            "Dockerfile",
            "compose.yaml",
            "chart/values.yaml",
            "chart/templates/deployment.yaml",
            "main.tf",
            "README.md",
            // pip requirements files are not read for index URLs.
            "requirements.txt",
            "docs/dev-requirements.in",
        ] {
            assert!(!credential_scope(path), "{path} should be out of scope");
            assert_eq!(scoped_refusal(path, &url), None, "{path}: {url}");
        }
        for path in [
            ".npmrc",
            ".yarnrc.yml",
            "pnpm-workspace.yaml",
            "package.json",
            "package-lock.json",
            "npm-shrinkwrap.json",
            "aube-lock.yaml",
            "pnpm-lock.yaml",
            "yarn.lock",
            "bun.lock",
            "pyproject.toml",
            "uv.toml",
            "poetry.toml",
            "pip.conf",
            "app/pip.ini",
            "uv.lock",
            "poetry.lock",
            "composer.json",
            "composer.lock",
            "nuget.config",
            "NuGet.Config",
            "Directory.Packages.props",
            "app/app.csproj",
            "Gemfile",
            "Gemfile.lock",
            ".bundle/config",
            "Cargo.toml",
            "Cargo.lock",
            ".cargo/config.toml",
            "go.mod",
            "go.work",
            "mise.toml",
            ".mise.toml",
        ] {
            assert!(credential_scope(path), "{path} should be in scope");
            assert!(scoped_refusal(path, &url).is_some(), "{path}: {url}");
        }
    }
}
