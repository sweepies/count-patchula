//! Helm `Chart.yaml` dependencies from HTTP and OCI chart repositories.
//! `file://` charts and repository aliases (`@name`, `alias:`) are local.
use crate::*;
use patchula_config::join_package;

pub(super) fn extract(path: &str, text: &Lines<'_>) -> Result<Vec<Dependency>, String> {
    let locations = crate::yaml_locations::parse(text)?;
    let Some(document) = crate::yaml::docs(text)?.into_iter().next() else {
        return Ok(Vec::new());
    };
    let Some(dependencies) = document.get("dependencies").and_then(|d| d.as_sequence()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (index, chart) in dependencies.iter().enumerate() {
        let field = |key: &str| chart.get(key).and_then(|v| v.as_str()).map(str::trim);
        let (Some(name), Some(version), Some(repository)) =
            (field("name"), field("version"), field("repository"))
        else {
            continue;
        };
        let (package, source) = if let Some(oci) = repository.strip_prefix("oci://") {
            (format!("{}/{name}", oci.trim_end_matches('/')), Source::Oci)
        } else if repository.starts_with("https://") || repository.starts_with("http://") {
            (
                join_package(repository.trim_end_matches('/'), name),
                Source::Helm,
            )
        } else {
            continue;
        };
        let pointer = format!("/0/dependencies/{index}/version");
        let location = locations
            .get(&pointer)
            .cloned()
            .ok_or_else(|| format!("cannot locate chart version {pointer}"))?;
        out.push(Dependency {
            manager: Manager::Helm,
            file: path.into(),
            location,
            form: EditForm::Scalar(Syntax::Yaml),
            name: name.into(),
            package,
            kind: DependencyKind::Chart,
            current: version.into(),
            resolved: None,
            digest: None,
            source,
            scheme: Scheme::Semver,
            extract: None,
            included_by: None,
            registry: None,
        });
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
        path.rsplit('/').next() == Some("Chart.yaml")
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
