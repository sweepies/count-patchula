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
//! Only an explicit, small allowlist of inert metadata keys per format —
//! `package.json`'s `homepage`, `[project]`'s `urls` and their like —
//! goes unchecked: a URL there reaches a web page, not a download. A key
//! missing from the allowlist can only cost performance — the run lands
//! on the installation partition — never safety.
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
/// version, a hash, prose — is not a source and is not checked.
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

/// Whether `key` names inert metadata: for a key missing from the
/// allowlists the value is walked, which can only ever cost a run its
/// `public` partition, never make an unsafe file public.
fn no_inert(_: &[String], _: &str) -> bool {
    false
}
fn package_json_inert(path: &[String], key: &str) -> bool {
    path.is_empty() && PACKAGE_JSON_INERT.contains(&key)
}
fn composer_inert(path: &[String], key: &str) -> bool {
    path.is_empty() && COMPOSER_INERT.contains(&key)
}
fn project_inert(path: &[String], key: &str) -> bool {
    matches!(path, [table] if table == "project") && PROJECT_INERT.contains(&key)
}
fn cargo_package_inert(path: &[String], key: &str) -> bool {
    (matches!(path, [table] if table == "package")
        || matches!(path, [parent, table] if parent == "workspace" && table == "package"))
        && CARGO_PACKAGE_INERT.contains(&key)
}

/// Every key and string in a JSON (or JSONC, when `jsonc`) document,
/// recursively.
fn json_public(
    text: &str,
    jsonc: bool,
    public: &Public,
    inert: impl Fn(&[String], &str) -> bool,
) -> bool {
    let Ok(root) = crate::npm::ast(text, jsonc) else {
        return false;
    };
    fn item(
        value: &Value<'_>,
        path: &mut Vec<String>,
        inert: &dyn Fn(&[String], &str) -> bool,
        public: &Public,
    ) -> bool {
        match value {
            Value::Object(object) => object.properties.iter().all(|prop| {
                let key = prop.name.as_str();
                string_ok(key, public)
                    && (inert(path, key) || {
                        path.push(key.to_owned());
                        let ok = item(&prop.value, path, inert, public);
                        path.pop();
                        ok
                    })
            }),
            Value::Array(array) => array
                .elements
                .iter()
                .all(|element| item(element, path, inert, public)),
            Value::StringLit(lit) => string_ok(&lit.value, public),
            _ => true,
        }
    }
    item(&root, &mut Vec::new(), &inert, public)
}

/// Every key and string in a TOML document, recursively.
fn toml_public(text: &str, public: &Public, inert: impl Fn(&[String], &str) -> bool) -> bool {
    fn table(
        table: &dyn toml_edit::TableLike,
        path: &mut Vec<String>,
        inert: &dyn Fn(&[String], &str) -> bool,
        public: &Public,
    ) -> bool {
        table.iter().all(|(key, nested)| {
            string_ok(key, public)
                && (inert(path, key) || {
                    path.push(key.to_owned());
                    let ok = item(nested, path, inert, public);
                    path.pop();
                    ok
                })
        })
    }
    fn scalar(
        value: &toml_edit::Value,
        path: &mut Vec<String>,
        inert: &dyn Fn(&[String], &str) -> bool,
        public: &Public,
    ) -> bool {
        match value {
            toml_edit::Value::InlineTable(inline) => table(inline, path, inert, public),
            toml_edit::Value::Array(array) => array
                .iter()
                .all(|nested| scalar(nested, path, inert, public)),
            _ => value.as_str().is_none_or(|s| string_ok(s, public)),
        }
    }
    fn item(
        value: &toml_edit::Item,
        path: &mut Vec<String>,
        inert: &dyn Fn(&[String], &str) -> bool,
        public: &Public,
    ) -> bool {
        match value {
            toml_edit::Item::Table(inner) => table(inner, path, inert, public),
            toml_edit::Item::ArrayOfTables(tables) => {
                tables.iter().all(|inner| table(inner, path, inert, public))
            }
            toml_edit::Item::Value(inner) => scalar(inner, path, inert, public),
            toml_edit::Item::None => true,
        }
    }
    let Ok(document) = text.parse::<toml_edit::DocumentMut>() else {
        return false;
    };
    item(document.as_item(), &mut Vec::new(), &inert, public)
}

/// Every key and string in a YAML document, recursively.
fn yaml_public(text: &str, public: &Public) -> bool {
    let Ok(root) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(text) else {
        return false;
    };
    fn item(value: &serde_yaml_ng::Value, public: &Public) -> bool {
        match value {
            serde_yaml_ng::Value::Mapping(map) => map.iter().all(|(key, value)| {
                key.as_str().is_none_or(|key| string_ok(key, public)) && item(value, public)
            }),
            serde_yaml_ng::Value::Sequence(items) => items.iter().all(|v| item(v, public)),
            serde_yaml_ng::Value::String(value) => string_ok(value, public),
            serde_yaml_ng::Value::Tagged(tagged) => item(&tagged.value, public),
            _ => true,
        }
    }
    item(&root, public)
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

/// A `composer.json`: its strings, and its `extra.merge-plugin`, which
/// reads repositories from files the Count does not see.
fn composer_json_public(text: &str) -> bool {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    json.pointer("/extra/merge-plugin").is_none()
        && json_public(text, false, &crate::composer::PACKAGIST, composer_inert)
}

/// A `composer.lock`: its strings, and that it holds no Composer plugin
/// package, which can add repositories and run code the file does not
/// name.
fn composer_lock_public(text: &str) -> bool {
    crate::composer::lock_has_plugins(text) == Some(false)
        && json_public(text, false, &crate::composer::PACKAGIST, no_inert)
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

/// Whether `path` can steer what a lockfile tool fetches, and if so
/// whether every fetch it can name is public-only. Files are decided one
/// at a time; verdicts on the settings files around a file come from the
/// runner checking those too, so each verdict covers the file alone —
/// plus, for a Gemfile, the lockfile sitting beside it that its `source`
/// lines and Bundler's resolution share.
pub(crate) fn verdict(path: &str, text: &str, files: &Files) -> Option<bool> {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    let flag = match leaf {
        "package.json" => json_public(text, false, &crate::npm::NPM, package_json_inert),
        ".npmrc" => npmrc_public(text),
        ".yarnrc.yml" | "pnpm-workspace.yaml" | "pnpm-lock.yaml" | "aube-lock.yaml" => {
            yaml_public(text, &crate::npm::NPM)
        }
        "package-lock.json" | "npm-shrinkwrap.json" => {
            json_public(text, false, &crate::npm::NPM, no_inert)
        }
        "bun.lock" => json_public(text, true, &crate::npm::NPM, no_inert),
        "yarn.lock" => lines_public(text, &crate::npm::NPM),
        "pyproject.toml" => toml_public(text, &crate::pypi::PYPI, project_inert),
        "uv.toml" | "poetry.toml" | "pdm.toml" | "pixi.toml" | "hatch.toml" | "uv.lock"
        | "poetry.lock" => toml_public(text, &crate::pypi::PYPI, no_inert),
        "Gemfile" => crate::bundler::public_only(path, text, files),
        "Gemfile.lock" => lines_public(text, &crate::bundler::RUBYGEMS),
        "composer.json" => composer_json_public(text),
        "composer.lock" => composer_lock_public(text),
        "Cargo.toml" => toml_public(text, &crate::cargo::CRATES, cargo_package_inert),
        "Cargo.lock" => toml_public(text, &crate::cargo::CRATES, no_inert),
        "packages.lock.json" | "global.json" | "dotnet-tools.json" => {
            json_public(text, false, &crate::nuget::NUGET_ORG, no_inert)
        }
        _ if crate::nuget::config_file(path) => xml_public(text, &crate::nuget::NUGET_ORG, &[]),
        _ if crate::nuget::msbuild_file(path) => msbuild_public(text),
        _ if leaf.ends_with(".slnx") => xml_public(text, &crate::nuget::NUGET_ORG, &[]),
        _ if leaf.ends_with(".sln") => lines_public(text, &crate::nuget::NUGET_ORG),
        _ => return None,
    };
    // A file that could not be read can name anything.
    Some(flag && text != crate::UNREADABLE_SETTINGS)
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
        assert_eq!(verdict("package.json", "{}", &files), Some(true));
        assert_eq!(
            verdict(".npmrc", "registry=https://registry.npmjs.org/\n", &files),
            Some(true)
        );
        // A private or unknown registry is not public.
        for text in [
            "registry=https://npm.acme.dev/\n",
            "registry=${NPM_REGISTRY}\n",
            // A file the Count could not read.
            crate::UNREADABLE_SETTINGS,
        ] {
            assert_eq!(verdict(".npmrc", text, &files), Some(false), "{text:?}");
        }
        // A lockfile `resolved` URL on a non-public host is not public.
        let lock = "{\"packages\":{\"\":{\"resolved\":\"https://npm.acme.dev/x.tgz\"}}}";
        assert_eq!(verdict("package-lock.json", lock, &files), Some(false));
        let lock = "{\"packages\":{\"\":{\"resolved\":\"https://registry.npmjs.org/x.tgz\"}}}";
        assert_eq!(verdict("package-lock.json", lock, &files), Some(true));
        // A Gemfury-style index URL is not public.
        let pyproject = "[tool.uv]\nindex-url = \"https://pypi.fury.io/acme/simple\"\n";
        assert_eq!(verdict("pyproject.toml", pyproject, &files), Some(false));
        let pyproject = "[project]\ndependencies = [\"requests\"]\n";
        assert_eq!(verdict("pyproject.toml", pyproject, &files), Some(true));
        // A non-steering file has no verdict.
        assert_eq!(verdict("README.md", "hi", &files), None);
    }

    #[test]
    fn fetch_urls_split_public_from_private() {
        let npm = crate::origins::Public {
            source: patchula_config::Source::Npm,
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
            (
                ".yarnrc.yml",
                "npmRegistryServer: \"https://registry.npmjs.org\"\nnodeLinker: node-modules\n",
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
            // pnpm-workspace.yaml.
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
            (
                "pyproject.toml",
                "[[tool.poetry.source]]\nname = \"acme\"\nurl = \"https://pypi.acme.dev/simple\"\npriority = \"primary\"\n",
                false,
            ),
            (
                "pyproject.toml",
                "[tool.poetry.dependencies]\nx = { git = \"https://git.acme.dev/x.git\" }\n",
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
            // composer.json: repositories, and inert metadata.
            (
                "composer.json",
                "{\"name\":\"acme/app\",\"description\":\"an app\",\"homepage\":\"https://acme.dev\",\"support\":{\"issues\":\"https://github.com/acme/app/issues\",\"source\":\"https://github.com/acme/app\"},\"require\":{\"php\":\"^8.3\"}}",
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
                true,
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
                verdict(path, text, &files),
                Some(*expect),
                "{path} {text:?}"
            );
        }
    }
}
