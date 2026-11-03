use crate::*;
use jsonc_parser::{
    ast::{Object, Value},
    common::Ranged,
};

fn ast(text: &str, jsonc: bool) -> Result<Value<'_>, String> {
    let options = jsonc_parser::ParseOptions {
        allow_comments: jsonc,
        allow_trailing_commas: jsonc,
        allow_loose_object_property_names: false,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
        allow_bare_decimal_point_numbers: false,
        allow_non_finite_numbers: false,
        allow_extended_string_escapes: false,
    };
    jsonc_parser::parse_to_ast(text, &Default::default(), &options)
        .map_err(|e| e.to_string())?
        .value
        .ok_or_else(|| "empty JSON".into())
}
pub(super) fn locks(files: &Files) -> Result<BTreeMap<&str, Value<'_>>, String> {
    files
        .iter()
        .filter(|(path, _)| path.ends_with("package-lock.json") || path.ends_with("bun.lock"))
        .map(|(path, text)| Ok((path.as_str(), ast(text, path.ends_with("bun.lock"))?)))
        .collect()
}
fn escape(name: &str) -> String {
    name.replace('~', "~0").replace('/', "~1")
}
fn alias<'a>(name: &'a str, value: &'a str) -> (&'a str, &'a str) {
    value
        .strip_prefix("npm:")
        .and_then(|v| v.rsplit_once('@'))
        .unwrap_or((name, value))
}
fn registry(value: &str) -> bool {
    !value.starts_with(['$', '.', '/'])
        && !["workspace:", "file:", "link:", "catalog:", "git", "http"]
            .iter()
            .any(|p| value.starts_with(p))
        && !value.contains("github:")
}

pub(super) fn extract(
    path: &str,
    text: &str,
    manager: Manager,
    files: &BTreeMap<&str, Value<'_>>,
) -> Result<Vec<Dependency>, String> {
    let root = ast(text, false)?;
    let object = root.as_object().ok_or("package.json must be an object")?;
    let mut deps = Vec::new();
    for (field, kind) in [
        ("dependencies", DependencyKind::Production),
        ("devDependencies", DependencyKind::Development),
        ("peerDependencies", DependencyKind::Peer),
        ("optionalDependencies", DependencyKind::Optional),
    ] {
        if let Some(values) = object.get_object(field) {
            collect(
                path,
                text,
                manager,
                values,
                kind,
                &format!("/{field}"),
                false,
                &mut deps,
            );
        }
    }
    for field in ["overrides", "resolutions"] {
        if let Some(values) = object.get_object(field) {
            collect(
                path,
                text,
                manager,
                values,
                DependencyKind::Production,
                &format!("/{field}"),
                true,
                &mut deps,
            );
        }
    }
    // Bun declares catalogs in the root manifest, at the top level or under
    // `workspaces`; members refer to them with `catalog:` specifiers.
    let workspaces = object.get_object("workspaces");
    for (parent, base) in [(Some(object), ""), (workspaces, "/workspaces")] {
        let Some(parent) = parent else { continue };
        if let Some(values) = parent.get_object("catalog") {
            let pointer = format!("{base}/catalog");
            collect(
                path,
                text,
                manager,
                values,
                DependencyKind::Production,
                &pointer,
                false,
                &mut deps,
            );
        }
        if let Some(catalogs) = parent.get_object("catalogs") {
            for prop in &catalogs.properties {
                if let Some(values) = prop.value.as_object() {
                    let pointer = format!("{base}/catalogs/{}", escape(prop.name.as_str()));
                    collect(
                        path,
                        text,
                        manager,
                        values,
                        DependencyKind::Production,
                        &pointer,
                        false,
                        &mut deps,
                    );
                }
            }
        }
    }
    if let Some(pm) = object.get_string("packageManager")
        && let Some((name, value)) = pm.value.rsplit_once('@')
    {
        let (version, hash) = value
            .split_once('+')
            .map_or((value, None), |(v, h)| (v, Some(h.to_owned())));
        deps.push(Dependency {
            manager,
            file: path.into(),
            location: Location::new(text, pm.start(), pm.end(), "/packageManager".into()),
            name: name.into(),
            package: if name == "bun" { "oven-sh/bun" } else { name }.into(),
            kind: DependencyKind::Tool,
            current: version.into(),
            resolved: Some(version.into()),
            digest: hash,
            source: if name == "bun" {
                Source::GithubReleases
            } else {
                Source::Npm
            },
            scheme: Scheme::Semver,
            extract: None,
            included_by: None,
            registry: None,
        });
    }
    if let Some(version) = object
        .get_object("engines")
        .and_then(|engines| engines.get_string("node"))
    {
        deps.push(Dependency {
            manager,
            file: path.into(),
            location: Location::new(text, version.start(), version.end(), "/engines/node".into()),
            name: "node".into(),
            package: "node".into(),
            kind: DependencyKind::Toolchain,
            current: version.value.to_string(),
            resolved: None,
            digest: None,
            source: Source::Mise,
            scheme: Scheme::Semver,
            extract: None,
            included_by: None,
            registry: None,
        });
    }
    for dep in &mut deps {
        if dep.resolved.is_none() && dep.kind != DependencyKind::Toolchain {
            dep.resolved = resolved(path, &dep.name, files)?;
        }
    }
    Ok(deps)
}
#[allow(clippy::too_many_arguments)]
fn collect(
    path: &str,
    text: &str,
    manager: Manager,
    object: &Object<'_>,
    kind: DependencyKind,
    pointer: &str,
    nested: bool,
    deps: &mut Vec<Dependency>,
) {
    for prop in &object.properties {
        let name = prop.name.as_str();
        let ptr = format!("{pointer}/{}", escape(name));
        if let Some(value) = prop.value.as_string_lit() {
            if !registry(&value.value) {
                continue;
            }
            let override_name = if name == "." {
                pointer.rsplit('/').next().unwrap_or(name)
            } else {
                name
            };
            let override_name = override_name
                .rsplit_once('@')
                .filter(|(n, _)| !n.is_empty())
                .map_or(override_name, |(n, _)| n);
            let (package, current) = alias(override_name, &value.value);
            deps.push(Dependency {
                manager,
                file: path.into(),
                location: Location::new(text, value.start(), value.end(), ptr),
                name: if nested {
                    override_name.into()
                } else {
                    name.into()
                },
                package: package.into(),
                kind,
                current: current.into(),
                resolved: None,
                digest: None,
                source: Source::Npm,
                scheme: Scheme::Semver,
                extract: None,
                included_by: None,
                registry: None,
            });
        } else if nested && let Some(object) = prop.value.as_object() {
            collect(path, text, manager, object, kind, &ptr, true, deps);
        }
    }
}
fn resolved(
    path: &str,
    name: &str,
    files: &BTreeMap<&str, Value<'_>>,
) -> Result<Option<String>, String> {
    let mut directory = path.rsplit_once('/').map_or("", |(p, _)| p);
    loop {
        let prefix = if directory.is_empty() {
            String::new()
        } else {
            format!("{directory}/")
        };
        if let Some(root) = files.get(format!("{prefix}package-lock.json").as_str()) {
            if let Some(packages) = root.as_object().and_then(|v| v.get_object("packages")) {
                let relative = path
                    .strip_prefix(&prefix)
                    .unwrap_or(path)
                    .strip_suffix("package.json")
                    .unwrap_or("");
                for candidate in [
                    format!("{relative}node_modules/{name}"),
                    format!("node_modules/{name}"),
                ] {
                    if let Some(version) = packages
                        .get_object(&candidate)
                        .and_then(|v| v.get_string("version"))
                    {
                        return Ok(Some(version.value.to_string()));
                    }
                }
            }
            if let Some(v) = root
                .as_object()
                .and_then(|v| v.get_object("dependencies"))
                .and_then(|v| v.get_object(name))
                .and_then(|v| v.get_string("version"))
            {
                return Ok(Some(v.value.to_string()));
            }
        }
        if let Some(root) = files.get(format!("{prefix}bun.lock").as_str())
            && let Some(packages) = root.as_object().and_then(|v| v.get_object("packages"))
        {
            for key in [
                name.to_owned(),
                format!(
                    "{}/{name}",
                    path.strip_suffix("/package.json").unwrap_or("")
                ),
            ] {
                if let Some(value) = packages
                    .get_array(&key)
                    .and_then(|v| v.elements.first())
                    .and_then(Value::as_string_lit)
                    && let Some((_, v)) = value.value.rsplit_once('@')
                {
                    return Ok(Some(v.into()));
                }
            }
        }
        if directory.is_empty() {
            break;
        }
        directory = directory.rsplit_once('/').map_or("", |(p, _)| p);
    }
    Ok(None)
}

/// pnpm (`pnpm-workspace.yaml`) and Yarn (`.yarnrc.yml`) declare a default
/// `catalog` and named `catalogs` of package ranges for `catalog:` specifiers.
pub(super) fn catalogs(
    path: &str,
    text: &str,
    manager: Manager,
) -> Result<Vec<Dependency>, String> {
    let locations = crate::yaml_locations::parse(text)?;
    let Some(document) = crate::yaml::docs(text)?.into_iter().next() else {
        return Ok(Vec::new());
    };
    let mut groups = Vec::new();
    if let Some(catalog) = document.get("catalog").and_then(|v| v.as_mapping()) {
        groups.push(("/0/catalog".to_string(), catalog));
    }
    if let Some(named) = document.get("catalogs").and_then(|v| v.as_mapping()) {
        for (name, catalog) in named {
            if let (Some(name), Some(catalog)) = (name.as_str(), catalog.as_mapping()) {
                groups.push((format!("/0/catalogs/{}", escape(name)), catalog));
            }
        }
    }
    let mut deps = Vec::new();
    for (pointer, catalog) in groups {
        for (name, value) in catalog {
            let (Some(name), Some(value)) = (name.as_str(), value.as_str()) else {
                continue;
            };
            if !registry(value) {
                continue;
            }
            let pointer = format!("{pointer}/{}", escape(name));
            let location = locations
                .get(&pointer)
                .cloned()
                .ok_or_else(|| format!("cannot locate catalog entry {pointer}"))?;
            let (package, current) = alias(name, value);
            deps.push(Dependency {
                manager,
                file: path.into(),
                location,
                name: name.into(),
                package: package.into(),
                kind: DependencyKind::Production,
                current: current.into(),
                resolved: None,
                digest: None,
                source: Source::Npm,
                scheme: Scheme::Semver,
                extract: None,
                included_by: None,
                registry: None,
            });
        }
    }
    Ok(deps)
}

/// `.nvmrc` and `.node-version` pin Node with a single version, optionally
/// `v`-prefixed. Aliases such as `lts/*` or `node` are not versions.
pub(super) fn node_version(path: &str, text: &str, manager: Manager) -> Vec<Dependency> {
    let Some(token) = text.split_whitespace().next() else {
        return Vec::new();
    };
    let version = token.strip_prefix('v').unwrap_or(token);
    if version.is_empty()
        || !version
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
    {
        return Vec::new();
    }
    let start = text.find(token).expect("token from text") + (token.len() - version.len());
    vec![Dependency {
        manager,
        file: path.into(),
        location: Location::new(text, start, start + version.len(), "/node".into()),
        name: "node".into(),
        package: "node".into(),
        kind: DependencyKind::Toolchain,
        current: version.into(),
        resolved: Some(version.into()),
        digest: None,
        source: Source::Mise,
        scheme: Scheme::Loose,
        extract: None,
        included_by: None,
        registry: None,
    }]
}

/// What a registry setting names.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Named {
    /// npm's or Yarn's public registry, which serve the same packages.
    Public,
    Url(String),
    /// A value the Count cannot read from, such as `${VAR}` or plain http,
    /// kept as written. Its packages are never looked up on the public
    /// registry instead, where the same name may be someone else's.
    Unusable(String),
}

/// Whether `url` is on npm's or Yarn's public registry host, over https or the
/// plain http some old `.npmrc` files still name.
fn public(url: &str) -> bool {
    crate::web_host(url)
        .is_some_and(|host| matches!(host.as_str(), "registry.npmjs.org" | "registry.yarnpkg.com"))
}

fn named(value: &str) -> Named {
    let raw = value.trim().trim_matches(['"', '\'']);
    if public(raw) {
        return Named::Public;
    }
    match crate::registry_url(raw) {
        Some(url) => Named::Url(url),
        None => Named::Unusable(crate::redacted(raw)),
    }
}

/// The registry a dependency records for a setting: none for the public one.
fn recorded(named: &Named) -> Option<String> {
    match named {
        Named::Public => None,
        Named::Url(value) | Named::Unusable(value) => Some(value.clone()),
    }
}

/// An npm registry an extractor names, as a dependency records it.
pub(super) fn extracted_registry(value: &str) -> Option<String> {
    recorded(&named(value))
}

/// The registries a project's `.npmrc` or `.yarnrc.yml` names.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Npmrc {
    default: Option<Named>,
    scopes: BTreeMap<String, Named>,
}

impl Npmrc {
    fn names_any(&self) -> bool {
        self.default.is_some() || !self.scopes.is_empty()
    }
}

/// `registry=` and `@scope:registry=` from an `.npmrc`.
pub(super) fn npmrc(text: &str) -> Npmrc {
    let mut npmrc = Npmrc::default();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with(['#', ';']) {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        // An empty value sets nothing.
        if value.trim().trim_matches(['"', '\'']).is_empty() {
            continue;
        }
        if key == "registry" {
            npmrc.default = Some(named(value));
        } else if let Some(scope) = key.strip_suffix(":registry")
            && scope.starts_with('@')
            && scope.len() > 1
        {
            npmrc.scopes.insert(scope.to_owned(), named(value));
        }
    }
    npmrc
}

/// Yarn Berry's `npmRegistryServer` in `.yarnrc.yml`, at the top level and
/// under `npmScopes`.
pub(super) fn yarnrc(text: &str) -> Npmrc {
    let mut npmrc = Npmrc::default();
    let Ok(value) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(text) else {
        return npmrc;
    };
    let server = |value: &serde_yaml_ng::Value| {
        value
            .get("npmRegistryServer")
            .and_then(serde_yaml_ng::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(named)
    };
    npmrc.default = server(&value);
    if let Some(scopes) = value
        .get("npmScopes")
        .and_then(serde_yaml_ng::Value::as_mapping)
    {
        for (scope, settings) in scopes {
            if let (Some(scope), Some(named)) = (scope.as_str(), server(settings))
                && !scope.is_empty()
            {
                npmrc.scopes.insert(format!("@{scope}"), named);
            }
        }
    }
    npmrc
}

/// The registries each directory's `.npmrc` and `.yarnrc.yml` name, keyed by
/// directory ("" at the root). A file that names none is left out, so it
/// does not hide one further up. Where both files name the same, Yarn's wins.
pub(super) fn npmrcs(files: &crate::Files) -> BTreeMap<&str, Npmrc> {
    let mut found: BTreeMap<&str, Npmrc> = BTreeMap::new();
    for (leaf, parse) in [
        (".npmrc", npmrc as fn(&str) -> Npmrc),
        (".yarnrc.yml", yarnrc),
    ] {
        for (path, text) in files {
            let directory = match path.rsplit_once('/') {
                Some((directory, name)) if name == leaf => directory,
                None if path == leaf => "",
                _ => continue,
            };
            let read = parse(text);
            if !read.names_any() {
                continue;
            }
            let entry = found.entry(directory).or_default();
            if read.default.is_some() {
                entry.default = read.default;
            }
            entry.scopes.extend(read.scopes);
        }
    }
    found
}

/// The registry for `package` in `file`: the nearest setting for its scope at
/// or above the file, else the nearest `registry=` there.
pub(super) fn registry_for(
    file: &str,
    package: &str,
    npmrcs: &BTreeMap<&str, Npmrc>,
) -> Option<String> {
    let mut directories = Vec::new();
    let mut directory = file.rsplit_once('/').map_or("", |(d, _)| d);
    loop {
        directories.push(directory);
        if directory.is_empty() {
            break;
        }
        directory = directory.rsplit_once('/').map_or("", |(d, _)| d);
    }
    let settings: Vec<&Npmrc> = directories.iter().filter_map(|d| npmrcs.get(d)).collect();
    let scope = package
        .split_once('/')
        .map(|(scope, _)| scope)
        .filter(|scope| scope.starts_with('@'));
    let named = scope
        .and_then(|scope| settings.iter().find_map(|s| s.scopes.get(scope)))
        .or_else(|| settings.iter().find_map(|s| s.default.as_ref()))?;
    recorded(named)
}

/// Look each npm dependency up in the registry its `.npmrc` or `.yarnrc.yml`
/// names. `packageManager` stays on the public registry, since Corepack does
/// not read them.
pub(super) fn assign_registries(deps: &mut [Dependency], npmrcs: &BTreeMap<&str, Npmrc>) {
    for dep in deps
        .iter_mut()
        .filter(|dep| dep.source == Source::Npm && dep.location.pointer != "/packageManager")
    {
        dep.registry = registry_for(&dep.file, &dep.package, npmrcs);
    }
}

/// The same for packages only a lockfile records, so their advisories are
/// not read for the public package of their name.
pub(super) fn assign_locked_registries(
    locked: &mut [crate::locked::Locked],
    npmrcs: &BTreeMap<&str, Npmrc>,
) {
    for entry in locked
        .iter_mut()
        .filter(|entry| matches!(entry.manager, Manager::Npm | Manager::Bun))
    {
        entry.registry = registry_for(&entry.file, &entry.package, npmrcs);
    }
}
