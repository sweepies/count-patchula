//! `devcontainer.json`: the container `image` and OCI `features`, whose
//! versions live in the feature key (`ghcr.io/owner/features/node:1`).
use crate::*;
use jsonc_parser::common::Ranged;

pub(super) fn extract(path: &str, text: &str) -> Result<Vec<Dependency>, String> {
    let options = jsonc_parser::ParseOptions {
        allow_comments: true,
        allow_trailing_commas: true,
        ..Default::default()
    };
    let root = jsonc_parser::parse_to_ast(text, &Default::default(), &options)
        .map_err(|e| e.to_string())?
        .value
        .ok_or("empty devcontainer.json")?;
    let object = root
        .as_object()
        .ok_or("devcontainer.json must be an object")?;
    let mut out = Vec::new();
    if let Some(image) = object.get_string("image") {
        let (start, end) = (image.start() + 1, image.end() - 1);
        // Escapes would shift offsets; such images are left alone.
        if !text[start..end].contains('\\')
            && let Some(dep) = crate::images::dependency(
                Manager::Devcontainer,
                path,
                text,
                &image.value,
                Location::new(text, start, end, "/image".into()),
            )
        {
            out.push(dep);
        }
    }
    if let Some(features) = object.get_object("features") {
        for prop in &features.properties {
            let key = prop.name.as_str();
            let range = prop.name.range();
            let raw = &text[range.start..range.end];
            // Only quoted keys without escapes have a locatable tag.
            if !raw.starts_with('"') || raw.contains('\\') || raw.len() != key.len() + 2 {
                continue;
            }
            let Some((reference, tag)) = key
                .rsplit_once(':')
                .filter(|(r, t)| r.contains('/') && !t.contains('/') && !t.is_empty())
            else {
                continue;
            };
            let start = range.start + 1 + reference.len() + 1;
            out.push(Dependency {
                manager: Manager::Devcontainer,
                file: path.into(),
                location: Location::new(
                    text,
                    start,
                    start + tag.len(),
                    format!(
                        "/features/{}/tag",
                        key.replace('~', "~0").replace('/', "~1")
                    ),
                ),
                name: reference.into(),
                package: reference.into(),
                // A feature key holds a tag only, so features are never
                // digest-pinned like container images.
                kind: DependencyKind::Tool,
                current: tag.into(),
                resolved: (tag != "latest").then(|| tag.into()),
                digest: None,
                source: Source::Docker,
                scheme: Scheme::Docker,
                extract: None,
                included_by: None,
            });
        }
    }
    Ok(out)
}
