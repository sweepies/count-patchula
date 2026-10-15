use crate::*;
use jsonc_parser::{
    ast::{Object, Value},
    common::Ranged,
};

fn ast(text: &str) -> Result<Value<'_>, String> {
    jsonc_parser::parse_to_ast(text, &Default::default(), &Default::default())
        .map_err(|e| e.to_string())?
        .value
        .ok_or_else(|| "empty JSON".into())
}
pub(super) fn locks(files: &Files) -> Result<BTreeMap<&str, Value<'_>>, String> {
    files
        .iter()
        .filter(|(path, _)| path.ends_with("package-lock.json") || path.ends_with("bun.lock"))
        .map(|(path, text)| Ok((path.as_str(), ast(text)?)))
        .collect()
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
    let root = ast(text)?;
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
        });
    }
    for dep in &mut deps {
        if dep.resolved.is_none() {
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
        let ptr = format!("{pointer}/{}", name.replace('~', "~0").replace('/', "~1"));
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
                    format!("override:{pointer}:{name}")
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
