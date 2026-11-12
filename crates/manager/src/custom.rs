use crate::*;
use patchula_config::Extractor;
use regex::{Captures, Regex};
fn template(text: &str, captures: &Captures<'_>) -> Result<String, String> {
    let mut out = String::new();
    let mut remaining = text;
    while let Some(start) = remaining.find('{') {
        out.push_str(&remaining[..start]);
        let end = remaining[start + 1..]
            .find('}')
            .ok_or("unclosed template")?
            + start
            + 1;
        let name = &remaining[start + 1..end];
        out.push_str(
            captures
                .name(name)
                .ok_or_else(|| format!("unmatched template capture {name}"))?
                .as_str(),
        );
        remaining = &remaining[end + 1..];
    }
    out.push_str(remaining);
    Ok(out)
}
pub(super) fn extract(
    path: &str,
    text: &Lines<'_>,
    extractors: &[Extractor],
    npmrcs: &BTreeMap<&str, npm::Npmrc>,
) -> Result<Vec<Dependency>, String> {
    let mut out = Vec::new();
    for extractor in extractors {
        if !Patterns::compile(&extractor.paths)?.matches(path) {
            continue;
        }
        for pattern in &extractor.patterns {
            let regex = Regex::new(pattern).map_err(|e| e.to_string())?;
            for captures in regex.captures_iter(text) {
                let current = captures.name("current").ok_or("missing current capture")?;
                let source = serde_json::from_value(serde_json::Value::String(template(
                    &extractor.source,
                    &captures,
                )?))
                .map_err(|e| e.to_string())?;
                let scheme = serde_json::from_value(serde_json::Value::String(template(
                    &extractor.scheme,
                    &captures,
                )?))
                .map_err(|e| e.to_string())?;
                let package = template(&extractor.package, &captures)?;
                out.push(Dependency {
                    manager: Manager::Regex,
                    file: path.into(),
                    location: Location::new(text, current.start(), current.end(), String::new()),
                    form: EditForm::Scalar(Syntax::Text),
                    name: template(&extractor.dependency, &captures)?,
                    package: package.clone(),
                    kind: DependencyKind::Production,
                    current: current.as_str().into(),
                    resolved: None,
                    digest: None,
                    source,
                    scheme,
                    extract: extractor.extract.clone(),
                    included_by: None,
                    // An optional capture that did not take part, or is empty,
                    // means the source's public registry — an explicit public
                    // answer the `.npmrc` fallback must not override. A value
                    // the Count cannot read from is kept as written, so that
                    // one lookup fails instead of reading the public package
                    // of the name. Without a template, npm packages follow
                    // `.npmrc`.
                    registry: match &extractor.registry {
                        Some(registry) => match template(registry, &captures) {
                            Ok(raw) if raw.is_empty() => None,
                            Ok(raw) if source == Source::Npm => npm::extracted_registry(&raw),
                            Ok(raw) if source == Source::Pypi => pypi::named(&raw).registry(),
                            Ok(raw) => Some(match registry_url(&raw) {
                                Some(url) => Registry::Url(url),
                                None => Registry::Unusable(crate::redacted(&raw)),
                            }),
                            Err(error) if error.starts_with("unmatched template capture") => None,
                            Err(error) => return Err(error),
                        },
                        None if source == Source::Npm => {
                            npm::named_for(path, &package, npmrcs).registry()
                        }
                        None => None,
                    },
                });
            }
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
    fn selected(&self, config: &Config, path: &str) -> bool {
        config
            .extractors
            .iter()
            .any(|e| Patterns::compile(&e.paths).is_ok_and(|p| p.matches(path)))
    }
    fn prepare<'a>(
        &self,
        scanner: &'a Scanner,
        _files: &'a Files,
        shared: &'a Shared<'a>,
    ) -> Result<Prepared<'a>, String> {
        let extractors: &[Extractor] = &scanner.config.extractors;
        let npmrcs = &shared.npmrcs;
        Ok(Prepared {
            extract: Some(Box::new(move |path, text, out| {
                // A bad extractor fails the whole extraction, as before.
                let dependencies = extract(path, text, extractors, npmrcs)?;
                out.dependencies.extend(dependencies);
                Ok(())
            })),
            finish: None,
        })
    }
}
