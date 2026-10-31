//! Helm `Chart.yaml` dependencies from HTTP and OCI chart repositories.
//! `file://` charts and repository aliases (`@name`, `alias:`) are local.
use crate::*;

pub(super) fn extract(path: &str, text: &str) -> Result<Vec<Dependency>, String> {
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
                format!("{}|{name}", repository.trim_end_matches('/')),
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
        });
    }
    Ok(out)
}
