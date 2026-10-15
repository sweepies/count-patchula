use crate::*;
use regex::Regex;

pub(super) fn dependency(
    manager: Manager,
    path: &str,
    _text: &str,
    image: &str,
    location: Location,
) -> Option<Dependency> {
    let image = image.trim_matches(['\'', '"']);
    if image.contains(['$', ' ', '{']) || image == "scratch" {
        return None;
    }
    let (image, digest) = image
        .split_once('@')
        .map_or((image, None), |(v, d)| (v, Some(d.to_owned())));
    let (package, tag) = image
        .rsplit_once(':')
        .filter(|(_, v)| !v.contains('/'))
        .unwrap_or((image, "latest"));
    if package.is_empty() || tag.is_empty() {
        return None;
    }
    Some(Dependency {
        manager,
        file: path.into(),
        location,
        name: package.into(),
        package: package.into(),
        kind: DependencyKind::ContainerImage,
        current: tag.into(),
        resolved: (tag != "latest").then(|| tag.into()),
        digest,
        source: Source::Docker,
        scheme: Scheme::Docker,
        extract: None,
    })
}

pub(super) fn dockerfile(path: &str, text: &str) -> Result<Vec<Dependency>, String> {
    let arg = Regex::new(
        r"\$(?:\{(?P<braced>[A-Za-z_][A-Za-z0-9_]*)\}|(?P<plain>[A-Za-z_][A-Za-z0-9_]*))",
    )
    .map_err(|e| e.to_string())?;
    let mut args = BTreeMap::<String, String>::new();
    let mut stages = std::collections::BTreeSet::new();
    let mut seen_from = false;
    let mut out = Vec::new();
    let mut offset = 0;
    // Join continuations while retaining the original byte range.
    let mut instruction = String::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        if instruction.is_empty() {
            start = offset;
        }
        offset += line.len();
        let code = line.trim();
        if code.starts_with('#') {
            continue;
        }
        instruction.push_str(code.trim_end_matches('\\'));
        if code.ends_with('\\') {
            instruction.push(' ');
            continue;
        }
        let parts: Vec<_> = instruction.split_whitespace().collect();
        if parts.first().is_some_and(|v| v.eq_ignore_ascii_case("ARG")) {
            if !seen_from && let Some((name, value)) = parts.get(1).and_then(|v| v.split_once('='))
            {
                args.insert(name.into(), value.trim_matches(['\'', '"']).into());
            }
        } else if parts
            .first()
            .is_some_and(|v| v.eq_ignore_ascii_case("FROM"))
            && let Some(image) = parts.iter().skip(1).find(|v| !v.starts_with("--"))
        {
            seen_from = true;
            let mut expanded = (*image).to_owned();
            for _ in 0..8 {
                let next = arg
                    .replace_all(&expanded, |caps: &regex::Captures<'_>| {
                        let name = caps
                            .name("braced")
                            .or_else(|| caps.name("plain"))
                            .unwrap()
                            .as_str();
                        args.get(name).cloned().unwrap_or_else(|| caps[0].into())
                    })
                    .into_owned();
                if next == expanded {
                    break;
                }
                expanded = next;
            }
            if !stages.contains(&expanded.to_ascii_lowercase()) {
                // Full instruction range remains meaningful when ARG or a
                // line continuation prevents a contiguous image scalar.
                let location = text[start..offset].find(*image).map_or_else(
                    || Location::new(text, start, offset, format!("/FROM/{}", out.len())),
                    |n| {
                        Location::new(
                            text,
                            start + n,
                            start + n + image.len(),
                            format!("/FROM/{}", out.len()),
                        )
                    },
                );
                if let Some(dep) = dependency(Manager::Dockerfile, path, text, &expanded, location)
                {
                    out.push(dep);
                }
            }
            if let Some(i) = parts.iter().position(|v| v.eq_ignore_ascii_case("AS"))
                && let Some(name) = parts.get(i + 1)
            {
                stages.insert(name.to_ascii_lowercase());
            }
        }
        instruction.clear();
    }
    Ok(out)
}
