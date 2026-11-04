//! Operator registry credentials for lockfile tools, from entries that set
//! `tools = true`, in the form each tool reads them. npm, aube, Bun and Yarn
//! get theirs from `node`.
//!
//! Bundler, Composer, Go, uv and Terraform match credentials by host rather
//! than by prefix, so they only get an entry whose prefix is a bare host,
//! with no path or port, and that is the only entry on its host: a path would
//! widen to the whole host, and another entry there, for any installation,
//! would have its URLs given this one's credentials. Cargo, NuGet and Poetry
//! match them by the name the repository gives a registry, so a name gets
//! the entry its URL falls under, as a lookup would.
use super::Runner;
use patchula_http::{Auth, Credential};
use std::collections::BTreeMap;

impl Runner {
    /// Tools entries a host-matching tool can use.
    fn by_host(&self) -> impl Iterator<Item = &Credential> {
        self.registries.iter().filter(|entry| {
            let (host, port) = entry.host();
            port.is_none()
                && entry
                    .prefix()
                    .strip_prefix("https://")
                    .is_some_and(|rest| !rest.contains('/'))
                && self
                    .entries
                    .iter()
                    .filter(|other| other.host().0 == host)
                    .count()
                    == 1
        })
    }

    /// `COMPOSER_AUTH`: `http-basic` and `bearer` credentials by host.
    pub(super) fn composer_auth(&self) -> Option<String> {
        let mut basic = serde_json::Map::new();
        let mut bearer = serde_json::Map::new();
        for entry in self.by_host() {
            let host = entry.host().0.to_owned();
            match &entry.auth {
                Auth::Basic { username, password } => {
                    basic.insert(
                        host,
                        serde_json::json!({ "username": username, "password": password }),
                    );
                }
                Auth::Bearer { token } => {
                    bearer.insert(host, token.clone().into());
                }
                _ => {}
            }
        }
        (!basic.is_empty() || !bearer.is_empty())
            .then(|| serde_json::json!({ "http-basic": basic, "bearer": bearer }).to_string())
    }

    /// Terraform's `TF_TOKEN_<host>` variables, for bearer entries: dots in
    /// the host become `_` and hyphens `__`. A host with `_` of its own is
    /// left out, as its variable could name another host.
    pub(super) fn terraform_env(&self) -> Vec<(String, String)> {
        self.by_host()
            .filter(|entry| !entry.host().0.contains('_'))
            .filter_map(|entry| match &entry.auth {
                Auth::Bearer { token } => Some((
                    format!(
                        "TF_TOKEN_{}",
                        entry.host().0.replace('-', "__").replace('.', "_")
                    ),
                    token.clone(),
                )),
                _ => None,
            })
            .collect()
    }

    /// `.netrc` for basic entries, read by Go, Git and uv, and the hosts it
    /// covers. `.netrc` separates fields by whitespace, and readers differ on
    /// quotes, backslashes and `#`, so a user or password holding any is
    /// left out.
    pub(super) fn netrc(&self) -> (String, Vec<String>) {
        let mut netrc = String::new();
        let mut hosts = Vec::new();
        for entry in self.by_host() {
            let Auth::Basic { username, password } = &entry.auth else {
                continue;
            };
            if [username, password].iter().any(|field| {
                field
                    .chars()
                    .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | '#'))
            }) {
                continue;
            }
            let host = entry.host().0;
            netrc.push_str(&format!(
                "machine {host} login {username} password {password}\n"
            ));
            hosts.push(host.to_owned());
        }
        (netrc, hosts)
    }

    /// Bundler's `BUNDLE_<HOST>` variables for basic entries, `user:password`:
    /// the host in uppercase, with dots as `__` and hyphens as `___`. A host
    /// with `_` of its own is left out, as its variable could name another
    /// host.
    pub(super) fn bundler_env(&self) -> Vec<(String, String)> {
        self.by_host()
            .filter(|entry| !entry.host().0.contains('_'))
            .filter_map(|entry| match &entry.auth {
                Auth::Basic { username, password } => Some((
                    format!(
                        "BUNDLE_{}",
                        entry
                            .host()
                            .0
                            .to_ascii_uppercase()
                            .replace('-', "___")
                            .replace('.', "__")
                    ),
                    format!("{username}:{password}"),
                )),
                _ => None,
            })
            .collect()
    }

    /// Each name `definitions` gives a URL under a tools entry, by the
    /// variable fragment `key` makes of it, with the definition and the
    /// entry. `url` takes the https URL out of a definition, which counts only
    /// when it is written as it parses: tools read the text as written and
    /// could read a backslash, escape or case differently and send the
    /// credentials to another host. A name defined more than one way, or two
    /// names with the same fragment, get nothing.
    fn named<'a>(
        &'a self,
        definitions: &'a [BTreeMap<String, String>],
        key: impl Fn(&str) -> Option<String>,
        url: impl Fn(&str) -> Option<&str>,
    ) -> Vec<(String, &'a str, &'a Credential)> {
        let mut by_key: BTreeMap<String, Vec<(&str, Option<&Credential>)>> = BTreeMap::new();
        for (name, definition) in definitions.iter().flatten() {
            let Some(fragment) = key(name) else {
                continue;
            };
            let entry = url(definition)
                .filter(|url| {
                    patchula_http::url_prefix(url).as_deref() == Some(url.trim_end_matches('/'))
                })
                .and_then(|url| patchula_http::entry_for(&self.entries, url))
                .filter(|entry| self.registries.contains(entry));
            by_key
                .entry(fragment)
                .or_default()
                .push((definition, entry));
        }
        by_key
            .into_iter()
            .filter_map(|(fragment, all)| {
                let (definition, entry) = *all.first()?;
                let entry = entry?;
                if all.iter().any(|other| *other != (definition, Some(entry))) {
                    return None;
                }
                Some((fragment, definition, entry))
            })
            .collect()
    }

    /// Cargo's `CARGO_REGISTRIES_<NAME>_INDEX` and `_TOKEN` variables for
    /// the registries `.cargo/config.toml` files define, by name and sparse
    /// index URL. Cargo sends the token as the `Authorization` header, so it
    /// is the header a lookup sends: `Bearer <token>` or `Basic <base64>`.
    pub(super) fn cargo_env(
        &self,
        registries: &[BTreeMap<String, String>],
    ) -> Vec<(String, String)> {
        self.named(
            registries,
            |name| plain(name, &['-', '_']).then(|| name.to_ascii_uppercase().replace('-', "_")),
            |index| index.strip_prefix("sparse+"),
        )
        .into_iter()
        .filter_map(|(name, index, entry)| {
            let header = entry.auth.header()?.to_str().ok()?.to_owned();
            Some([
                (format!("CARGO_REGISTRIES_{name}_INDEX"), index.to_owned()),
                (format!("CARGO_REGISTRIES_{name}_TOKEN"), header),
            ])
        })
        .flatten()
        .collect()
    }

    /// NuGet's `NuGetPackageSourceCredentials_<key>` variables for the
    /// basic entries the `packageSources` of `nuget.config` files fall under,
    /// by source key. The value separates fields by `;`, so a user or
    /// password holding `;` is left out.
    pub(super) fn nuget_env(&self, sources: &[BTreeMap<String, String>]) -> Vec<(String, String)> {
        self.named(
            sources,
            |name| plain(name, &['-', '_', '.']).then(|| name.to_owned()),
            |url: &str| Some(url),
        )
        .into_iter()
        .filter_map(|(name, _, entry)| match &entry.auth {
            Auth::Basic { username, password }
                if !username.contains(';') && !password.contains(';') =>
            {
                Some((
                    format!("NuGetPackageSourceCredentials_{name}"),
                    format!(
                        "Username={username};Password={password};ValidAuthenticationTypes=basic"
                    ),
                ))
            }
            _ => None,
        })
        .collect()
    }

    /// Poetry's `POETRY_HTTP_BASIC_<NAME>_USERNAME` and `_PASSWORD`
    /// variables for the basic entries the `[[tool.poetry.source]]` URLs of
    /// `pyproject.toml` fall under: the name in uppercase, with `-` and `.` as
    /// `_`.
    pub(super) fn poetry_env(&self, sources: &[BTreeMap<String, String>]) -> Vec<(String, String)> {
        self.named(
            sources,
            |name| {
                plain(name, &['-', '_', '.'])
                    .then(|| name.to_ascii_uppercase().replace(['-', '.'], "_"))
            },
            |url: &str| Some(url),
        )
        .into_iter()
        .filter_map(|(name, _, entry)| match &entry.auth {
            Auth::Basic { username, password } => Some([
                (
                    format!("POETRY_HTTP_BASIC_{name}_USERNAME"),
                    username.clone(),
                ),
                (
                    format!("POETRY_HTTP_BASIC_{name}_PASSWORD"),
                    password.clone(),
                ),
            ]),
            _ => None,
        })
        .flatten()
        .collect()
    }
}

/// Whether `name` is ASCII letters, digits and `extra` alone.
fn plain(name: &str, extra: &[char]) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(&c))
}

/// The sources a `nuget.config` adds, by key and URL. A file NuGet could
/// not read adds none.
pub(super) fn nuget_sources(text: &str) -> BTreeMap<String, String> {
    let Ok(document) = roxmltree::Document::parse(text) else {
        return BTreeMap::new();
    };
    document
        .root_element()
        .children()
        .filter(|node| node.has_tag_name("packageSources"))
        .flat_map(|sources| sources.children())
        .filter(|node| node.has_tag_name("add"))
        .filter_map(|add| {
            Some((
                add.attribute("key")?.to_owned(),
                add.attribute("value")?.to_owned(),
            ))
        })
        .collect()
}

/// The sources a `pyproject.toml` gives Poetry, by name and URL. A file
/// Poetry could not read gives none.
pub(super) fn poetry_sources(text: &str) -> BTreeMap<String, String> {
    let Ok(document) = text.parse::<toml_edit::DocumentMut>() else {
        return BTreeMap::new();
    };
    document
        .get("tool")
        .and_then(|tool| tool.get("poetry"))
        .and_then(|poetry| poetry.get("source"))
        .and_then(|sources| sources.as_array_of_tables())
        .into_iter()
        .flat_map(|sources| sources.iter())
        .filter_map(|source| {
            Some((
                source.get("name")?.as_str()?.to_owned(),
                source.get("url")?.as_str()?.to_owned(),
            ))
        })
        .collect()
}

/// The registries a `.cargo/config.toml` defines, by name and index URL. A
/// file Cargo could not read defines none.
pub(super) fn cargo_registries(text: &str) -> BTreeMap<String, String> {
    let Ok(config) = text.parse::<toml_edit::DocumentMut>() else {
        return BTreeMap::new();
    };
    config
        .get("registries")
        .and_then(|registries| registries.as_table_like())
        .into_iter()
        .flat_map(|registries| registries.iter())
        .filter_map(|(name, registry)| {
            Some((name.to_owned(), registry.get("index")?.as_str()?.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(prefix: &str, auth: Auth) -> Credential {
        Credential::new(prefix, auth).unwrap()
    }
    fn bearer(token: &str) -> Auth {
        Auth::Bearer {
            token: token.into(),
        }
    }
    fn basic(username: &str, password: &str) -> Auth {
        Auth::Basic {
            username: username.into(),
            password: password.into(),
        }
    }
    fn runner(entries: Vec<Credential>) -> Runner {
        Runner::new(std::env::temp_dir(), "").with_registries(&entries)
    }

    #[test]
    fn host_matching_tools_get_only_an_entry_alone_on_its_host() {
        let runner = runner(vec![
            entry("https://php.acme.dev", basic("ci", "pw")).for_tools(),
            entry("https://tf.acme.dev", bearer("tf")).for_tools(),
            // Shares its host with an entry that is not for tools.
            entry("https://shared.acme.dev/a", bearer("a")).for_tools(),
            entry("https://shared.acme.dev/b", bearer("b")),
            // Names a port, or a path.
            entry("https://port.acme.dev:8443", bearer("p")).for_tools(),
            entry("https://path.acme.dev/team", bearer("p")).for_tools(),
            // Not for tools.
            entry("https://other.acme.dev", bearer("o")),
        ]);
        let auth: serde_json::Value =
            serde_json::from_str(&runner.composer_auth().unwrap()).unwrap();
        assert_eq!(
            auth,
            serde_json::json!({
                "http-basic": { "php.acme.dev": { "username": "ci", "password": "pw" } },
                "bearer": { "tf.acme.dev": "tf" },
            })
        );
        assert_eq!(
            runner.terraform_env(),
            [("TF_TOKEN_tf_acme_dev".to_string(), "tf".to_string())]
        );
        let (netrc, hosts) = runner.netrc();
        assert_eq!(netrc, "machine php.acme.dev login ci password pw\n");
        assert_eq!(hosts, ["php.acme.dev"]);
    }

    #[test]
    fn terraform_hosts_double_their_hyphens() {
        let runner = runner(vec![
            entry("https://tf-reg.acme.dev", bearer("t")).for_tools(),
            // `TF_TOKEN_a_b_acme_dev` would name `a.b.acme.dev` too.
            entry("https://a_b.acme.dev", bearer("u")).for_tools(),
        ]);
        let env = runner.terraform_env();
        assert_eq!(env.len(), 1, "{env:?}");
        assert_eq!(env[0].0, "TF_TOKEN_tf__reg_acme_dev");
    }

    #[test]
    fn go_leaves_out_what_netrc_cannot_hold() {
        let runner = runner(vec![
            entry("https://go.acme.dev", basic("ci", "two words")).for_tools(),
            entry("https://go2.acme.dev", basic("ci", "a#b")).for_tools(),
            entry("https://go3.acme.dev", basic("ci", "a\"b")).for_tools(),
        ]);
        assert_eq!(runner.netrc(), (String::new(), Vec::new()));
    }

    #[test]
    fn cargo_names_get_the_entry_their_index_falls_under() {
        let runner = runner(vec![
            entry("https://crates.acme.dev", bearer("t0ken")).for_tools(),
            // A longer prefix that is not for tools claims its own URLs.
            entry("https://crates.acme.dev/locked", bearer("other")),
            entry("https://basic.acme.dev", basic("ci", "pw")).for_tools(),
        ]);
        let config = cargo_registries(
            "[registries.acme]\nindex = \"sparse+https://crates.acme.dev/index/\"\n\
             [registries.locked]\nindex = \"sparse+https://crates.acme.dev/locked/index/\"\n\
             [registries.git]\nindex = \"https://crates.acme.dev/index.git\"\n\
             [registries.basic-one]\nindex = \"sparse+https://basic.acme.dev/\"\n\
             [source.crates-io]\nreplace-with = \"acme\"\n",
        );
        let env: BTreeMap<_, _> = runner.cargo_env(&[config]).into_iter().collect();
        assert_eq!(
            env,
            BTreeMap::from(
                [
                    (
                        "CARGO_REGISTRIES_ACME_INDEX",
                        "sparse+https://crates.acme.dev/index/"
                    ),
                    ("CARGO_REGISTRIES_ACME_TOKEN", "Bearer t0ken"),
                    (
                        "CARGO_REGISTRIES_BASIC_ONE_INDEX",
                        "sparse+https://basic.acme.dev/"
                    ),
                    ("CARGO_REGISTRIES_BASIC_ONE_TOKEN", "Basic Y2k6cHc="),
                ]
                .map(|(k, v)| (k.to_string(), v.to_string()))
            )
        );
    }

    #[test]
    fn a_cargo_index_written_other_than_it_parses_gets_nothing() {
        let runner = runner(vec![
            entry("https://crates.acme.dev", bearer("t0ken")).for_tools(),
        ]);
        for index in [
            "sparse+https://crates.acme.dev\\@evil.example/",
            "sparse+https://crates%2Eacme.dev/",
            "sparse+https://CRATES.acme.dev/",
            "sparse+https://ci@crates.acme.dev/",
            "sparse+https://crates.acme.dev/a/../b/",
        ] {
            let config = BTreeMap::from([("acme".to_string(), index.to_string())]);
            assert!(runner.cargo_env(&[config]).is_empty(), "{index}");
        }
        let odd = BTreeMap::from([(
            "e_index=x".to_string(),
            "sparse+https://crates.acme.dev/".to_string(),
        )]);
        assert!(runner.cargo_env(&[odd]).is_empty());
    }

    #[test]
    fn a_cargo_name_defined_two_ways_gets_nothing() {
        let runner = runner(vec![
            entry("https://crates.acme.dev", bearer("t0ken")).for_tools(),
            entry("https://crates.other.dev", bearer("other")).for_tools(),
        ]);
        let root =
            cargo_registries("[registries.acme]\nindex = \"sparse+https://crates.acme.dev/\"\n");
        let nested =
            cargo_registries("[registries.acme]\nindex = \"sparse+https://crates.other.dev/\"\n");
        assert!(runner.cargo_env(&[root.clone(), nested]).is_empty());
        // The same entry, but two index URLs.
        let elsewhere = cargo_registries(
            "[registries.acme]\nindex = \"sparse+https://crates.acme.dev/other/\"\n",
        );
        assert!(runner.cargo_env(&[root.clone(), elsewhere]).is_empty());
        // The same definition twice is one.
        assert_eq!(runner.cargo_env(&[root.clone(), root.clone()]).len(), 2);
        let dashed = cargo_registries(
            "[registries.a-b]\nindex = \"sparse+https://crates.acme.dev/\"\n[registries.a_b]\nindex = \"sparse+https://crates.other.dev/\"\n",
        );
        assert!(runner.cargo_env(&[dashed]).is_empty());
    }

    #[test]
    fn cargo_registries_read_tables_and_inline_tables() {
        let registries = cargo_registries(
            "registries.inline = { index = \"sparse+https://a.dev/\" }\n[registries.acme]\nindex = \"sparse+https://crates.acme.dev/\"\ncredential-provider = \"evil\"\n",
        );
        assert_eq!(registries["acme"], "sparse+https://crates.acme.dev/");
        assert_eq!(registries["inline"], "sparse+https://a.dev/");
        assert_eq!(cargo_registries("not toml ["), BTreeMap::new());
    }

    #[test]
    fn bundler_gets_basic_entries_by_host() {
        let runner = runner(vec![
            entry("https://gems.acme-corp.dev", basic("ci", "pw:x")).for_tools(),
            entry("https://token.acme.dev", bearer("t")).for_tools(),
            entry("https://a_b.acme.dev", basic("ci", "pw")).for_tools(),
        ]);
        assert_eq!(
            runner.bundler_env(),
            [(
                "BUNDLE_GEMS__ACME___CORP__DEV".to_string(),
                "ci:pw:x".to_string()
            )]
        );
    }

    #[test]
    fn nuget_sources_get_basic_credentials_by_key() {
        let runner = runner(vec![
            entry("https://nuget.acme.dev", basic("ci", "pw")).for_tools(),
            entry("https://semi.acme.dev", basic("ci", "a;b")).for_tools(),
            entry("https://token.acme.dev", bearer("t")).for_tools(),
        ]);
        let sources = nuget_sources(
            r#"<?xml version="1.0"?>
<configuration>
  <packageSources>
    <clear />
    <add key="acme.feed" value="https://nuget.acme.dev/v3/index.json" />
    <add key="semi" value="https://semi.acme.dev/v3/index.json" />
    <add key="token" value="https://token.acme.dev/v3/index.json" />
    <add key="has space" value="https://nuget.acme.dev/other/index.json" />
    <add key="nuget.org" value="https://api.nuget.org/v3/index.json" />
  </packageSources>
</configuration>"#,
        );
        assert_eq!(sources.len(), 5);
        assert_eq!(
            runner.nuget_env(&[sources]),
            [(
                "NuGetPackageSourceCredentials_acme.feed".to_string(),
                "Username=ci;Password=pw;ValidAuthenticationTypes=basic".to_string()
            )]
        );
        assert!(nuget_sources("<configuration").is_empty());
    }

    #[test]
    fn poetry_sources_get_basic_credentials_by_name() {
        let runner = runner(vec![
            entry("https://py.acme.dev", basic("ci", "pw")).for_tools(),
        ]);
        let sources = poetry_sources(
            "[[tool.poetry.source]]\nname = \"acme-private.x\"\nurl = \"https://py.acme.dev/simple/\"\npriority = \"supplemental\"\n[[tool.poetry.source]]\nname = \"pypi-mirror\"\nurl = \"https://pypi.org/simple/\"\n",
        );
        let env: BTreeMap<_, _> = runner.poetry_env(&[sources]).into_iter().collect();
        assert_eq!(
            env,
            BTreeMap::from([
                (
                    "POETRY_HTTP_BASIC_ACME_PRIVATE_X_USERNAME".to_string(),
                    "ci".to_string()
                ),
                (
                    "POETRY_HTTP_BASIC_ACME_PRIVATE_X_PASSWORD".to_string(),
                    "pw".to_string()
                ),
            ])
        );
        assert!(poetry_sources("[tool.poetry").is_empty());
    }
}
