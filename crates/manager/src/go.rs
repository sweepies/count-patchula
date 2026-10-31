use crate::*;
use regex::Regex;

pub(super) fn extract(path: &str, text: &str) -> Result<Vec<Dependency>, String> {
    let require = Regex::new(
        r#"^\s*(?:require\s+)?(?P<name>[^\s"`]+|"[^"]+"|`[^`]+`)\s+(?P<version>v\d[^\s]*)"#,
    )
    .map_err(|e| e.to_string())?;
    let directive = Regex::new(r"^\s*(?P<kind>go|toolchain)\s+(?P<version>(?:go)?\d[^\s]*)")
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut block = false;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let code = line.split("//").next().unwrap_or("").trim();
        if code.starts_with("require") && code.ends_with('(') {
            block = true;
        } else if code == ")" {
            block = false;
        } else if (block || code.starts_with("require "))
            && let Some(c) = require.captures(code)
        {
            let version = c.name("version").unwrap();
            let n = line.find(code).unwrap() + version.start();
            let name = c["name"].trim_matches(['"', '`']);
            out.push(Dependency {
                manager: Manager::Go,
                file: path.into(),
                location: Location::new(
                    text,
                    offset + n,
                    offset + n + version.len(),
                    format!("/require/{name}"),
                ),
                name: name.into(),
                package: name.into(),
                kind: DependencyKind::Production,
                current: version.as_str().into(),
                resolved: Some(version.as_str().into()),
                digest: version
                    .as_str()
                    .rsplit_once('-')
                    .and_then(|(prefix, hash)| {
                        (prefix.rsplit('-').next().is_some_and(|time| {
                            time.len() == 14 && time.bytes().all(|b| b.is_ascii_digit())
                        }) && hash.len() == 12
                            && hash.bytes().all(|b| b.is_ascii_hexdigit()))
                        .then(|| hash.to_owned())
                    }),
                source: Source::Go,
                scheme: Scheme::Go,
                extract: None,
                included_by: None,
            });
        } else if let Some(c) = directive.captures(code) {
            let version = c.name("version").unwrap();
            let n = line.find(code).unwrap() + version.start();
            out.push(Dependency {
                manager: Manager::Go,
                file: path.into(),
                location: Location::new(
                    text,
                    offset + n,
                    offset + n + version.len(),
                    format!("/{}", &c["kind"]),
                ),
                name: c["kind"].into(),
                package: "go".into(),
                kind: if &c["kind"] == "go" {
                    DependencyKind::GoDirective
                } else {
                    DependencyKind::Toolchain
                },
                current: version.as_str().trim_start_matches("go").into(),
                resolved: Some(version.as_str().trim_start_matches("go").into()),
                digest: None,
                source: Source::Mise,
                scheme: Scheme::Loose,
                extract: None,
                included_by: None,
            });
        }
        offset += line.len();
    }
    Ok(out)
}
