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
        included_by: None,
        registry: None,
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
    // A byte-order mark is not part of the first instruction.
    let body = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut offset = text.len() - body.len();
    // The escape directive, among the leading comments, picks the character
    // that continues a line: a backslash unless it names a backtick.
    let escape = body
        .lines()
        .take_while(|line| line.trim_start().starts_with('#'))
        .find_map(|line| {
            let (key, value) = line.trim_start()[1..].split_once('=')?;
            key.trim()
                .eq_ignore_ascii_case("escape")
                .then(|| value.trim() == "`")
        })
        .map_or('\\', |backtick| if backtick { '`' } else { '\\' });
    let heredoc =
        Regex::new(r#"<<(-?)(["']?)([A-Za-z_][A-Za-z0-9_]*)["']?"#).map_err(|e| e.to_string())?;
    // Terminators of the heredocs still open, and whether each strips tabs.
    let mut bodies = std::collections::VecDeque::<(String, bool)>::new();
    // Join continuations while retaining the original byte range.
    let mut instruction = String::new();
    let mut start = 0;
    for line in body.split_inclusive('\n') {
        if instruction.is_empty() {
            start = offset;
        }
        offset += line.len();
        let raw = line.trim_end_matches(['\n', '\r']);
        // A heredoc's body is text for its instruction, not instructions.
        if let Some((end, tabs)) = bodies.front() {
            let candidate = if *tabs {
                raw.trim_start_matches('\t')
            } else {
                raw
            };
            if candidate == end {
                bodies.pop_front();
            }
            continue;
        }
        let code = raw.trim();
        // Docker drops comment lines, and empty lines inside a continuation,
        // before joining an instruction.
        if code.starts_with('#') || (code.is_empty() && !instruction.is_empty()) {
            continue;
        }
        // The escape character and the line end are removed and the next line
        // joined as it is, so a word split across lines stays one word.
        let trimmed = raw.trim_end();
        if let Some(head) = trimmed.strip_suffix(escape) {
            instruction.push_str(head);
            continue;
        }
        instruction.push_str(raw);
        let parts: Vec<_> = instruction.split_whitespace().collect();
        if parts.first().is_some_and(|v| {
            ["RUN", "COPY", "ADD"]
                .iter()
                .any(|k| v.eq_ignore_ascii_case(k))
        }) {
            bodies.extend(
                heredoc
                    .captures_iter(&instruction)
                    .map(|c| (c[3].to_owned(), &c[1] == "-")),
            );
        }
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
