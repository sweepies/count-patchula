//! `composer.json` `require` and `require-dev`, resolved from `composer.lock`.
//! Platform packages (`php`, `ext-*`, `lib-*`, Composer's APIs) are not
//! updated.
use crate::*;
use jsonc_parser::common::Ranged;

fn platform(name: &str) -> bool {
    !name.contains('/')
        || name.starts_with("ext-")
        || name.starts_with("lib-")
        || matches!(name, "composer-plugin-api" | "composer-runtime-api")
}

fn locked(lock: Option<&String>) -> BTreeMap<String, String> {
    let Some(value) = lock.and_then(|l| serde_json::from_str::<serde_json::Value>(l).ok()) else {
        return BTreeMap::new();
    };
    ["packages", "packages-dev"]
        .iter()
        .filter_map(|k| value[k].as_array())
        .flatten()
        .filter_map(|p| {
            let version = p["version"].as_str()?;
            Some((
                p["name"].as_str()?.to_ascii_lowercase(),
                version.to_string(),
            ))
        })
        .collect()
}

pub(super) fn extract(path: &str, text: &str, files: &Files) -> Result<Vec<Dependency>, String> {
    if !path.ends_with("composer.json") {
        return Ok(Vec::new());
    }
    let root = jsonc_parser::parse_to_ast(text, &Default::default(), &Default::default())
        .map_err(|e| e.to_string())?
        .value
        .ok_or("empty composer.json")?;
    let object = root.as_object().ok_or("composer.json must be an object")?;
    let lock = locked(files.get(&format!(
        "{}composer.lock",
        path.strip_suffix("composer.json").unwrap_or("")
    )));
    let mut out = Vec::new();
    for (field, kind) in [
        ("require", DependencyKind::Production),
        ("require-dev", DependencyKind::Development),
    ] {
        let Some(requirements) = object.get_object(field) else {
            continue;
        };
        for prop in &requirements.properties {
            let name = prop.name.as_str();
            let Some(value) = prop.value.as_string_lit() else {
                continue;
            };
            let (start, end) = (value.start(), value.end());
            // Escapes, local `dev-` branches and `*` are not version pins.
            if platform(name)
                || end - start != value.value.len() + 2
                || value.value.starts_with("dev-")
                || value.value.trim() == "*"
            {
                continue;
            }
            out.push(Dependency {
                manager: Manager::Composer,
                file: path.into(),
                location: Location::new(
                    text,
                    start + 1,
                    end - 1,
                    format!("/{field}/{}", name.replace('/', "~1")),
                ),
                name: name.into(),
                package: name.to_ascii_lowercase(),
                kind,
                current: value.value.to_string(),
                resolved: lock.get(&name.to_ascii_lowercase()).cloned(),
                digest: None,
                source: Source::Packagist,
                scheme: Scheme::Composer,
                extract: None,
                included_by: None,
            });
        }
    }
    Ok(out)
}
