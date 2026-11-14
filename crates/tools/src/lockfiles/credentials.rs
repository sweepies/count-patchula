//! Operator registry credentials for lockfile tools, from entries that set
//! `tools = true`, in the form each tool reads them. npm, aube, Bun and Yarn
//! get theirs from `node`.
//!
//! Bundler, Composer, Go, uv, Terraform and, through `~/.netrc`, Poetry
//! match credentials by host rather than by prefix, so they only get an entry whose prefix is a bare host,
//! with no path or port, and that is the only entry on its host: a path would
//! widen to the whole host, and another entry there, for any installation,
//! would have its URLs given this one's credentials. Cargo, NuGet and Poetry
//! match them by the name the repository gives a registry, so a name gets
//! the entry its URL falls under, as a lookup would.
use super::Runner;
use patchula_http::{Auth, Credential};
use std::collections::BTreeMap;

/// The names a repository's configuration gives registries, and their URLs,
/// in the order it gives them; a name may come more than once.
pub(super) type Definitions = Vec<(String, String)>;

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

    /// `COMPOSER_AUTH`: `http-basic` and `bearer` credentials by host, and the
    /// entries used.
    pub(super) fn composer_auth(&self) -> (Option<String>, Vec<&Credential>) {
        let mut basic = serde_json::Map::new();
        let mut bearer = serde_json::Map::new();
        let mut entries = Vec::new();
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
                _ => continue,
            }
            entries.push(entry);
        }
        (
            (!basic.is_empty() || !bearer.is_empty())
                .then(|| serde_json::json!({ "http-basic": basic, "bearer": bearer }).to_string()),
            entries,
        )
    }

    /// Terraform's `TF_TOKEN_<host>` variables, for bearer entries: dots in
    /// the host become `_` and hyphens `__`. A host with `_` of its own is
    /// left out, as its variable could name another host. The entries used
    /// come back too.
    pub(super) fn terraform_env(&self) -> (Vec<(String, String)>, Vec<&Credential>) {
        self.by_host()
            .filter(|entry| !entry.host().0.contains('_'))
            .filter_map(|entry| match &entry.auth {
                Auth::Bearer { token } => Some((
                    (
                        format!(
                            "TF_TOKEN_{}",
                            entry.host().0.replace('-', "__").replace('.', "_")
                        ),
                        token.clone(),
                    ),
                    entry,
                )),
                _ => None,
            })
            .unzip()
    }

    /// `.netrc` for basic entries, read by Go, Git and uv, the hosts it
    /// covers and the entries used. `.netrc` separates fields by whitespace,
    /// and readers differ on quotes, backslashes and `#`, so a user or
    /// password holding any is left out.
    pub(super) fn netrc(&self) -> (String, Vec<String>, Vec<&Credential>) {
        let mut netrc = String::new();
        let mut hosts = Vec::new();
        let mut entries = Vec::new();
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
            entries.push(entry);
        }
        (netrc, hosts, entries)
    }

    /// Bundler's `BUNDLE_<HOST>` variables for basic entries, `user:password`:
    /// the host in uppercase, with dots as `__` and hyphens as `___`. A host
    /// with `_` of its own, whose variable could name another host, or with
    /// no dot, whose variable could be a Bundler setting such as
    /// `BUNDLE_PATH`, is left out, and so is a user or password with anything
    /// but the characters a URL's user part holds as they are. The entries
    /// used come back too.
    pub(super) fn bundler_env(&self) -> (Vec<(String, String)>, Vec<&Credential>) {
        self.by_host()
            .filter(|entry| !entry.host().0.contains('_') && entry.host().0.contains('.'))
            .filter_map(|entry| match &entry.auth {
                Auth::Basic { username, password }
                    if userinfo(username, false) && userinfo(password, true) =>
                {
                    Some((
                        (
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
                        ),
                        entry,
                    ))
                }
                _ => None,
            })
            .unzip()
    }

    /// Each name `definitions` gives a URL under a tools entry, by the
    /// fragment `fold` makes of it as the tool does, with the name,
    /// definition and entry. Only a name `plain` accepts can get one. `url`
    /// takes the https URL out of a definition, which counts only when it is
    /// written as it parses: tools read the text as written and could read a
    /// backslash, escape or case differently and send the credentials to
    /// another host. Names with the same fragment, unless every one is the
    /// same plain name with the same definition, get nothing, so a name the
    /// tool folds onto a credentialed one keeps it from being given.
    fn named<'a, 'b>(
        &'a self,
        definitions: &'b [Definitions],
        fold: impl Fn(&str) -> String,
        plain: impl Fn(&str) -> bool,
        url: impl Fn(&'b str) -> Option<&'b str>,
    ) -> Vec<(String, &'b str, &'b str, &'a Credential)> {
        type Definition<'a, 'b> = (&'b str, &'b str, Option<&'a Credential>);
        let mut by_key: BTreeMap<String, Vec<Definition<'a, 'b>>> = BTreeMap::new();
        for (name, definition) in definitions.iter().flatten() {
            let fragment = fold(name);
            let entry = plain(name)
                .then_some(definition.as_str())
                .and_then(&url)
                .filter(|url| {
                    patchula_http::url_prefix(url).as_deref() == Some(url.trim_end_matches('/'))
                })
                .and_then(|url| patchula_http::entry_for(&self.entries, url))
                .filter(|entry| self.registries.contains(entry));
            by_key
                .entry(fragment)
                .or_default()
                .push((name, definition, entry));
        }
        by_key
            .into_iter()
            .filter_map(|(fragment, all)| {
                let (name, definition, entry) = *all.first()?;
                let entry = entry?;
                if all
                    .iter()
                    .any(|other| *other != (name, definition, Some(entry)))
                {
                    return None;
                }
                Some((fragment, name, definition, entry))
            })
            .collect()
    }

    /// Cargo's `CARGO_REGISTRIES_<NAME>_INDEX` and `_TOKEN` variables for
    /// the registries `.cargo/config.toml` files define, by name and sparse
    /// index URL. Cargo sends the token as the `Authorization` header, so it
    /// is the header a lookup sends: `Bearer <token>` or `Basic <base64>`.
    pub(super) fn cargo_env<'a>(
        &'a self,
        registries: &[Definitions],
    ) -> (Vec<(String, String)>, Vec<&'a Credential>) {
        self.named(
            registries,
            |name| name.to_uppercase().replace('-', "_"),
            |name| plain(name, &['-', '_']),
            |index| index.strip_prefix("sparse+"),
        )
        .into_iter()
        .filter_map(|(name, _, index, entry)| {
            let header = entry.auth.header()?.to_str().ok()?.to_owned();
            Some((
                [
                    (format!("CARGO_REGISTRIES_{name}_INDEX"), index.to_owned()),
                    (format!("CARGO_REGISTRIES_{name}_TOKEN"), header),
                ],
                entry,
            ))
        })
        .fold((Vec::new(), Vec::new()), fold_env)
    }

    /// NuGet's `NuGetPackageSourceCredentials_<key>` variables for the
    /// basic entries the `packageSources` of `nuget.config` files fall under,
    /// by source key. NuGet compares keys without case, so keys that differ
    /// only in case are one. The value separates fields by `;`, so a user or
    /// password holding `;` is left out.
    pub(super) fn nuget_env<'a>(
        &'a self,
        sources: &[Definitions],
    ) -> (Vec<(String, String)>, Vec<&'a Credential>) {
        self.named(
            sources,
            str::to_uppercase,
            |name| plain(name, &['-', '_', '.']),
            |url: &str| Some(url),
        )
        .into_iter()
        .filter_map(|(_, name, _, entry)| match &entry.auth {
            Auth::Basic { username, password }
                if !username.contains(';') && !password.contains(';') =>
            {
                Some((
                    (
                        format!("NuGetPackageSourceCredentials_{name}"),
                        format!(
                            "Username={username};Password={password};ValidAuthenticationTypes=basic"
                        ),
                    ),
                    entry,
                ))
            }
            _ => None,
        })
        .unzip()
    }

    /// Poetry's `POETRY_HTTP_BASIC_<NAME>_USERNAME` and `_PASSWORD`
    /// variables for the basic entries the `[[tool.poetry.source]]` URLs of
    /// `pyproject.toml` fall under: the name in uppercase, with `-` and `.` as
    /// `_`.
    pub(super) fn poetry_env<'a>(
        &'a self,
        sources: &[Definitions],
    ) -> (Vec<(String, String)>, Vec<&'a Credential>) {
        self.named(
            sources,
            |name| name.to_uppercase().replace(['-', '.'], "_"),
            |name| plain(name, &['-', '_', '.']),
            |url: &str| Some(url),
        )
        .into_iter()
        .filter_map(|(name, _, _, entry)| match &entry.auth {
            Auth::Basic { username, password } => Some((
                [
                    (
                        format!("POETRY_HTTP_BASIC_{name}_USERNAME"),
                        username.clone(),
                    ),
                    (
                        format!("POETRY_HTTP_BASIC_{name}_PASSWORD"),
                        password.clone(),
                    ),
                ],
                entry,
            )),
            _ => None,
        })
        .fold((Vec::new(), Vec::new()), fold_env)
    }
}

/// Folds one entry's environment pairs into the run's `(env, entries)`.
fn fold_env<'a, const N: usize>(
    (mut env, mut entries): (Vec<(String, String)>, Vec<&'a Credential>),
    (vars, entry): ([(String, String); N], &'a Credential),
) -> (Vec<(String, String)>, Vec<&'a Credential>) {
    env.extend(vars);
    entries.push(entry);
    (env, entries)
}

/// The `Credential::scope()`s of `entries`, sorted and deduplicated: the cache
/// partition key for a run that receives exactly these entries.
pub(super) fn scopes<'a>(entries: impl IntoIterator<Item = &'a Credential>) -> Vec<String> {
    let mut scopes: Vec<String> = entries
        .into_iter()
        .map(|entry| entry.scope().to_owned())
        .collect();
    scopes.sort_unstable();
    scopes.dedup();
    scopes
}

/// Every value a run holding `entries` was given, in each form a tool could
/// echo back: the secret itself and the encodings the tools write.
pub(super) fn secrets<'a>(entries: impl IntoIterator<Item = &'a Credential>) -> Vec<String> {
    use base64::Engine;
    let mut secrets = Vec::new();
    for entry in entries {
        match &entry.auth {
            Auth::Bearer { token } => {
                secrets.push(token.clone());
                secrets.push(format!("Bearer {token}"));
            }
            Auth::Basic { username, password } => {
                secrets.push(password.clone());
                let userinfo = format!("{username}:{password}");
                secrets.push(userinfo.clone());
                let encoded = base64::engine::general_purpose::STANDARD.encode(&userinfo);
                secrets.push(encoded.clone());
                secrets.push(format!("Basic {encoded}"));
            }
            Auth::Withheld | Auth::Anonymous => {}
        }
    }
    secrets
}

/// Whether `field` is only characters a URL's user part holds unescaped:
/// unreserved characters and sub-delimiters but `+`, which a form decoder
/// reads as a space, and `:` in a password.
fn userinfo(field: &str, password: bool) -> bool {
    field.chars().all(|c| {
        c.is_ascii_alphanumeric() || "-._~!$&'()*,;=".contains(c) || (password && c == ':')
    })
}

/// Whether `name` is ASCII letters, digits and `extra` alone.
fn plain(name: &str, extra: &[char]) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(&c))
}

/// Whether `node` is the element `name`, in any case or namespace, as NuGet
/// reads it.
fn named(node: &roxmltree::Node, name: &str) -> bool {
    node.is_element() && node.tag_name().name().eq_ignore_ascii_case(name)
}

/// The sources a `nuget.config` adds, by key and URL, or None if the Count
/// cannot read it.
pub(super) fn nuget_sources(text: &str) -> Option<Definitions> {
    let document = roxmltree::Document::parse(text).ok()?;
    let sources = document
        .root_element()
        .children()
        .filter(|node| named(node, "packageSources"))
        .flat_map(|sources| sources.children())
        .filter(|node| named(node, "add"))
        .filter_map(|add| {
            let attribute = |name: &str| {
                add.attributes()
                    .find(|attribute| attribute.name().eq_ignore_ascii_case(name))
                    .map(|attribute| attribute.value().to_owned())
            };
            Some((attribute("key")?, attribute("value")?))
        })
        .collect();
    Some(sources)
}

/// The sources a `pyproject.toml` gives Poetry, by name and URL. A file
/// Poetry could not read gives none.
pub(super) fn poetry_sources(text: &str) -> Definitions {
    let Ok(document) = text.parse::<toml_edit::DocumentMut>() else {
        return Vec::new();
    };
    let Some(sources) = document
        .get("tool")
        .and_then(|tool| tool.get("poetry"))
        .and_then(|poetry| poetry.get("source"))
    else {
        return Vec::new();
    };
    let tables: Vec<&dyn toml_edit::TableLike> = if let Some(tables) = sources.as_array_of_tables()
    {
        tables
            .iter()
            .map(|table| table as &dyn toml_edit::TableLike)
            .collect()
    } else if let Some(array) = sources.as_array() {
        array
            .iter()
            .filter_map(|value| value.as_inline_table())
            .map(|table| table as &dyn toml_edit::TableLike)
            .collect()
    } else {
        Vec::new()
    };
    tables
        .into_iter()
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
pub(super) fn cargo_registries(text: &str) -> Definitions {
    let Ok(config) = text.parse::<toml_edit::DocumentMut>() else {
        return Vec::new();
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
    #[test]
    fn every_form_a_tool_could_echo_is_a_secret() {
        let entries = [
            entry("https://a.example/", bearer("tok")),
            entry("https://b.example/", basic("user", "pass")),
            entry("https://c.example/", Auth::Anonymous),
        ];
        let secrets = secrets(&entries);
        assert_eq!(
            secrets,
            [
                "tok",
                "Bearer tok",
                "pass",
                "user:pass",
                "dXNlcjpwYXNz",
                "Basic dXNlcjpwYXNz",
            ]
        );
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
            serde_json::from_str(&runner.composer_auth().0.unwrap()).unwrap();
        assert_eq!(
            auth,
            serde_json::json!({
                "http-basic": { "php.acme.dev": { "username": "ci", "password": "pw" } },
                "bearer": { "tf.acme.dev": "tf" },
            })
        );
        assert_eq!(
            runner.terraform_env().0,
            [("TF_TOKEN_tf_acme_dev".to_string(), "tf".to_string())]
        );
        let (netrc, hosts, _) = runner.netrc();
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
        let env = runner.terraform_env().0;
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
        assert_eq!(runner.netrc(), (String::new(), Vec::new(), Vec::new()));
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
        let env: BTreeMap<_, _> = runner.cargo_env(&[config]).0.into_iter().collect();
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
            let config = vec![("acme".to_string(), index.to_string())];
            assert!(runner.cargo_env(&[config]).0.is_empty(), "{index}");
        }
        let odd = vec![(
            "e_index=x".to_string(),
            "sparse+https://crates.acme.dev/".to_string(),
        )];
        assert!(runner.cargo_env(&[odd]).0.is_empty());
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
        assert!(runner.cargo_env(&[root.clone(), nested]).0.is_empty());
        // The same entry, but two index URLs.
        let elsewhere = cargo_registries(
            "[registries.acme]\nindex = \"sparse+https://crates.acme.dev/other/\"\n",
        );
        assert!(runner.cargo_env(&[root.clone(), elsewhere]).0.is_empty());
        // The same definition twice is one.
        assert_eq!(runner.cargo_env(&[root.clone(), root.clone()]).0.len(), 2);
        let dashed = cargo_registries(
            "[registries.a-b]\nindex = \"sparse+https://crates.acme.dev/\"\n[registries.a_b]\nindex = \"sparse+https://crates.other.dev/\"\n",
        );
        assert!(runner.cargo_env(&[dashed]).0.is_empty());
    }

    #[test]
    fn cargo_registries_read_tables_and_inline_tables() {
        let registries = cargo_registries(
            "registries.inline = { index = \"sparse+https://a.dev/\" }\n[registries.acme]\nindex = \"sparse+https://crates.acme.dev/\"\ncredential-provider = \"evil\"\n",
        );
        let registries: BTreeMap<_, _> = registries.into_iter().collect();
        assert_eq!(registries["acme"], "sparse+https://crates.acme.dev/");
        assert_eq!(registries["inline"], "sparse+https://a.dev/");
        assert!(cargo_registries("not toml [").is_empty());
    }

    #[test]
    fn bundler_gets_basic_entries_by_host() {
        let runner = runner(vec![
            entry("https://gems.acme-corp.dev", basic("ci", "pw:x")).for_tools(),
            entry("https://token.acme.dev", bearer("t")).for_tools(),
            entry("https://a_b.acme.dev", basic("ci", "pw")).for_tools(),
            // `BUNDLE_PATH` is a Bundler setting.
            entry("https://path", basic("ci", "pw")).for_tools(),
            entry("https://at.acme.dev", basic("ci", "p@ss")).for_tools(),
            entry("https://plus.acme.dev", basic("ci", "a+b")).for_tools(),
        ]);
        assert_eq!(
            runner.bundler_env().0,
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
        )
        .unwrap();
        assert_eq!(sources.len(), 5);
        assert_eq!(
            runner.nuget_env(&[sources]).0,
            [(
                "NuGetPackageSourceCredentials_acme.feed".to_string(),
                "Username=ci;Password=pw;ValidAuthenticationTypes=basic".to_string()
            )]
        );
        assert!(nuget_sources("<configuration").is_none());
        // NuGet reads elements and attributes in any case or namespace.
        assert_eq!(
            nuget_sources(
                "<configuration xmlns=\"urn:x\"><PackageSources><Add Key=\"feed\" Value=\"https://nuget.acme.dev/\" /></PackageSources></configuration>",
            ),
            Some(vec![(
                "feed".to_string(),
                "https://nuget.acme.dev/".to_string()
            )])
        );
        // Keys differ in case only, or come twice in one file.
        for config in [
            "<configuration><packageSources><add key=\"Feed\" value=\"https://nuget.acme.dev/a/\" /></packageSources></configuration>",
            "<configuration><packageSources><add key=\"feed\" value=\"https://nuget.acme.dev/a/\" /><add key=\"feed\" value=\"https://nuget.acme.dev/b/\" /></packageSources></configuration>",
        ] {
            let feed = nuget_sources(
                "<configuration><packageSources><add key=\"feed\" value=\"https://nuget.acme.dev/a/\" /></packageSources></configuration>",
            )
            .unwrap();
            assert_eq!(runner.nuget_env(std::slice::from_ref(&feed)).0.len(), 1);
            assert!(
                runner
                    .nuget_env(&[feed, nuget_sources(config).unwrap()])
                    .0
                    .is_empty(),
                "{config}"
            );
        }
    }

    #[test]
    fn poetry_sources_get_basic_credentials_by_name() {
        let runner = runner(vec![
            entry("https://py.acme.dev", basic("ci", "pw")).for_tools(),
        ]);
        let sources = poetry_sources(
            "[[tool.poetry.source]]\nname = \"acme-private.x\"\nurl = \"https://py.acme.dev/simple/\"\npriority = \"supplemental\"\n[[tool.poetry.source]]\nname = \"pypi-mirror\"\nurl = \"https://pypi.org/simple/\"\n",
        );
        let env: BTreeMap<_, _> = runner.poetry_env(&[sources]).0.into_iter().collect();
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
        // Poetry folds `acmeſ` onto `ACMES`, so it keeps `acmes` from its
        // credentials.
        let folded = vec![
            (
                "acmes".to_string(),
                "https://py.acme.dev/simple/".to_string(),
            ),
            (
                "acmeſ".to_string(),
                "https://evil.example/simple/".to_string(),
            ),
        ];
        assert!(runner.poetry_env(&[folded]).0.is_empty());
        let inline = poetry_sources(
            "[tool.poetry]\nsource = [{ name = \"acme\", url = \"https://py.acme.dev/simple/\" }]\n",
        );
        assert_eq!(runner.poetry_env(&[inline]).0.len(), 2);
    }
}
