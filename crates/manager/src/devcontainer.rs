//! `devcontainer.json`: the container `image` and OCI `features`, whose
//! versions live in the feature key (`ghcr.io/owner/features/node:1`).
use crate::*;
use jsonc_parser::common::Ranged;

pub(super) fn extract(path: &str, text: &str) -> Result<Vec<Dependency>, String> {
    let root = crate::jsonc_ast_lenient(text).map_err(|e| {
        if e == "empty JSON" {
            "empty devcontainer.json".to_owned()
        } else {
            e
        }
    })?;
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
                Syntax::InnerToken,
                None,
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
                    format!("/features/{}/tag", pointer_token(key)),
                ),
                form: EditForm::Scalar(Syntax::InnerToken),
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
                registry: None,
            });
        }
    }
    Ok(out)
}

pub(crate) static DESCRIPTOR: Descriptor = Descriptor;

pub(crate) struct Descriptor;

impl crate::Ecosystem for Descriptor {
    fn proposing(&self, dep: &Dependency) -> Proposing {
        Proposing {
            range: Range::Installed,
            runtime_toolchain: false,
            written: Written::Range,
            keeps_satisfied: false,
            unversioned: Unversioned {
                looked_up: false,
                opaque: false,
            },
            short_tags: ShortTags::Fixed,
            selector_resolved: false,
            resolved_via_listing: dep.kind == DependencyKind::Tool,
            pin: Pin {
                commit: false,
                needs_digest: matches!(dep.source, Source::Docker | Source::Oci)
                    && matches!(
                        dep.kind,
                        DependencyKind::Action
                            | DependencyKind::ReusableWorkflow
                            | DependencyKind::ContainerImage
                            | DependencyKind::Chart
                    ),
                from_catalog: false,
            },
            lock: Lock {
                keeps_ranges: false,
                movable: false,
                transitive_movable: dep.kind == DependencyKind::Transitive
                    && dep.source == Source::Crates,
                reported: false,
            },
            edits: Edits::Manifest,
            advisories: Advisories::BySource,
        }
    }
    fn selected(&self, _config: &Config, path: &str) -> bool {
        matches!(
            path.rsplit('/').next().unwrap_or(path),
            "devcontainer.json" | ".devcontainer.json"
        )
    }
    fn prepare<'a>(
        &self,
        _scanner: &'a Scanner,
        _files: &'a Files,
        _shared: &'a Shared<'a>,
    ) -> Result<Prepared<'a>, String> {
        Ok(Prepared {
            extract: Some(Box::new(|path, text, out| {
                file_result(out, path, extract(path, text));
                Ok(())
            })),
            finish: None,
        })
    }
}
