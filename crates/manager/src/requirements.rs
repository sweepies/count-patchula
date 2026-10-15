use crate::*;
use regex::Regex;
pub(super) fn extract(path: &str, text: &str) -> Result<Vec<Dependency>, String> {
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
            resolved: None,
            digest: None,
            source: Source::Pypi,
            scheme: Scheme::Pep440,
            extract: None,
        });
    }
    Ok(out)
}
