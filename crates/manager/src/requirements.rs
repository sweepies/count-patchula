use crate::*;
use regex::Regex;
pub(super) fn extract(
    path: &str,
    text: &str,
    graph: &crate::pypi::RequirementsGraph,
) -> Result<Vec<Dependency>, String> {
    let pattern = Regex::new(r"(?m)^\s*(?P<name>[A-Za-z0-9][A-Za-z0-9._-]*)(?:\[[^\]\n]+\])?\s*(?P<version>(?:===|==|~=|!=|<=|>=|<|>)[^;#\\\n]+)").map_err(|e|e.to_string())?;
    let mut out = Vec::new();
    for c in pattern.captures_iter(text) {
        let version = c.name("version").unwrap();
        let current = version.as_str().trim();
        // Hashes are verification metadata, not part of a version constraint.
        let current = current.split(" --hash").next().unwrap().trim();
        let name = &c["name"];
        out.push(Dependency {
            manager: Manager::Requirements,
            file: path.into(),
            location: Location::new(
                text,
                version.start(),
                version.start() + current.len(),
                format!("/{name}"),
            ),
            name: name.into(),
            package: name.to_ascii_lowercase().replace(['_', '.'], "-"),
            kind: DependencyKind::Production,
            current: current.into(),
            resolved: current
                .strip_prefix("==")
                .filter(|v| {
                    !v.starts_with('=')
                        && !v.contains(['*', ','])
                        && patchula_versioning::Versioning::parse(
                            &patchula_versioning::Ecosystem::Pep440,
                            v,
                        )
                        .is_ok()
                })
                .map(str::to_owned),
            digest: None,
            source: Source::Pypi,
            scheme: Scheme::Pep440,
            extract: None,
            included_by: None,
            registry: None,
        });
    }
    // Without a lockfile, a requirements file pins nothing to an index.
    // Requirements files name no non-index sources, so nothing is dropped.
    let _ = crate::pypi::requirements(path, graph).assign(&mut out, |_| None);
    Ok(out)
}
