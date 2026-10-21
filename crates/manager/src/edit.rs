//! Surgical edits against the exact extracted UTF-8 contents.
use crate::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub replacement: String,
}

/// Render only extracted scalars; surrounding documents are never serialized.
pub fn edits(
    dep: &Dependency,
    text: &str,
    value: &str,
    digest: Option<&str>,
) -> Result<Vec<Edit>, String> {
    let raw = text
        .get(dep.location.start..dep.location.end)
        .ok_or("invalid UTF-8 edit range")?;
    if dep.manager == Manager::Cargo && raw == "true" {
        // workspace=true is inventory; the root declaration owns its version.
        return Ok(Vec::new());
    }
    if dep.location.pointer == "/flux-version" {
        return Err("generated Flux bundles require regeneration".into());
    }
    let replacement = if dep.manager == Manager::GithubActions && dep.source == Source::GithubTags {
        if value.is_empty()
            || value.contains([' ', '\t', '\r', '\n', '\0', '\'', '"', '#', '@', '\\'])
            || digest.is_some_and(|d| d.len() != 40 || !d.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err("invalid action ref or commit digest".into());
        }
        if dep.digest.is_some() && value != dep.current && digest.is_none() {
            return Err("cannot change a pinned action without its target digest".into());
        }
        actions::replacement(dep, text, value, digest)?
    } else if dep.kind == DependencyKind::Runner {
        actions::runner_replacement(dep, text, value)?
    } else if dep.kind == DependencyKind::ContainerImage {
        return image_edits(dep, text, raw, value, digest);
    } else {
        let current = scalar(dep, raw)?;
        let target = if matches!(dep.manager, Manager::Npm | Manager::Bun)
            && dep.location.pointer == "/packageManager"
        {
            if current
                != format!(
                    "{}@{}{}",
                    dep.name,
                    dep.current,
                    dep.digest
                        .as_ref()
                        .map_or(String::new(), |d| format!("+{d}"))
                )
            {
                return Err("packageManager location mismatch".into());
            }
            if dep.digest.is_some() {
                return Err("packageManager integrity requires lockfile tooling".into());
            }
            format!("{}@{value}", dep.name)
        } else if matches!(dep.manager, Manager::Npm | Manager::Bun) && current.starts_with("npm:")
        {
            if current != format!("npm:{}@{}", dep.package, dep.current) {
                return Err("npm alias location mismatch".into());
            }
            format!("npm:{}@{value}", dep.package)
        } else if dep.manager == Manager::Mise && current.starts_with("tag:") {
            if current.strip_prefix("tag:") != Some(&dep.current) {
                return Err("mise tag location mismatch".into());
            }
            format!("tag:{value}")
        } else if dep.manager == Manager::Go && dep.location.pointer == "/toolchain" {
            if current != format!("go{}", dep.current) {
                return Err("Go toolchain location mismatch".into());
            }
            format!("go{}", value.trim_start_matches("go"))
        } else {
            if current != dep.current {
                return Err(format!(
                    "extracted scalar does not match {} at {}",
                    dep.name, dep.location.pointer
                ));
            }
            value.into()
        };
        quote(dep, raw, &target)?
    };
    Ok(vec![Edit {
        start: dep.location.start,
        end: dep.location.end,
        replacement,
    }])
}

/// npm and Bun dependencies live in JSON manifests, YAML catalogs and plain
/// Node version files, so their syntax follows the file rather than the manager.
fn json(dep: &Dependency) -> bool {
    matches!(dep.manager, Manager::Npm | Manager::Bun) && dep.file.ends_with("package.json")
}
fn yaml(dep: &Dependency) -> bool {
    matches!(
        dep.manager,
        Manager::GithubActions
            | Manager::Compose
            | Manager::Kubernetes
            | Manager::Kustomize
            | Manager::Flux
            | Manager::HelmValues
            | Manager::PreCommit
            | Manager::Helm
    ) || (matches!(dep.manager, Manager::Npm | Manager::Bun)
        && (dep.file.ends_with(".yaml") || dep.file.ends_with(".yml")))
}
fn toml(dep: &Dependency) -> bool {
    (matches!(dep.manager, Manager::Cargo | Manager::Mise)
        && !dep.file.ends_with(".tool-versions")
        // Toolchain pins are located inside their string, not as a TOML value.
        && dep.kind != DependencyKind::Toolchain)
        // Poetry constraints are whole TOML strings; PEP 508 specifiers are
        // spans inside one and are replaced as raw text.
        || (dep.manager == Manager::Pyproject && dep.location.pointer.starts_with("/tool/poetry/"))
}
fn scalar(dep: &Dependency, raw: &str) -> Result<String, String> {
    if json(dep) {
        serde_json::from_str(raw).map_err(|e| e.to_string())
    } else if yaml(dep) {
        let value: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(raw).map_err(|e| e.to_string())?;
        value
            .as_str()
            .map(str::to_owned)
            .or_else(|| value.as_u64().map(|v| v.to_string()))
            .or_else(|| value.as_f64().map(|v| v.to_string()))
            .ok_or_else(|| "unsupported YAML scalar".into())
    } else if toml(dep) {
        let doc = toml_edit::Document::parse(format!("v = {raw}")).map_err(|e| e.to_string())?;
        doc["v"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "unsupported TOML scalar".into())
    } else {
        Ok(raw.into())
    }
}
fn quote(dep: &Dependency, raw: &str, target: &str) -> Result<String, String> {
    if target.contains(['\n', '\r', '\0']) {
        return Err("multiline or NUL update is not supported".into());
    }
    if dep.manager == Manager::Pyproject && !toml(dep) && target.contains(['"', '\'', '\\']) {
        return Err("unsafe replacement inside a TOML string".into());
    }
    if json(dep) || raw.starts_with('"') {
        if raw.starts_with("\"\"\"") {
            if target.contains(['"', '\\']) {
                return Err("unsafe multiline TOML replacement".into());
            }
            return Ok(format!("\"\"\"{target}\"\"\""));
        }
        return serde_json::to_string(target).map_err(|e| e.to_string());
    }
    if raw.starts_with('\'') {
        let delimiter = if raw.starts_with("'''") { "'''" } else { "'" };
        let escaped = if yaml(dep) {
            target.replace('\'', "''")
        } else {
            if target.contains('\'') {
                return Err("unsafe literal string replacement".into());
            }
            target.into()
        };
        return Ok(format!("{delimiter}{escaped}{delimiter}"));
    }
    if yaml(dep) {
        let parsed =
            serde_yaml_ng::from_str::<serde_yaml_ng::Value>(target).map_err(|e| e.to_string())?;
        if parsed.as_str() != Some(target) && !parsed.is_number() {
            return serde_json::to_string(target).map_err(|e| e.to_string());
        }
    } else if target.chars().any(char::is_whitespace) && dep.manager != Manager::Regex {
        return Err("unsafe unquoted replacement".into());
    }
    Ok(target.into())
}

fn image_edits(
    dep: &Dependency,
    text: &str,
    raw: &str,
    value: &str,
    digest: Option<&str>,
) -> Result<Vec<Edit>, String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
        || digest.is_some_and(|d| {
            d.split_once(':').is_none_or(|(algorithm, hash)| {
                !matches!((algorithm, hash.len()), ("sha256", 64) | ("sha512", 128))
                    || !hash.bytes().all(|b| b.is_ascii_hexdigit())
            })
        })
    {
        return Err("invalid image tag or content digest".into());
    }
    let literal = if dep.manager == Manager::GithubActions {
        raw.split_once(" #").map_or(raw, |(a, _)| a).trim_end()
    } else {
        raw
    };
    let suffix = &raw[literal.len()..];
    let current = scalar(dep, literal)?;
    let split = dep.location.pointer.ends_with("/tag") || dep.location.pointer.ends_with("/newTag");
    if dep.digest.is_some() && value != dep.current && digest.is_none() {
        return Err("cannot change a pinned image without its target digest".into());
    }
    let mut out = Vec::new();
    let target = if split {
        if current != dep.current {
            return Err("image tag location mismatch".into());
        }
        if let Some(digest) = digest {
            let pointer = format!(
                "{}/digest",
                dep.location.pointer.rsplit_once('/').unwrap().0
            );
            let locations = crate::yaml_locations::parse(text)?;
            let location = locations
                .get(&pointer)
                .ok_or("adding a separate YAML digest field is not supported yet")?;
            let old = text
                .get(location.start..location.end)
                .ok_or("invalid digest range")?;
            out.push(Edit {
                start: location.start,
                end: location.end,
                replacement: quote(dep, old, digest)?,
            });
        }
        value.into()
    } else {
        let prefix = if current.starts_with("docker://") {
            "docker://"
        } else {
            ""
        };
        let image = current.strip_prefix(prefix).unwrap();
        let base = image.split('@').next().unwrap();
        let expected = format!("{}:{}", dep.package, dep.current);
        if base != expected && !(base == dep.package && dep.current == "latest") {
            return Err(
                "image location contains an expression or expanded ARG; no safe literal edit"
                    .into(),
            );
        }
        let digest = digest.or(dep.digest.as_deref());
        format!(
            "{prefix}{}:{value}{}",
            dep.package,
            digest.map_or(String::new(), |d| format!("@{d}"))
        )
    };
    out.push(Edit {
        start: dep.location.start,
        end: dep.location.end,
        replacement: format!("{}{suffix}", quote(dep, literal, &target)?),
    });
    Ok(out)
}

/// Reject ambiguous overlaps before producing any edited contents.
pub fn apply(text: &str, mut edits: Vec<Edit>) -> Result<String, String> {
    edits.sort_by(|a, b| (a.start, a.end, &a.replacement).cmp(&(b.start, b.end, &b.replacement)));
    edits.dedup();
    let mut end = 0;
    for edit in &edits {
        if edit.start < end || edit.start >= edit.end || text.get(edit.start..edit.end).is_none() {
            return Err("overlapping or invalid manifest edits".into());
        }
        end = edit.end;
    }
    let mut result = text.to_owned();
    for edit in edits.into_iter().rev() {
        result.replace_range(edit.start..edit.end, &edit.replacement);
    }
    Ok(result)
}
