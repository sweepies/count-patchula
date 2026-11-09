//! Whether a file lets a lockfile tool fetch only from its ecosystem's
//! well-known public registry — the `public_only` verdicts that decide
//! whether a tool run may share the `public` cache partition. The runner
//! checks every file a run can read; a file with no verdict, or one the
//! Count cannot read, is not public.
//!
//! A file is public-only when every registry, index or fetch source it
//! names is the public registry, a well-known public forge, or a local
//! source that fetches nothing (a path, a link, a workspace member).
//! Anything else fails closed: private content must never enter a cache
//! shared with other installations, and a fetch the Count cannot rule on
//! might be one.
//!
//! The verdict is deny-by-default: the file is parsed — JSON, TOML, YAML,
//! XML or a line grammar for the formats that are not trees — and every
//! string it holds is checked, keys included, since some formats name
//! locators in keys. Every string that can be a fetch source must fetch
//! only public or local content; a file that does not parse, or a string
//! the Count cannot rule on, makes the file not public.
//!
//! Keys that by their format always hold a fetch location — a Composer
//! `repositories[].url`, a Yarn `npmRegistryServer`, an index `url`, a
//! `packageSources` value — get a stricter rule on their value alone:
//! it must name the ecosystem's public registry itself, or be a local
//! path where the format allows one, or carry a scheme and fetch only
//! public or local content. A bare host, a `${VAR}` or an unparseable
//! value there is a source a scheme-less string check would miss, so it
//! is not public.
//!
//! Only an explicit, small allowlist of inert metadata keys and tables
//! per format — `package.json`'s `homepage`, `[project]`'s `urls`, the
//! pure-lint `[tool]` tables and their like — goes unchecked: a URL
//! there reaches a web page, not a download. A key missing from the
//! allowlist can only cost performance — the run lands on the
//! installation partition — never safety.
use crate::Files;
use crate::origins::{Named, Public};
use jsonc_parser::ast::Value;

/// Well-known public forges: their content is public by construction, so
/// a fetch there can neither hold private packages nor answer as another
/// installation's private registry.
const PUBLIC_HOSTS: &[&str] = &["github.com", "gitlab.com", "bitbucket.org", "codeberg.org"];

/// `package.json` keys, at the top level, whose values never steer a
/// fetch.
const PACKAGE_JSON_INERT: &[&str] = &[
    "homepage",
    "repository",
    "bugs",
    "funding",
    "author",
    "contributors",
    "maintainers",
    "description",
    "license",
    "keywords",
];
/// `Cargo.toml` keys, under `[package]` or `[workspace.package]`, whose
/// values never steer a fetch.
const CARGO_PACKAGE_INERT: &[&str] = &[
    "homepage",
    "repository",
    "documentation",
    "description",
    "authors",
    "license",
    "readme",
    "keywords",
    "categories",
];
/// `pyproject.toml` keys, under `[project]`, whose values never steer a
/// fetch.
const PROJECT_INERT: &[&str] = &[
    "urls",
    "authors",
    "maintainers",
    "description",
    "readme",
    "license",
    "keywords",
    "classifiers",
];
/// `composer.json` keys, at the top level, whose values never steer a
/// fetch.
const COMPOSER_INERT: &[&str] = &[
    "homepage",
    "support",
    "authors",
    "description",
    "keywords",
    "license",
    "funding",
];
/// MSBuild elements whose text is project metadata: it reaches a package
/// page, not a download.
const MSBUILD_INERT: &[&str] = &[
    "PackageProjectUrl",
    "RepositoryUrl",
    "PackageLicenseUrl",
    "PackageIconUrl",
    "Description",
    "Authors",
];

/// The host a fetch `url` reaches: the authority's host without its
/// userinfo or port, for the schemes tools fetch over.
pub(crate) fn url_host(url: &str) -> Option<String> {
    let rest = ["https://", "http://", "ssh://", "git://", "ftp://"]
        .iter()
        .find_map(|scheme| {
            url.get(..scheme.len())
                .filter(|prefix| prefix.eq_ignore_ascii_case(scheme))
                .map(|_| &url[scheme.len()..])
        })?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Whether `userinfo` is entirely a placeholder the environment or CI
/// resolves at run time — `$NAME`, `${NAME}`, `${{ … }}`, `$(…)` or
/// `%NAME%` — and so holds no credential of its own.
fn placeholder(userinfo: &str) -> bool {
    if let Some(inner) = userinfo
        .strip_prefix("${{")
        .and_then(|u| u.strip_suffix("}}"))
    {
        return !inner.trim().is_empty();
    }
    if let Some(inner) = userinfo
        .strip_prefix("${")
        .and_then(|u| u.strip_suffix('}'))
    {
        return !inner.is_empty();
    }
    if let Some(inner) = userinfo
        .strip_prefix("$(")
        .and_then(|u| u.strip_suffix(')'))
    {
        return !inner.is_empty();
    }
    if let Some(name) = userinfo.strip_prefix('$') {
        return !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    }
    if let Some(name) = userinfo.strip_prefix('%').and_then(|u| u.strip_suffix('%')) {
        return !name.is_empty() && !name.contains('%');
    }
    false
}

/// Whether `value`, a URL or specifier a manifest or lockfile names for a
/// fetch, can only reach public content: the ecosystem's public registry
/// or a well-known public host, or a local source that fetches nothing.
/// Anything else fails closed.
pub(crate) fn fetch_ok(value: &str, public: &Public) -> bool {
    let value = value.trim().trim_matches(['"', '\'']);
    // SCP-style `git@host:path`.
    if let Some(rest) = value.strip_prefix("git@") {
        return rest
            .split(':')
            .next()
            .is_some_and(|host| PUBLIC_HOSTS.contains(&host));
    }
    // Yarn locators `name@protocol:reference`: check the reference.
    let value = value
        .rsplit_once('@')
        .filter(|(before, after)| {
            !before.is_empty() && !before.contains("://") && after.contains(':')
        })
        .map_or(value, |(_, after)| after.trim());
    for prefix in ["git+", "sparse+", "registry+", "tar+", "npm:"] {
        if let Some(rest) = value.strip_prefix(prefix) {
            return fetch_ok(rest, public);
        }
    }
    if (public.is_public)(value) {
        return true;
    }
    let Some((scheme, rest)) = value.split_once(':') else {
        // No scheme: a version, a hash or a local path.
        return true;
    };
    match scheme.to_ascii_lowercase().as_str() {
        // Local sources fetch nothing.
        "file" | "link" | "workspace" | "portal" | "path" | "catalog" | "patch" | "npm"
        | "exec" | "root" | "virtual" | "editable" | "directory" | "local" | "static"
        | "symlink" => true,
        // The public forges.
        "github" | "gitlab" | "bitbucket" | "gist" => true,
        "https" | "http" | "git" | "ftp" => {
            let Some(authority) = rest
                .strip_prefix("//")
                .and_then(|rest| rest.split(['/', '?', '#']).next())
            else {
                return false;
            };
            // Userinfo that is not entirely an environment placeholder can
            // authenticate, with or without a password: forges take a
            // token as the username.
            if let Some((userinfo, _)) = authority.rsplit_once('@')
                && !placeholder(userinfo)
            {
                return false;
            }
            url_host(value).is_some_and(|host| PUBLIC_HOSTS.contains(&host.as_str()))
        }
        // ssh's userinfo is a login name, not a credential; a literal
        // password in it still is one.
        "ssh" => {
            let Some(authority) = rest
                .strip_prefix("//")
                .and_then(|rest| rest.split(['/', '?', '#']).next())
            else {
                return false;
            };
            if let Some((userinfo, _)) = authority.rsplit_once('@')
                && userinfo
                    .split(':')
                    .nth(1)
                    .is_some_and(|password| !password.is_empty() && !placeholder(password))
            {
                return false;
            }
            url_host(value).is_some_and(|host| PUBLIC_HOSTS.contains(&host.as_str()))
        }
        _ => false,
    }
}

/// Whether `value`, one token of a file, can be a fetch source: a `://`
/// URL, a `git@` or protocol-prefixed source, or an npm `npm:` alias or
/// Yarn locator (`name@reference`) whose reference carries a protocol,
/// split the same way `fetch_ok` splits it. Anything else — a name, a
/// version, a hash, prose — is not a source and is not checked. A
/// scheme-less value can still be a source when its key always names one;
/// the [`Rule`] a key carries covers those.
fn possible_source(value: &str) -> bool {
    if value.contains("://") {
        return true;
    }
    for prefix in [
        "git@",
        "git+",
        "git:",
        "github:",
        "gitlab:",
        "bitbucket:",
        "gist:",
        "file:",
        "link:",
        "portal:",
        "patch:",
        "http:",
        "https:",
        "npm:",
    ] {
        if value
            .get(..prefix.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
        {
            return true;
        }
    }
    value.rsplit_once('@').is_some_and(|(before, after)| {
        !before.is_empty() && !before.contains("://") && after.contains(':')
    })
}

/// Whether `value` — a whole string a file holds — names no fetch source
/// that is not public or local. Each token of it is checked apart, so a
/// spec such as `requests @ https://…` or a lockfile's
/// `name 1.0 (registry+https://…)` is ruled on by the source it names,
/// not by the text around it.
fn string_ok(value: &str, public: &Public) -> bool {
    value.split_whitespace().all(|token| {
        let token = token.trim_matches(|c: char| {
            matches!(
                c,
                '"' | '\''
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '<'
                    | '>'
                    | ','
                    | ';'
                    | ':'
                    | '|'
                    | '&'
            )
        });
        !possible_source(token) || fetch_ok(token, public)
    })
}

/// How a key at `path` treats the strings it holds. A key with no rule
/// walks every string, which can only ever cost a run its `public`
/// partition, never make an unsafe file public.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Rule {
    /// Walk every string under the key.
    Walk,
    /// Inert metadata: the strings steer nothing and are not checked.
    Inert,
    /// The value names a registry: each string must be the ecosystem's
    /// public registry itself, the rule `.npmrc` applies to `registry`.
    Registry,
    /// The value names a fetch location: a local path where the format
    /// allows one, or a source carrying a scheme that `fetch_ok` accepts.
    /// Anything else — a bare host, a `${VAR}` — is not public.
    Location { local: bool },
    /// The map's keys name registries: each must be the public registry.
    /// The values are walked as usual.
    RegistryKeys,
}

/// Whether `value`, a string a registry key holds, names the ecosystem's
/// public registry itself.
fn registry_ok(value: &str, public: &Public) -> bool {
    matches!(Named::parse(value, public), Named::Public)
}

/// A local source that fetches nothing: a relative or absolute path, or
/// a `file:` URL.
fn local_path(value: &str) -> bool {
    let value = value.trim().trim_matches(['"', '\'']);
    // A relative path only: an absolute one, or a `file:` URL to one, could
    // name a directory outside the checkout, such as another run's cache.
    let path = match value.get(..5) {
        Some(start) if start.eq_ignore_ascii_case("file:") => &value[5..],
        _ => value,
    };
    (path.starts_with("./") || path.starts_with("../") || (value != path && !path.is_empty()))
        && !path.starts_with('/')
        && !path.starts_with('\\')
        && !path.contains(":\\")
}

/// Whether `value`, a string a location key holds, is a local path where
/// `local` allows one, or a source carrying a scheme that fetches only
/// public or local content.
fn location_ok(value: &str, public: &Public, local: bool) -> bool {
    if local && local_path(value) {
        return true;
    }
    let value = value.trim().trim_matches(['"', '\'']);
    value.contains(':') && fetch_ok(value, public)
}

/// Whether `path` plus `key` is `table` or inside it.
fn within(path: &[String], key: &str, table: &[&str]) -> bool {
    table.len() <= path.len() + 1
        && table
            .iter()
            .zip(path.iter().map(String::as_str).chain([key]))
            .all(|(wanted, part)| *wanted == part)
}

/// `path` below `prefix`, or `None` when it is not under it.
fn strip<'a>(path: &'a [String], prefix: &[&str]) -> Option<&'a [String]> {
    (path.len() >= prefix.len()
        && path[..prefix.len()]
            .iter()
            .zip(prefix)
            .all(|(part, wanted)| part == wanted))
    .then(|| &path[prefix.len()..])
}

fn no_rules(_: &[String], _: &str) -> Rule {
    Rule::Walk
}
fn package_json_rule(path: &[String], key: &str) -> Rule {
    if path.is_empty() && PACKAGE_JSON_INERT.contains(&key) {
        Rule::Inert
    } else {
        Rule::Walk
    }
}
/// `composer.json`'s keys: inert metadata, and `repositories`, which
/// `composer_repositories_public` rules on entry by entry.
fn composer_rule(path: &[String], key: &str) -> Rule {
    if path.is_empty() && (COMPOSER_INERT.contains(&key) || key == "repositories") {
        Rule::Inert
    } else {
        Rule::Walk
    }
}
/// `[tool]` tables whose strings never steer a fetch, at any depth below
/// them: the `fancy-pypi-readme` metadata hook, which writes URLs into a
/// readme, and the pure-lint tool tables. `[tool.*]` in general is not
/// skipped: build backends read their own tables, which can hold URLs.
const TOOL_INERT: &[&[&str]] = &[
    &["tool", "hatch", "metadata", "hooks", "fancy-pypi-readme"],
    &["tool", "ruff"],
    &["tool", "black"],
    &["tool", "isort"],
    &["tool", "mypy"],
    &["tool", "pytest"],
    &["tool", "coverage"],
    &["tool", "pyright"],
    &["tool", "towncrier"],
    &["tool", "codespell"],
];
/// `key` at `path` below a uv settings table — `[tool.uv]` in a
/// pyproject.toml or uv.toml's root — or its `[pip]` table: index URLs
/// must name PyPI itself, and `find-links` may also be a local
/// directory.
fn uv_keys(path: &[String], key: &str) -> Rule {
    if matches!(path, [table] if table == "index") && key == "url" {
        return Rule::Registry;
    }
    if path.is_empty() || matches!(path, [table] if table == "pip") {
        return match key {
            "index-url" | "extra-index-url" => Rule::Registry,
            "find-links" => Rule::Location { local: true },
            _ => Rule::Walk,
        };
    }
    Rule::Walk
}
/// The index settings a `[pypi-options]` table — pixi.toml's or
/// `[tool.pixi]`'s — holds.
fn pypi_options_key(key: &str) -> Rule {
    match key {
        "index-url" | "extra-index-urls" => Rule::Registry,
        "find-links" => Rule::Location { local: true },
        _ => Rule::Walk,
    }
}
fn pyproject_rule(path: &[String], key: &str) -> Rule {
    if matches!(path, [table] if table == "project") && PROJECT_INERT.contains(&key)
        || TOOL_INERT.iter().any(|table| within(path, key, table))
    {
        return Rule::Inert;
    }
    if let Some(below) = strip(path, &["tool", "uv"]) {
        return uv_keys(below, key);
    }
    if matches!(path.last().map(String::as_str), Some("pypi-options")) {
        return pypi_options_key(key);
    }
    // `[[tool.poetry.source]]` and `[[tool.pdm.source]]` name a package
    // index in `url`.
    if key == "url"
        && matches!(path, [a, b, c] if a == "tool"
            && (b == "poetry" || b == "pdm") && c == "source")
    {
        return Rule::Registry;
    }
    Rule::Walk
}
fn uv_rule(path: &[String], key: &str) -> Rule {
    uv_keys(path, key)
}
/// poetry.toml's `[repositories.<name>]` `url` names a package index.
fn poetry_rule(path: &[String], key: &str) -> Rule {
    if matches!(path, [a, _] if a == "repositories") && key == "url" {
        return Rule::Registry;
    }
    Rule::Walk
}
/// pdm.toml's `[pypi]` `url` is the package index.
fn pdm_rule(path: &[String], key: &str) -> Rule {
    if matches!(path, [table] if table == "pypi") && key == "url" {
        return Rule::Registry;
    }
    Rule::Walk
}
fn pixi_rule(path: &[String], key: &str) -> Rule {
    if matches!(path, [table] if table == "pypi-options") {
        return pypi_options_key(key);
    }
    Rule::Walk
}
/// `.yarnrc.yml`: `npmRegistryServer`, at the top level and under
/// `npmScopes`, and the registries `npmRegistries` is keyed by name the
/// npm registry or they name a private one.
fn yarnrc_rule(path: &[String], key: &str) -> Rule {
    if key == "npmRegistryServer"
        && (path.is_empty() || matches!(path, [scopes, _] if scopes == "npmScopes"))
    {
        return Rule::Registry;
    }
    if path.is_empty() && key == "npmRegistries" {
        return Rule::RegistryKeys;
    }
    Rule::Walk
}
/// pnpm-workspace.yaml: `.npmrc` settings in YAML — `registry` and
/// `@scope:registry` keys, and a `registries` map's values — name the
/// npm registry or they name a private one.
fn pnpm_rule(path: &[String], key: &str) -> Rule {
    if path.is_empty()
        && (key == "registry"
            || key
                .strip_suffix(":registry")
                .is_some_and(|scope| scope.starts_with('@') && scope.len() > 1))
        || matches!(path, [table] if table == "registries")
    {
        return Rule::Registry;
    }
    Rule::Walk
}
/// Cargo.toml: `[registries.<name>]` `index`, `[source.<name>]`
/// `registry` and a dependency's `registry-index` name the crates.io
/// index or they name a private one.
fn cargo_rule(path: &[String], key: &str) -> Rule {
    if (matches!(path, [table] if table == "package")
        || matches!(path, [parent, table] if parent == "workspace" && table == "package"))
        && CARGO_PACKAGE_INERT.contains(&key)
    {
        return Rule::Inert;
    }
    if key == "registry-index"
        || (matches!(path, [a, _] if a == "registries") && key == "index")
        || (matches!(path, [a, _] if a == "source") && key == "registry")
    {
        return Rule::Registry;
    }
    Rule::Walk
}

/// Whether `value` — the scalar or list a source key holds — passes
/// `check` in every string it is.
fn json_strings(value: &Value<'_>, check: &dyn Fn(&str) -> bool) -> bool {
    match value {
        Value::StringLit(lit) => check(&lit.value),
        Value::Array(array) => array
            .elements
            .iter()
            .all(|element| json_strings(element, check)),
        _ => false,
    }
}

/// Every key and string in a JSON (or JSONC, when `jsonc`) document,
/// recursively, with each key's [`Rule`] applied.
fn json_public(
    text: &str,
    jsonc: bool,
    public: &Public,
    rule: impl Fn(&[String], &str) -> Rule,
) -> bool {
    let Ok(root) = crate::npm::ast(text, jsonc) else {
        return false;
    };
    fn item(
        value: &Value<'_>,
        path: &mut Vec<String>,
        rule: &dyn Fn(&[String], &str) -> Rule,
        public: &Public,
    ) -> bool {
        match value {
            Value::Object(object) => object.properties.iter().all(|prop| {
                let key = prop.name.as_str();
                if !string_ok(key, public) {
                    return false;
                }
                let nested = |path: &mut Vec<String>| {
                    path.push(key.to_owned());
                    let ok = item(&prop.value, path, rule, public);
                    path.pop();
                    ok
                };
                match rule(path, key) {
                    Rule::Inert => true,
                    Rule::Registry => json_strings(&prop.value, &|s| registry_ok(s, public)),
                    Rule::Location { local } => {
                        json_strings(&prop.value, &|s| location_ok(s, public, local))
                    }
                    Rule::RegistryKeys => match &prop.value {
                        Value::Object(entries) => entries
                            .properties
                            .iter()
                            .all(|entry| registry_ok(entry.name.as_str(), public) && nested(path)),
                        _ => false,
                    },
                    Rule::Walk => nested(path),
                }
            }),
            Value::Array(array) => array
                .elements
                .iter()
                .all(|element| item(element, path, rule, public)),
            Value::StringLit(lit) => string_ok(&lit.value, public),
            _ => true,
        }
    }
    item(&root, &mut Vec::new(), &rule, public)
}

/// Whether `item` — the scalar or list a source key holds — passes
/// `check` in every string it is.
fn toml_strings(item: &toml_edit::Item, check: &dyn Fn(&str) -> bool) -> bool {
    fn scalar(nested: &toml_edit::Value, check: &dyn Fn(&str) -> bool) -> bool {
        match nested {
            toml_edit::Value::Array(array) => array.iter().all(|v| scalar(v, check)),
            _ => nested.as_str().is_some_and(check),
        }
    }
    match item {
        toml_edit::Item::Value(value) => scalar(value, check),
        _ => false,
    }
}

/// Every key and string in a TOML document, recursively, with each key's
/// [`Rule`] applied.
fn toml_public(text: &str, public: &Public, rule: impl Fn(&[String], &str) -> Rule) -> bool {
    fn table(
        table: &dyn toml_edit::TableLike,
        path: &mut Vec<String>,
        rule: &dyn Fn(&[String], &str) -> Rule,
        public: &Public,
    ) -> bool {
        table.iter().all(|(key, nested)| {
            if !string_ok(key, public) {
                return false;
            }
            let nested_item = |path: &mut Vec<String>| {
                path.push(key.to_owned());
                let ok = item(nested, path, rule, public);
                path.pop();
                ok
            };
            match rule(path, key) {
                Rule::Inert => true,
                Rule::Registry => toml_strings(nested, &|s| registry_ok(s, public)),
                Rule::Location { local } => {
                    toml_strings(nested, &|s| location_ok(s, public, local))
                }
                Rule::RegistryKeys => {
                    let entries = match nested {
                        toml_edit::Item::Table(inner) => Some(inner as &dyn toml_edit::TableLike),
                        toml_edit::Item::Value(toml_edit::Value::InlineTable(inner)) => {
                            Some(inner as &dyn toml_edit::TableLike)
                        }
                        _ => None,
                    };
                    entries.is_some_and(|entries| {
                        entries.iter().all(|(name, _)| registry_ok(name, public))
                    }) && nested_item(path)
                }
                Rule::Walk => nested_item(path),
            }
        })
    }
    fn scalar(
        value: &toml_edit::Value,
        path: &mut Vec<String>,
        rule: &dyn Fn(&[String], &str) -> Rule,
        public: &Public,
    ) -> bool {
        match value {
            toml_edit::Value::InlineTable(inline) => table(inline, path, rule, public),
            toml_edit::Value::Array(array) => array
                .iter()
                .all(|nested| scalar(nested, path, rule, public)),
            _ => value.as_str().is_none_or(|s| string_ok(s, public)),
        }
    }
    fn item(
        value: &toml_edit::Item,
        path: &mut Vec<String>,
        rule: &dyn Fn(&[String], &str) -> Rule,
        public: &Public,
    ) -> bool {
        match value {
            toml_edit::Item::Table(inner) => table(inner, path, rule, public),
            toml_edit::Item::ArrayOfTables(tables) => {
                tables.iter().all(|inner| table(inner, path, rule, public))
            }
            toml_edit::Item::Value(inner) => scalar(inner, path, rule, public),
            toml_edit::Item::None => true,
        }
    }
    let Ok(document) = text.parse::<toml_edit::DocumentMut>() else {
        return false;
    };
    item(document.as_item(), &mut Vec::new(), &rule, public)
}

/// Whether `value` — the scalar or list a source key holds — passes
/// `check` in every string it is.
fn yaml_strings(value: &serde_yaml_ng::Value, check: &dyn Fn(&str) -> bool) -> bool {
    match value {
        serde_yaml_ng::Value::String(value) => check(value),
        serde_yaml_ng::Value::Sequence(items) => items.iter().all(|v| yaml_strings(v, check)),
        serde_yaml_ng::Value::Tagged(tagged) => yaml_strings(&tagged.value, check),
        _ => false,
    }
}

/// Every key and string in a YAML document, recursively, with each key's
/// [`Rule`] applied.
fn yaml_public(text: &str, public: &Public, rule: impl Fn(&[String], &str) -> Rule) -> bool {
    let Ok(root) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(text) else {
        return false;
    };
    fn item(
        value: &serde_yaml_ng::Value,
        path: &mut Vec<String>,
        rule: &dyn Fn(&[String], &str) -> Rule,
        public: &Public,
    ) -> bool {
        match value {
            serde_yaml_ng::Value::Mapping(map) => map.iter().all(|(key, value)| {
                let Some(name) = key.as_str() else {
                    return item(value, path, rule, public);
                };
                if !string_ok(name, public) {
                    return false;
                }
                let nested = |path: &mut Vec<String>| {
                    path.push(name.to_owned());
                    let ok = item(value, path, rule, public);
                    path.pop();
                    ok
                };
                match rule(path, name) {
                    Rule::Inert => true,
                    Rule::Registry => yaml_strings(value, &|s| registry_ok(s, public)),
                    Rule::Location { local } => {
                        yaml_strings(value, &|s| location_ok(s, public, local))
                    }
                    Rule::RegistryKeys => match value {
                        serde_yaml_ng::Value::Mapping(entries) => entries.iter().all(|(k, v)| {
                            k.as_str().is_some_and(|k| registry_ok(k, public)) && {
                                path.push(name.to_owned());
                                let ok = item(v, path, rule, public);
                                path.pop();
                                ok
                            }
                        }),
                        _ => false,
                    },
                    Rule::Walk => nested(path),
                }
            }),
            serde_yaml_ng::Value::Sequence(items) => {
                items.iter().all(|v| item(v, path, rule, public))
            }
            serde_yaml_ng::Value::String(value) => string_ok(value, public),
            serde_yaml_ng::Value::Tagged(tagged) => item(&tagged.value, path, rule, public),
            _ => true,
        }
    }
    item(&root, &mut Vec::new(), &rule, public)
}

/// Every tag name, attribute and text node of one XML element, its
/// comments and processing instructions excepted — they steer nothing.
fn xml_ok(node: roxmltree::Node<'_, '_>, inert: &[&str], public: &Public) -> bool {
    if !node.is_element() {
        return true;
    }
    if inert
        .iter()
        .any(|name| node.tag_name().name().eq_ignore_ascii_case(name))
    {
        return string_ok(node.tag_name().name(), public);
    }
    node.attributes().all(|attribute| {
        string_ok(attribute.name(), public) && string_ok(attribute.value(), public)
    }) && node.children().all(|child| {
        if child.is_comment() || child.is_pi() {
            return true;
        }
        if child.is_element() {
            return xml_ok(child, inert, public);
        }
        child.text().is_none_or(|text| string_ok(text, public))
    })
}

/// Every tag name, attribute and text node in an XML document.
fn xml_public(text: &str, public: &Public, inert: &[&str]) -> bool {
    roxmltree::Document::parse(text)
        .is_ok_and(|document| xml_ok(document.root_element(), inert, public))
}

/// Every token of a line-grammar file — a `yarn.lock`, `Gemfile.lock`,
/// `.sln` or other text lock — checked for a fetch source that is not
/// public or local.
pub(crate) fn lines_public(text: &str, public: &Public) -> bool {
    string_ok(text, public)
}

/// An `.npmrc`'s lines: a `key=value` setting's key and value, or the
/// tokens of a line of another shape. `registry` and `@scope:registry`
/// values must name the public registry itself — a `${VAR}` placeholder
/// could resolve to anything at run time.
fn npmrc_public(text: &str) -> bool {
    let public = &crate::npm::NPM;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        match line.split_once('=') {
            Some((key, value)) => {
                let key = key.trim();
                if !string_ok(key, public) {
                    return false;
                }
                let registry = key == "registry"
                    || key
                        .strip_suffix(":registry")
                        .is_some_and(|scope| scope.starts_with('@') && scope.len() > 1);
                if registry {
                    if !matches!(Named::parse(value, public), Named::Public) {
                        return false;
                    }
                } else if !string_ok(value, public) {
                    return false;
                }
            }
            None if !string_ok(line, public) => return false,
            None => {}
        }
    }
    true
}

/// A `composer.json`: its `repositories` entries, its other strings, and
/// its `extra.merge-plugin`, which reads repositories from files the
/// Count does not see.
fn composer_json_public(text: &str) -> bool {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    json.pointer("/extra/merge-plugin").is_none()
        && composer_repositories_public(&json)
        && json_public(text, false, &crate::composer::PACKAGIST, composer_rule)
}

/// A `composer.json`'s `repositories`: a `"packagist.org": false`-style
/// switch, a `composer` repository naming Packagist, and `path` and
/// `artifact` repositories naming local paths are public. Any other type
/// — `vcs`, `git`, `package` and the rest — or a value the Count cannot
/// rule on, such as a scheme-less URL Composer would prepend a scheme to,
/// is not.
fn composer_repositories_public(json: &serde_json::Value) -> bool {
    let Some(repositories) = json.get("repositories") else {
        return true;
    };
    let entries: Vec<(Option<&str>, &serde_json::Value)> = match repositories {
        serde_json::Value::Array(entries) => entries.iter().map(|e| (None, e)).collect(),
        serde_json::Value::Object(entries) => entries
            .iter()
            .map(|(key, entry)| (Some(key.as_str()), entry))
            .collect(),
        _ => return false,
    };
    let packagist = |key: &str| matches!(key, "packagist.org" | "packagist");
    for (key, entry) in entries {
        if let Some(key) = key
            && !string_ok(key, &crate::composer::PACKAGIST)
        {
            return false;
        }
        let toggle = match (key, entry) {
            (Some(key), serde_json::Value::Bool(_)) if packagist(key) => true,
            (_, serde_json::Value::Object(map)) if map.len() == 1 => map
                .iter()
                .next()
                .is_some_and(|(key, on)| packagist(key) && on.is_boolean()),
            _ => false,
        };
        if toggle {
            continue;
        }
        let Some(map) = entry.as_object() else {
            return false;
        };
        let url = map.get("url").and_then(|url| url.as_str());
        let public = match map.get("type").and_then(|t| t.as_str()) {
            // `url` defaults to Packagist.
            Some("composer") => url.is_none_or(|url| registry_ok(url, &crate::composer::PACKAGIST)),
            Some("path" | "artifact") => url.is_some_and(local_path),
            _ => false,
        };
        if !public {
            return false;
        }
    }
    true
}

/// A `composer.lock`: its strings, and that it holds no Composer plugin
/// package, which can add repositories and run code the file does not
/// name.
fn composer_lock_public(text: &str) -> bool {
    crate::composer::lock_has_plugins(text) == Some(false)
        && json_public(text, false, &crate::composer::PACKAGIST, no_rules)
}

/// An MSBuild file: no property that moves restore sources, no element
/// that runs code (`<Target>`, `<UsingTask>`, `<Exec>`), and — as XML,
/// or token by token when it is not XML, such as a response file — no
/// fetch source that is not public or local.
fn msbuild_public(text: &str) -> bool {
    let lower = crate::nuget::decoded(text).to_ascii_lowercase();
    if crate::nuget::RESTORE_OVERRIDES
        .iter()
        .any(|property| lower.contains(property))
        || ["target", "usingtask", "exec"]
            .iter()
            .any(|name| crate::nuget::has_element(&lower, name))
    {
        return false;
    }
    match roxmltree::Document::parse(text) {
        Ok(document) => xml_ok(
            document.root_element(),
            MSBUILD_INERT,
            &crate::nuget::NUGET_ORG,
        ),
        Err(_) => lines_public(text, &crate::nuget::NUGET_ORG),
    }
}

/// A `nuget.config`: every attribute and text node, with each
/// `packageSources` `add` value required to be a local folder or the
/// public feed — a bare host NuGet would fetch over its default scheme
/// is not public.
fn nuget_config_public(text: &str) -> bool {
    let Ok(document) = roxmltree::Document::parse(text) else {
        return false;
    };
    fn item(node: roxmltree::Node<'_, '_>, in_sources: bool, public: &Public) -> bool {
        if !node.is_element() {
            return true;
        }
        let tag = node.tag_name().name();
        node.attributes().all(|attribute| {
            string_ok(attribute.name(), public)
                && if in_sources && attribute.name().eq_ignore_ascii_case("value") {
                    local_path(attribute.value()) || registry_ok(attribute.value(), public)
                } else {
                    string_ok(attribute.value(), public)
                }
        }) && node.children().all(|child| {
            if child.is_comment() || child.is_pi() {
                return true;
            }
            if child.is_element() {
                // NuGet reads element and attribute names in any case.
                return item(
                    child,
                    in_sources || tag.eq_ignore_ascii_case("packageSources"),
                    public,
                );
            }
            child.text().is_none_or(|text| string_ok(text, public))
        })
    }
    item(document.root_element(), false, &crate::nuget::NUGET_ORG)
}

/// Whether `path` can steer what a lockfile tool fetches, and if so
/// whether every fetch it can name is public-only. Files are decided one
/// at a time; verdicts on the settings files around a file come from the
/// runner checking those too, so each verdict covers the file alone —
/// plus, for a Gemfile, the lockfile sitting beside it that its `source`
/// lines and Bundler's resolution share.
/// An unread file can name anything, so it is never public, for every file type
/// a verdict covers.
pub(crate) fn verdict(path: &str, text: &crate::Text, files: &Files) -> Option<bool> {
    match text.read() {
        Ok(text) => read_verdict(path, text, files),
        // Asked of empty text only to learn whether the file type is covered.
        Err(_) => read_verdict(path, "", files).map(|_| false),
    }
}

fn read_verdict(path: &str, text: &str, files: &Files) -> Option<bool> {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    let flag = match leaf {
        "package.json" => json_public(text, false, &crate::npm::NPM, package_json_rule),
        ".npmrc" => npmrc_public(text),
        ".yarnrc.yml" => yaml_public(text, &crate::npm::NPM, yarnrc_rule),
        "pnpm-workspace.yaml" => yaml_public(text, &crate::npm::NPM, pnpm_rule),
        "pnpm-lock.yaml" | "aube-lock.yaml" => yaml_public(text, &crate::npm::NPM, no_rules),
        "package-lock.json" | "npm-shrinkwrap.json" => {
            json_public(text, false, &crate::npm::NPM, no_rules)
        }
        "bun.lock" => json_public(text, true, &crate::npm::NPM, no_rules),
        "yarn.lock" => lines_public(text, &crate::npm::NPM),
        "pyproject.toml" => toml_public(text, &crate::pypi::PYPI, pyproject_rule),
        "uv.toml" => toml_public(text, &crate::pypi::PYPI, uv_rule),
        "poetry.toml" => toml_public(text, &crate::pypi::PYPI, poetry_rule),
        "pdm.toml" => toml_public(text, &crate::pypi::PYPI, pdm_rule),
        "pixi.toml" => toml_public(text, &crate::pypi::PYPI, pixi_rule),
        "hatch.toml" | "uv.lock" | "poetry.lock" => toml_public(text, &crate::pypi::PYPI, no_rules),
        "Gemfile" => crate::bundler::public_only(path, text, files),
        "Gemfile.lock" => lines_public(text, &crate::bundler::RUBYGEMS),
        "composer.json" => composer_json_public(text),
        "composer.lock" => composer_lock_public(text),
        "Cargo.toml" => toml_public(text, &crate::cargo::CRATES, cargo_rule),
        "Cargo.lock" => toml_public(text, &crate::cargo::CRATES, no_rules),
        "packages.lock.json" | "global.json" | "dotnet-tools.json" => {
            json_public(text, false, &crate::nuget::NUGET_ORG, no_rules)
        }
        _ if crate::nuget::config_file(path) => nuget_config_public(text),
        _ if crate::nuget::msbuild_file(path) => msbuild_public(text),
        _ if leaf.ends_with(".slnx") => xml_public(text, &crate::nuget::NUGET_ORG, &[]),
        _ if leaf.ends_with(".sln") => lines_public(text, &crate::nuget::NUGET_ORG),
        _ => return None,
    };
    Some(flag)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Files are decided one at a time: a file is public-only when every
    /// fetch it can name is the public registry, a public forge or a
    /// local source, and it fails closed when the Count cannot read it.
    #[test]
    fn verdicts_fail_closed_on_anything_not_public() {
        let files = Files::from([
            ("package.json".into(), "{}".into()),
            (".npmrc".into(), "registry=https://registry.npmjs.org/\n".into()),
            ("package-lock.json".into(), "{\"packages\":{\"\":{\"resolved\":\"https://registry.npmjs.org/x/-/x-1.0.0.tgz\"}}}".into()),
        ]);
        assert_eq!(read_verdict("package.json", "{}", &files), Some(true));
        assert_eq!(
            read_verdict(".npmrc", "registry=https://registry.npmjs.org/\n", &files),
            Some(true)
        );
        // A private or unknown registry is not public.
        for text in [
            "registry=https://npm.acme.dev/\n",
            "registry=${NPM_REGISTRY}\n",
        ] {
            assert_eq!(
                read_verdict(".npmrc", text, &files),
                Some(false),
                "{text:?}"
            );
        }
        // Whatever the reason, an unread file is never public, and a file type
        // no verdict covers stays uncovered.
        for unread in [crate::Unread::Failed, crate::Unread::TooDeep] {
            assert_eq!(
                verdict("README.md", &crate::Text::Unread(unread), &files),
                None
            );
            for path in ["yarn.lock", "Gemfile.lock", "app.sln", ".npmrc", "Gemfile"] {
                assert_eq!(
                    verdict(path, &crate::Text::Unread(unread), &files),
                    Some(false),
                    "{path} {unread:?}"
                );
            }
        }
        // A lockfile `resolved` URL on a non-public host is not public.
        let lock = "{\"packages\":{\"\":{\"resolved\":\"https://npm.acme.dev/x.tgz\"}}}";
        assert_eq!(read_verdict("package-lock.json", lock, &files), Some(false));
        let lock = "{\"packages\":{\"\":{\"resolved\":\"https://registry.npmjs.org/x.tgz\"}}}";
        assert_eq!(read_verdict("package-lock.json", lock, &files), Some(true));
        // A Gemfury-style index URL is not public.
        let pyproject = "[tool.uv]\nindex-url = \"https://pypi.fury.io/acme/simple\"\n";
        assert_eq!(
            read_verdict("pyproject.toml", pyproject, &files),
            Some(false)
        );
        let pyproject = "[project]\ndependencies = [\"requests\"]\n";
        assert_eq!(
            read_verdict("pyproject.toml", pyproject, &files),
            Some(true)
        );
        // A non-steering file has no verdict.
        assert_eq!(read_verdict("README.md", "hi", &files), None);
    }

    #[test]
    fn fetch_urls_split_public_from_private() {
        let npm = crate::origins::Public {
            source: patchula_words::Source::Npm,
            label: "npm",
            is_public: |url| {
                crate::web_host(url).is_some_and(|host| {
                    matches!(host.as_str(), "registry.npmjs.org" | "registry.yarnpkg.com")
                })
            },
        };
        // Built at runtime so the fixture scan finds no credential text.
        let passworded = format!(
            "https://ci:{}@github.com/acme/x.git",
            "s3".to_owned() + "cret"
        );
        let ssh_passworded = format!("ssh://git:{}@github.com/acme/x.git", "p".to_owned() + "w");
        for (value, expect) in [
            (
                "https://registry.npmjs.org/lodash/-/lodash-4.17.21.tgz",
                true,
            ),
            ("https://npm.acme.dev/lodash/-/lodash-4.17.21.tgz", false),
            ("git+ssh://git@github.com/acme/x.git", true),
            ("git+ssh://git@gitlab.acme.dev/acme/x.git", false),
            ("git@github.com:acme/x.git", true),
            ("ssh://git@github.com/acme/x.git", true),
            ("https://github.com/acme/x.git", true),
            ("https://gitlab.acme.dev/acme/x.git", false),
            ("lodash@npm:4.17.21", true),
            ("x@jsr:@scope/x", false),
            ("file:../x", true),
            ("workspace:*", true),
            ("portal:../x", true),
            ("sha512-deadbeef", true),
            ("1.2.3", true),
            // A user alone still authenticates: forges take a token as the
            // username, so only an all-placeholder userinfo is public.
            ("https://user@bitbucket.org/acme/x.git", false),
            (passworded.as_str(), false),
            ("https://token@github.com/acme/x.git", false),
            ("https://ci:${TOKEN}@github.com/acme/x.git", false),
            ("https://${TOKEN}@github.com/acme/x.git", true),
            ("https://$TOKEN@github.com/acme/x.git", true),
            ("https://$(TOKEN)@github.com/acme/x.git", true),
            ("https://%TOKEN%@github.com/acme/x.git", true),
            (ssh_passworded.as_str(), false),
        ] {
            assert_eq!(fetch_ok(value, &npm), expect, "{value}");
        }
    }

    /// Every format's verdict: a realistic public file, and the private
    /// fetch sources — anywhere in it — that make it not public.
    #[test]
    fn every_string_decides_the_verdict() {
        let files = Files::new();
        let cases: &[(&str, &str, bool)] = &[
            // package.json: realistic, then a git dep and a private
            // specifier anywhere in it.
            (
                "package.json",
                "{\"name\":\"app\",\"dependencies\":{\"x\":\"^1.0.0\",\"y\":\"file:../y\",\"z\":\"workspace:*\"},\"scripts\":{\"test\":\"node --test\"}}",
                true,
            ),
            (
                "package.json",
                "{\"dependencies\":{\"x\":\"git+https://git.acme.dev/x.git\"}}",
                false,
            ),
            (
                "package.json",
                "{\"overrides\":{\"a\":{\"b\":{\"x\":\"x@npm:https://npm.acme.dev/x.tgz\"}}}}",
                false,
            ),
            // An inert `homepage` — metadata, never a fetch — stays
            // public.
            (
                "package.json",
                "{\"name\":\"app\",\"homepage\":\"https://acme.dev\",\"repository\":{\"url\":\"https://git.acme.dev/x\"},\"description\":\"see https://acme.dev/docs\"}",
                true,
            ),
            // .npmrc.
            (
                ".npmrc",
                "registry=https://registry.npmjs.org/\n@acme:registry=https://registry.yarnpkg.com\nstrict-ssl=false\n",
                true,
            ),
            (".npmrc", "@acme:registry=https://npm.acme.dev/\n", false),
            (".npmrc", "https-proxy=http://proxy.acme.dev:8080\n", false),
            // .yarnrc.yml: registry settings plus Yarn's catalogs and
            // package extensions, which can name specifiers.
            // `npmRegistryServer` and the registries `npmRegistries` is
            // keyed by must name the public registry itself — a bare
            // host, a private one or a `${VAR}` is not public.
            (
                ".yarnrc.yml",
                "npmRegistryServer: \"https://registry.npmjs.org\"\nnodeLinker: node-modules\n",
                true,
            ),
            (
                ".yarnrc.yml",
                "npmRegistryServer: \"https://registry.yarnpkg.com\"\n",
                true,
            ),
            (
                ".yarnrc.yml",
                "npmRegistryServer: registry.acme.dev\n",
                false,
            ),
            (
                ".yarnrc.yml",
                "npmRegistryServer: \"${NPM_REGISTRY}\"\n",
                false,
            ),
            (
                ".yarnrc.yml",
                "npmRegistries:\n  \"https://npm.acme.dev\":\n    npmAlwaysAuth: true\n",
                false,
            ),
            (
                ".yarnrc.yml",
                "npmRegistries:\n  \"https://registry.npmjs.org\":\n    npmAlwaysAuth: true\n",
                true,
            ),
            (
                ".yarnrc.yml",
                "catalog:\n  x: \"git+https://git.acme.dev/x.git\"\n",
                false,
            ),
            (
                ".yarnrc.yml",
                "packageExtensions:\n  \"x@*\":\n    dependencies:\n      y: \"https://npm.acme.dev/y.tgz\"\n",
                false,
            ),
            (
                ".yarnrc.yml",
                "npmScopes:\n  acme:\n    npmRegistryServer: \"https://npm.acme.dev\"\n",
                false,
            ),
            (
                ".yarnrc.yml",
                "npmScopes:\n  acme:\n    npmRegistryServer: npm.acme.dev\n",
                false,
            ),
            // pnpm-workspace.yaml: catalogs, and the `.npmrc` registry
            // settings it can hold, which must name the public registry.
            (
                "pnpm-workspace.yaml",
                "packages:\n  - 'packages/*'\ncatalog:\n  react: ^19.0.0\n",
                true,
            ),
            (
                "pnpm-workspace.yaml",
                "packages:\n  - 'packages/*'\ncatalog:\n  x: \"git+ssh://git@git.acme.dev/x.git\"\n",
                false,
            ),
            (
                "pnpm-workspace.yaml",
                "packages:\n  - 'packages/*'\nregistry: https://npm.acme.dev\n",
                false,
            ),
            ("pnpm-workspace.yaml", "registry: npm.acme.dev\n", false),
            (
                "pnpm-workspace.yaml",
                "registry: https://registry.npmjs.org\n'@acme:registry': ${NPM_REGISTRY}\n",
                false,
            ),
            (
                "pnpm-workspace.yaml",
                "registry: https://registry.npmjs.org\n",
                true,
            ),
            (
                "pnpm-workspace.yaml",
                "registries:\n  acme: https://npm.acme.dev\n",
                false,
            ),
            // package-lock.json: v3 resolved URLs, and v1's `version` and
            // `from`, which can be git URLs.
            (
                "package-lock.json",
                "{\"lockfileVersion\":3,\"packages\":{\"\":{\"name\":\"app\"},\"node_modules/x\":{\"version\":\"1.0.0\",\"resolved\":\"https://registry.npmjs.org/x/-/x-1.0.0.tgz\",\"integrity\":\"sha512-x\"}}}",
                true,
            ),
            (
                "package-lock.json",
                "{\"lockfileVersion\":1,\"dependencies\":{\"x\":{\"version\":\"git+ssh://git@git.acme.dev/x.git#abc\"}}}",
                false,
            ),
            (
                "package-lock.json",
                "{\"lockfileVersion\":1,\"dependencies\":{\"x\":{\"version\":\"1.0.0\",\"from\":\"x@https://npm.acme.dev/x.tgz\"}}}",
                false,
            ),
            // bun.lock: JSONC; `packages` tuples name a tarball URL.
            (
                "bun.lock",
                "{\n  \"lockfileVersion\": 1,\n  \"workspaces\": {\"\": {\"name\": \"app\"}},\n  \"packages\": {\n    \"x\": [\"x@1.0.0\", \"https://registry.npmjs.org/x/-/x-1.0.0.tgz\", {}, \"sha512-x\"],\n  },\n}\n",
                true,
            ),
            (
                "bun.lock",
                "{\"lockfileVersion\": 1, \"packages\": {\"x\": [\"x@https://npm.acme.dev/x.tgz\", \"https://npm.acme.dev/x.tgz\", {}, \"sha512-x\"]}}",
                false,
            ),
            // yarn.lock.
            (
                "yarn.lock",
                "x@^1.0.0:\n  version \"1.0.0\"\n  resolved \"https://registry.npmjs.org/x/-/x-1.0.0.tgz#abc\"\n  integrity sha512-x\n",
                true,
            ),
            (
                "yarn.lock",
                "\"x@npm:^1.0.0\":\n  version: 1.0.0\n  resolution: \"x@npm:1.0.0\"\n  checksum: abc\n",
                true,
            ),
            (
                "yarn.lock",
                "x@^1.0.0:\n  version \"1.0.0\"\n  resolved \"https://npm.acme.dev/x/-/x-1.0.0.tgz\"\n",
                false,
            ),
            (
                "yarn.lock",
                "\"x@git+ssh://git@git.acme.dev/x.git\":\n  version: 1.0.0\n  resolution: \"x@git+ssh://git@git.acme.dev/x.git#abc\"\n",
                false,
            ),
            // pnpm-lock.yaml / aube-lock.yaml.
            (
                "pnpm-lock.yaml",
                "lockfileVersion: '9.0'\npackages:\n  x@1.0.0:\n    resolution: {integrity: sha512-x}\n",
                true,
            ),
            (
                "aube-lock.yaml",
                "packages:\n  x@1.0.0:\n    resolution: {tarball: \"https://npm.acme.dev/x.tgz\"}\n",
                false,
            ),
            // Cargo.toml: a workspace manifest; git deps wherever they
            // nest, patch tables, workspace dependencies and named
            // registries all steer.
            (
                "Cargo.toml",
                "[package]\nname = \"app\"\nversion = \"1.0.0\"\nhomepage = \"https://acme.dev\"\n\n[dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\nlocal = { path = \"../local\" }\n",
                true,
            ),
            (
                "Cargo.toml",
                "[package]\nname = \"app\"\n\n[dependencies]\nx = { git = \"https://git.acme.dev/x.git\" }\n",
                false,
            ),
            (
                "Cargo.toml",
                "[package]\nname = \"app\"\n\n[patch.crates-io]\nx = { git = \"https://git.acme.dev/x.git\" }\n",
                false,
            ),
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.dependencies]\nx = { git = \"https://git.acme.dev/x.git\" }\n",
                false,
            ),
            (
                "Cargo.toml",
                "[package]\nname = \"app\"\n\n[registries.acme]\nindex = \"https://index.acme.dev\"\n",
                false,
            ),
            // A registry index must name the crates.io index itself —
            // a bare host is not public either — and so must a
            // dependency's `registry-index`.
            (
                "Cargo.toml",
                "[package]\nname = \"app\"\n\n[registries.acme]\nindex = \"index.acme.dev\"\n",
                false,
            ),
            (
                "Cargo.toml",
                "[package]\nname = \"app\"\n\n[registries.acme]\nindex = \"https://index.crates.io/\"\n",
                true,
            ),
            (
                "Cargo.toml",
                "[package]\nname = \"app\"\n\n[dependencies]\nx = { version = \"1\", registry-index = \"index.acme.dev\" }\n",
                false,
            ),
            (
                "Cargo.toml",
                "[package]\nname = \"app\"\n\n[source.acme]\nregistry = \"https://index.acme.dev\"\n",
                false,
            ),
            // Cargo.lock: `source` strings and `(registry+...)` dep
            // qualifiers.
            (
                "Cargo.lock",
                "version = 4\n\n[[package]]\nname = \"x\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\ndependencies = [\n \"y 2.0 (sparse+https://index.crates.io/)\",\n]\n",
                true,
            ),
            (
                "Cargo.lock",
                "version = 4\n\n[[package]]\nname = \"x\"\nversion = \"1.0.0\"\nsource = \"git+https://git.acme.dev/x.git#abc\"\n",
                false,
            ),
            // pyproject.toml: PEP 508 requirements, uv and Poetry tables.
            (
                "pyproject.toml",
                "[project]\nname = \"app\"\ndependencies = [\"requests\"]\ndescription = \"see https://acme.dev\"\n\n[project.urls]\nHomepage = \"https://acme.dev\"\n",
                true,
            ),
            (
                "pyproject.toml",
                "[tool.uv]\ndev-dependencies = [\"x @ https://acme.dev/x.whl\"]\n",
                false,
            ),
            (
                "pyproject.toml",
                "[tool.uv]\nbuild-constraint-dependencies = [\"x @ https://acme.dev/x.whl\"]\n",
                false,
            ),
            (
                "pyproject.toml",
                "[tool.uv]\nextra-build-dependencies = [\"x @ https://acme.dev/x.whl\"]\n",
                false,
            ),
            (
                "pyproject.toml",
                "[tool.uv.sources]\nx = { git = \"https://git.acme.dev/x\" }\n",
                false,
            ),
            (
                "pyproject.toml",
                "[[tool.uv.index]]\nname = \"acme\"\nurl = \"https://pypi.acme.dev/simple\"\n",
                false,
            ),
            // An index URL must name PyPI itself: a bare host is not
            // public either.
            (
                "pyproject.toml",
                "[[tool.uv.index]]\nname = \"acme\"\nurl = \"pypi.acme.dev/simple\"\n",
                false,
            ),
            (
                "pyproject.toml",
                "[[tool.uv.index]]\nname = \"mirror\"\nurl = \"https://pypi.org/simple\"\nexplicit = true\n",
                true,
            ),
            (
                "pyproject.toml",
                "[tool.uv]\nindex-url = \"https://pypi.org/simple\"\nextra-index-url = [\"pypi.acme.dev/simple\"]\n",
                false,
            ),
            (
                "pyproject.toml",
                "[tool.uv]\nindex-url = \"https://pypi.org/simple\"\nfind-links = \"./wheels\"\n",
                true,
            ),
            (
                "pyproject.toml",
                "[tool.uv]\nfind-links = \"https://wheels.acme.dev\"\n",
                false,
            ),
            (
                "pyproject.toml",
                "[tool.uv.pip]\nindex-url = \"pypi.acme.dev/simple\"\n",
                false,
            ),
            (
                "pyproject.toml",
                "[[tool.poetry.source]]\nname = \"acme\"\nurl = \"https://pypi.acme.dev/simple\"\npriority = \"primary\"\n",
                false,
            ),
            (
                "pyproject.toml",
                "[[tool.poetry.source]]\nname = \"acme\"\nurl = \"pypi.acme.dev/simple\"\n",
                false,
            ),
            (
                "pyproject.toml",
                "[[tool.pdm.source]]\nname = \"acme\"\nurl = \"https://pypi.acme.dev/simple\"\n",
                false,
            ),
            (
                "pyproject.toml",
                "[tool.poetry.dependencies]\nx = { git = \"https://git.acme.dev/x.git\" }\n",
                false,
            ),
            // The readme hook's fragments and the pure-lint tool tables
            // hold URLs that steer nothing; a build backend's table does,
            // and stays checked.
            (
                "pyproject.toml",
                "[tool.hatch.metadata.hooks.fancy-pypi-readme]\nfragments = [{text = \"see https://acme.dev/docs\"}]\nsubstitutions = [{pattern = 'x', replacement = '[x](https://acme.dev/x)'}]\n",
                true,
            ),
            (
                "pyproject.toml",
                "[tool.ruff]\nextend-exclude = [\"x\"]\n\n[tool.pyright]\nignore = [\"https://acme.dev\"]\n",
                true,
            ),
            (
                "pyproject.toml",
                "[tool.flit.metadata]\nhome-page = \"https://acme.dev\"\n",
                false,
            ),
            // uv.lock / poetry.lock.
            (
                "uv.lock",
                "version = 1\n\n[[package]]\nname = \"x\"\nversion = \"1.0.0\"\nsource = { registry = \"https://pypi.org/simple\" }\nsdist = { url = \"https://files.pythonhosted.org/packages/x.tar.gz\" }\n",
                true,
            ),
            (
                "uv.lock",
                "version = 1\n\n[[package]]\nname = \"x\"\nversion = \"1.0.0\"\nsource = { git = \"https://git.acme.dev/x\", rev = \"abc\" }\n",
                false,
            ),
            (
                "poetry.lock",
                "[[package]]\nname = \"x\"\nversion = \"1.0.0\"\n\n[package.source]\ntype = \"legacy\"\nurl = \"https://pypi.acme.dev/simple\"\n",
                false,
            ),
            (
                "poetry.lock",
                "[[package]]\nname = \"x\"\nversion = \"1.0.0\"\n\n[package.source]\ntype = \"git\"\nurl = \"https://github.com/acme/x\"\nreference = \"main\"\n",
                true,
            ),
            // uv.toml and the other Python settings files.
            ("uv.toml", "index-url = \"https://pypi.org/simple\"\n", true),
            (
                "uv.toml",
                "[[index]]\nurl = \"https://pypi.acme.dev/simple\"\n",
                false,
            ),
            (
                "pdm.toml",
                "[pypi]\nurl = \"https://pypi.acme.dev/simple\"\n",
                false,
            ),
            (
                "pixi.toml",
                "[pypi-options]\nindex-url = \"https://pypi.acme.dev/simple\"\n",
                false,
            ),
            (
                "hatch.toml",
                "[envs.default]\nenv-vars = { PIP_INDEX_URL = \"https://pypi.acme.dev/simple\" }\n",
                false,
            ),
            (
                "poetry.toml",
                "[http-basic.acme]\nusername = \"bot\"\n",
                true,
            ),
            // composer.json: repositories, and inert metadata. Only a
            // `composer` repository naming Packagist, a `path` or
            // `artifact` naming a local path, or a `"packagist.org":
            // false`-style switch is public; any other type is not, nor
            // is a scheme-less URL Composer would prepend a scheme to.
            (
                "composer.json",
                "{\"name\":\"acme/app\",\"description\":\"an app\",\"homepage\":\"https://acme.dev\",\"support\":{\"issues\":\"https://github.com/acme/app/issues\",\"source\":\"https://github.com/acme/app\"},\"require\":{\"php\":\"^8.3\"}}",
                true,
            ),
            (
                "composer.json",
                "{\"repositories\":[{\"type\":\"composer\",\"url\":\"packages.acme.dev\"}]}",
                false,
            ),
            (
                "composer.json",
                "{\"repositories\":[{\"type\":\"composer\",\"url\":\"https://repo.packagist.org\"}]}",
                true,
            ),
            (
                "composer.json",
                "{\"repositories\":[{\"type\":\"vcs\",\"url\":\"https://git.acme.dev/x\"}]}",
                false,
            ),
            (
                "composer.json",
                "{\"repositories\":[{\"type\":\"vcs\",\"url\":\"https://github.com/acme/x\"}]}",
                false,
            ),
            (
                "composer.json",
                "{\"repositories\":[{\"type\":\"path\",\"url\":\"../lib\"},{\"type\":\"artifact\",\"url\":\"./packages\"}]}",
                true,
            ),
            // An absolute artifact folder could be outside the checkout.
            (
                "composer.json",
                "{\"repositories\":[{\"type\":\"artifact\",\"url\":\"/srv/packages\"}]}",
                false,
            ),
            (
                "composer.json",
                "{\"repositories\":{\"packagist.org\":false}}",
                true,
            ),
            (
                "composer.json",
                "{\"repositories\":[{\"packagist.org\":false}]}",
                true,
            ),
            (
                "composer.json",
                "{\"repositories\":{\"acme\":{\"type\":\"package\",\"package\":{\"name\":\"x\"}}}}",
                false,
            ),
            (
                "composer.json",
                "{\"extra\":{\"merge-plugin\":{\"include\":[\"composer.local.json\"]}}}",
                false,
            ),
            // composer.lock.
            (
                "composer.lock",
                "{\"packages\":[{\"name\":\"acme/x\",\"version\":\"1.0.0\",\"dist\":{\"url\":\"https://repo.packagist.org/x.zip\",\"type\":\"zip\"},\"notification-url\":\"https://packagist.org/downloads/\"}]}",
                true,
            ),
            (
                "composer.lock",
                "{\"packages\":[{\"name\":\"acme/x\",\"version\":\"1.0.0\",\"dist\":{\"url\":\"https://php.acme.dev/x.zip\",\"type\":\"zip\"}}]}",
                false,
            ),
            (
                "composer.lock",
                "{\"packages\":[{\"name\":\"acme/x\",\"version\":\"1.0.0\",\"type\":\"composer-plugin\",\"notification-url\":\"https://packagist.org/downloads/\"}]}",
                false,
            ),
            // Gemfile and Gemfile.lock.
            (
                "Gemfile",
                "source \"https://rubygems.org\"\n\ngem \"rails\", \"~> 8.0\"\ngem \"releaser\", path: \"tools/releaser\"\n",
                true,
            ),
            (
                "Gemfile",
                "source \"https://gems.acme.dev\"\n\ngem \"rails\"\n",
                false,
            ),
            (
                "Gemfile",
                "source \"https://rubygems.org\"\n\nsource \"https://gems.acme.dev\" do\n  gem \"x\"\nend\n",
                false,
            ),
            (
                "Gemfile",
                "source \"https://rubygems.org\"\n\ngem \"x\", git: \"https://git.acme.dev/x.git\"\n",
                false,
            ),
            (
                "Gemfile.lock",
                "GEM\n  remote: https://rubygems.org/\n  specs:\n    x (1.0.0)\n\nPATH\n  remote: tools/releaser\n  specs:\n\nDEPENDENCIES\n  x\n",
                true,
            ),
            (
                "Gemfile.lock",
                "GIT\n  remote: https://git.acme.dev/x.git\n  revision: abc\n  specs:\n    x (1.0.0)\n",
                false,
            ),
            // nuget.config and the dotnet files.
            (
                "nuget.config",
                "<configuration>\n  <packageSources>\n    <add key=\"nuget.org\" value=\"https://api.nuget.org/v3/index.json\" />\n  </packageSources>\n</configuration>",
                true,
            ),
            (
                "nuget.config",
                "<configuration>\n  <packageSources>\n    <add key=\"acme\" value=\"https://nuget.acme.dev/v3/index.json\" />\n  </packageSources>\n</configuration>",
                false,
            ),
            // A package source is a local folder or the public feed; a
            // bare host is not public.
            (
                "nuget.config",
                "<configuration>\n  <packageSources>\n    <add key=\"local\" value=\"./packages\" />\n  </packageSources>\n</configuration>",
                true,
            ),
            // An absolute folder, a UNC share or a drive path could be
            // outside the checkout.
            (
                "nuget.config",
                "<configuration>\n  <packageSources>\n    <add key=\"cache\" value=\"file:///srv/nuget\" />\n  </packageSources>\n</configuration>",
                false,
            ),
            // NuGet reads element and attribute names in any case.
            (
                "nuget.config",
                "<configuration>\n  <PackageSources>\n    <add key=\"share\" Value=\"\\\\fileserver\\share\" />\n  </PackageSources>\n</configuration>",
                false,
            ),
            (
                "nuget.config",
                "<configuration>\n  <PACKAGESOURCES>\n    <add key=\"d\" value=\"D:\\feed\" />\n  </PACKAGESOURCES>\n</configuration>",
                false,
            ),
            (
                "nuget.config",
                "<configuration>\n  <packageSources>\n    <add key=\"acme\" value=\"nuget.acme.dev/v3/index.json\" />\n  </packageSources>\n</configuration>",
                false,
            ),
            (
                "app.csproj",
                "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <TargetFramework>net10.0</TargetFramework>\n    <PackageProjectUrl>https://acme.dev</PackageProjectUrl>\n  </PropertyGroup>\n  <ItemGroup>\n    <PackageReference Include=\"x\" Version=\"1.0.0\" />\n    <ProjectReference Include=\"..\\other\\other.csproj\" />\n  </ItemGroup>\n</Project>",
                true,
            ),
            (
                "app.csproj",
                "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <Target Name=\"x\" />\n</Project>",
                false,
            ),
            (
                "Directory.Build.props",
                "<Project>\n  <PropertyGroup>\n    <RestoreSources>https://nuget.acme.dev/v3/index.json</RestoreSources>\n  </PropertyGroup>\n</Project>",
                false,
            ),
            (
                "packages.lock.json",
                "{\"version\":1,\"dependencies\":{\"x\":{\"type\":\"Direct\",\"requested\":\"1.0.0\",\"resolved\":\"1.0.0\",\"contentHash\":\"abc\"}}}",
                true,
            ),
            (
                "global.json",
                "{\"sdk\":{\"version\":\"10.0.100\",\"rollForward\":\"latestMinor\"}}",
                true,
            ),
            (
                "dotnet-tools.json",
                "{\"version\":1,\"isRoot\":true,\"tools\":{\"x\":{\"version\":\"1.0.0\",\"commands\":[\"x\"]}}}",
                true,
            ),
            (
                "app.sln",
                "Microsoft Visual Studio Solution File, Format Version 12.00\nProject(\"{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}\") = \"app\", \"app\\app.csproj\", \"{1}\"\nEndProject\n",
                true,
            ),
            (
                "app.slnx",
                "<Solution>\n  <Project Path=\"app/app.csproj\" />\n</Solution>",
                true,
            ),
        ];
        for (path, text, expect) in cases {
            assert_eq!(
                read_verdict(path, text, &files),
                Some(*expect),
                "{path} {text:?}"
            );
        }
    }
}
