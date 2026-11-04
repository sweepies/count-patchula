//! Operator registry credentials for lockfile tools, from entries that set
//! `tools = true`, in the form each tool reads them. npm, aube, Bun and Yarn
//! get theirs from `node`.
//!
//! Composer, Go and Terraform match credentials by host rather than by
//! prefix, so they only get an entry that is the only one on its host and
//! names no port: one any other entry shares, or for another installation,
//! would otherwise lend its credentials to that entry's URLs. Cargo matches
//! them by registry name, so a name gets the entry its index URL falls
//! under, as a lookup would.
use super::Runner;
use patchula_http::{Auth, Credential};
use std::collections::BTreeMap;

impl Runner {
    /// Tools entries a host-matching tool can use.
    fn by_host(&self) -> impl Iterator<Item = &Credential> {
        self.registries.iter().filter(|entry| {
            let (host, port) = entry.host();
            port.is_none()
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
    /// the host become `_` and hyphens `__`.
    pub(super) fn terraform_env(&self) -> Vec<(String, String)> {
        self.by_host()
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

    /// Go's `.netrc` for basic entries, and the hosts it covers, which go to
    /// `GOPRIVATE` so their modules are fetched from the host itself, never
    /// asked of the public module proxy or checksum database. `.netrc`
    /// separates fields by whitespace, so a user or password holding any is
    /// left out.
    pub(super) fn go_netrc(&self) -> (String, Vec<String>) {
        let mut netrc = String::new();
        let mut hosts = Vec::new();
        for entry in self.by_host() {
            let Auth::Basic { username, password } = &entry.auth else {
                continue;
            };
            if [username, password]
                .iter()
                .any(|field| field.chars().any(char::is_whitespace))
            {
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

    /// Cargo's `CARGO_REGISTRIES_<NAME>_TOKEN` variables for the registries
    /// `.cargo/config.toml` files define, by name and sparse index URL. Cargo
    /// sends the token as the `Authorization` header, so it is the header a
    /// lookup sends: `Bearer <token>` or `Basic <base64>`. A name defined
    /// under more than one entry, or two names Cargo would read from the same
    /// variable, get nothing.
    pub(super) fn cargo_env(
        &self,
        registries: &[BTreeMap<String, String>],
    ) -> Vec<(String, String)> {
        let mut by_variable: BTreeMap<String, Vec<Option<&Credential>>> = BTreeMap::new();
        for (name, index) in registries.iter().flatten() {
            let entry = index
                .strip_prefix("sparse+")
                .and_then(|url| patchula_http::entry_for(&self.entries, url))
                .filter(|entry| self.registries.contains(entry));
            by_variable
                .entry(format!(
                    "CARGO_REGISTRIES_{}_TOKEN",
                    name.to_ascii_uppercase().replace('-', "_")
                ))
                .or_default()
                .push(entry);
        }
        by_variable
            .into_iter()
            .filter_map(|(variable, entries)| {
                let first = (*entries.first()?)?;
                if entries.iter().any(|entry| *entry != Some(first)) {
                    return None;
                }
                let header = first.auth.header()?;
                Some((variable, header.to_str().ok()?.to_owned()))
            })
            .collect()
    }
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

/// A `.cargo/config.toml` that only defines `registries`: no credential
/// provider, source replacement, proxy, alias or anything else of the
/// repository's.
pub(super) fn cargo_config(registries: &BTreeMap<String, String>) -> String {
    let mut table = toml_edit::Table::new();
    table.set_implicit(true);
    for (name, index) in registries {
        let mut registry = toml_edit::Table::new();
        registry.insert("index", toml_edit::value(index.as_str()));
        table.insert(name, toml_edit::Item::Table(registry));
    }
    let mut config = toml_edit::DocumentMut::new();
    config.insert("registries", toml_edit::Item::Table(table));
    config.to_string()
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
            // Names a port.
            entry("https://port.acme.dev:8443", bearer("p")).for_tools(),
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
        let (netrc, hosts) = runner.go_netrc();
        assert_eq!(netrc, "machine php.acme.dev login ci password pw\n");
        assert_eq!(hosts, ["php.acme.dev"]);
    }

    #[test]
    fn terraform_hosts_double_their_hyphens() {
        let runner = runner(vec![
            entry("https://tf-reg.acme.dev", bearer("t")).for_tools(),
        ]);
        assert_eq!(runner.terraform_env()[0].0, "TF_TOKEN_tf__reg_acme_dev");
    }

    #[test]
    fn go_leaves_out_what_netrc_cannot_hold() {
        let runner = runner(vec![
            entry("https://go.acme.dev", basic("ci", "two words")).for_tools(),
        ]);
        assert_eq!(runner.go_netrc(), (String::new(), Vec::new()));
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
        assert_eq!(
            runner.cargo_env(&[config]),
            [
                (
                    "CARGO_REGISTRIES_ACME_TOKEN".to_string(),
                    "Bearer t0ken".to_string()
                ),
                (
                    "CARGO_REGISTRIES_BASIC_ONE_TOKEN".to_string(),
                    "Basic Y2k6cHc=".to_string()
                ),
            ]
        );
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
        let dashed = cargo_registries(
            "[registries.a-b]\nindex = \"sparse+https://crates.acme.dev/\"\n[registries.a_b]\nindex = \"sparse+https://crates.other.dev/\"\n",
        );
        assert!(runner.cargo_env(&[dashed]).is_empty());
    }

    #[test]
    fn cargo_config_keeps_only_registries() {
        let registries = cargo_registries(
            "[registries.acme]\nindex = \"sparse+https://crates.acme.dev/\"\ncredential-provider = \"evil\"\n[build]\nrustc-wrapper = \"evil\"\n",
        );
        let config = cargo_config(&registries);
        assert!(!config.contains("evil"), "{config}");
        assert_eq!(cargo_registries(&config), registries);
        assert_eq!(registries["acme"], "sparse+https://crates.acme.dev/");
        assert_eq!(cargo_registries("not toml ["), BTreeMap::new());
    }
}
