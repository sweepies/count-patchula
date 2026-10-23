//! Single-version toolchain files: `rust-toolchain(.toml)` for Cargo and
//! `.terraform-version` for Terraform. Only numeric versions are pins;
//! channels such as `stable`, `nightly-…` or `latest` are not.
use crate::*;

fn numeric(version: &str) -> bool {
    !version.is_empty()
        && version
            .split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

fn pin(
    path: &str,
    text: &str,
    manager: Manager,
    tool: &str,
    start: usize,
    version: &str,
) -> Dependency {
    Dependency {
        manager,
        file: path.into(),
        location: Location::new(text, start, start + version.len(), format!("/{tool}")),
        name: tool.into(),
        package: tool.into(),
        kind: DependencyKind::Toolchain,
        current: version.into(),
        resolved: Some(version.into()),
        digest: None,
        source: Source::Mise,
        scheme: Scheme::Loose,
        extract: None,
    }
}

/// The first whitespace-separated token of a plain version file.
fn first_token(text: &str) -> Option<(usize, &str)> {
    let token = text.split_whitespace().next()?;
    Some((text.find(token)?, token))
}

pub(super) fn rust(path: &str, text: &str) -> Result<Vec<Dependency>, String> {
    if path.ends_with(".toml") || text.trim_start().starts_with('[') {
        let doc = toml_edit::Document::parse(text).map_err(|e| e.to_string())?;
        let Some(channel) = doc.get("toolchain").and_then(|t| t.get("channel")) else {
            return Ok(Vec::new());
        };
        let (Some(version), Some(span)) = (channel.as_str(), channel.span()) else {
            return Ok(Vec::new());
        };
        let raw = &text[span.clone()];
        if !numeric(version) || raw.len() != version.len() + 2 {
            return Ok(Vec::new());
        }
        return Ok(vec![pin(
            path,
            text,
            Manager::Cargo,
            "rust",
            span.start + 1,
            version,
        )]);
    }
    Ok(first_token(text)
        .filter(|(_, v)| numeric(v))
        .map(|(start, v)| pin(path, text, Manager::Cargo, "rust", start, v))
        .into_iter()
        .collect())
}

pub(super) fn terraform(path: &str, text: &str) -> Vec<Dependency> {
    first_token(text)
        .and_then(|(start, v)| {
            let version = v.strip_prefix('v').unwrap_or(v);
            numeric(version).then(|| {
                pin(
                    path,
                    text,
                    Manager::Terraform,
                    "terraform",
                    start + (v.len() - version.len()),
                    version,
                )
            })
        })
        .into_iter()
        .collect()
}
