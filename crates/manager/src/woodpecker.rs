//! Woodpecker workflows: the images of steps, services and the clone step, in
//! the current `steps:` and the legacy `pipeline:` shape, each a map or a
//! list. An image defined once under an anchor is edited at the anchor, so it
//! is one dependency however many steps alias it.
use crate::*;
use serde_yaml_ng::Value;

const SECTIONS: &[&str] = &["clone", "steps", "pipeline", "services"];

pub(super) fn extract(
    path: &str,
    text: &str,
    skipped: &mut Vec<Skipped>,
) -> Result<Vec<Dependency>, String> {
    let documents = yaml::docs(text)?;
    let (locations, aliases) = yaml_locations::parse_aliased(text)?;
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (index, doc) in documents.iter().enumerate() {
        let matrix = matrix(doc);
        for section in SECTIONS {
            let pointer = format!("/{index}/{section}");
            let containers: Vec<(String, &Value)> = match doc.get(section) {
                Some(Value::Mapping(map)) => map
                    .iter()
                    .filter_map(|(k, v)| {
                        let key = k.as_str()?.replace('~', "~0").replace('/', "~1");
                        Some((format!("{pointer}/{key}"), v))
                    })
                    .collect(),
                Some(Value::Sequence(items)) => items
                    .iter()
                    .enumerate()
                    .map(|(i, v)| (format!("{pointer}/{i}"), v))
                    .collect(),
                _ => continue,
            };
            for (pointer, container) in containers {
                let Some((image, pointer)) = container_image(container, &pointer) else {
                    continue;
                };
                let location = locations.get(&aliases.resolve(&pointer));
                let skip = |reason: String| Skipped {
                    manager: Manager::Woodpecker,
                    file: path.into(),
                    line: location.map(|l| l.line),
                    value: image.into(),
                    reason,
                };
                if let Some(reason) = templated(image, &matrix) {
                    skipped.push(skip(reason));
                    continue;
                }
                let Some(location) = location else {
                    skipped.push(skip(
                        "the image is not a plain scalar the Count can edit in place".into(),
                    ));
                    continue;
                };
                if !seen.insert(location.start) {
                    continue;
                }
                match images::dependency(Manager::Woodpecker, path, text, image, location.clone()) {
                    Some(dep) => out.push(dep),
                    None => skipped.push(skip("not an image reference".into())),
                }
            }
        }
    }
    Ok(out)
}

/// A step's own `image`, or one merged in with `<<: *anchor`.
fn container_image<'a>(container: &'a Value, pointer: &str) -> Option<(&'a str, String)> {
    if let Some(image) = container.get("image") {
        return Some((image.as_str()?, format!("{pointer}/image")));
    }
    match container.get("<<")? {
        Value::Sequence(items) => items.iter().enumerate().find_map(|(i, v)| {
            Some((v.get("image")?.as_str()?, format!("{pointer}/<</{i}/image")))
        }),
        merged => Some((
            merged.get("image")?.as_str()?,
            format!("{pointer}/<</image"),
        )),
    }
}

/// Variables a workflow's `matrix` defines, as a map or as `include` entries.
fn matrix(doc: &Value) -> Vec<String> {
    let Some(matrix) = doc.get("matrix") else {
        return Vec::new();
    };
    let includes = matrix.get("include").and_then(Value::as_sequence);
    std::iter::once(matrix)
        .chain(includes.into_iter().flatten())
        .filter_map(Value::as_mapping)
        .flat_map(|map| map.keys().filter_map(Value::as_str).map(str::to_owned))
        .collect()
}

/// Why an image can't be read as written: Woodpecker fills in `${VAR}` and
/// `$VAR` from the environment or the matrix only when the workflow runs.
fn templated(image: &str, matrix: &[String]) -> Option<String> {
    if image.contains("{{") {
        return Some("the image is a template".into());
    }
    if !image.contains('$') {
        return None;
    }
    let from_matrix = image.split('$').skip(1).find_map(|rest| {
        let name: String = rest
            .trim_start_matches('{')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        matrix.contains(&name).then_some(name)
    });
    Some(match from_matrix {
        Some(name) => format!("the image varies with the matrix variable {name}"),
        None => "the image contains a variable".into(),
    })
}
