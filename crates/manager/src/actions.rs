use crate::*;
use regex::Regex;
use serde_yaml_ng::Value;

pub(super) fn extract(path: &str, text: &str) -> Result<Vec<Dependency>, String> {
    let root: Value = serde_yaml_ng::from_str(text).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut used = std::collections::BTreeSet::new();
    if let Some(jobs) = root.get("jobs").and_then(Value::as_mapping) {
        for (job, value) in jobs {
            let job = job.as_str().unwrap_or("?");
            if let Some(reference) = value.get("uses").and_then(Value::as_str) {
                add(
                    path,
                    text,
                    reference,
                    "uses",
                    &format!("/jobs/{job}/uses"),
                    DependencyKind::ReusableWorkflow,
                    &mut used,
                    &mut out,
                )?;
            }
            if let Some(steps) = value.get("steps").and_then(Value::as_sequence) {
                steps_extract(
                    path,
                    text,
                    steps,
                    &format!("/jobs/{job}/steps"),
                    &mut used,
                    &mut out,
                )?;
            }
            if let Some(container) = value.get("container")
                && let Some(image) = container
                    .as_str()
                    .or_else(|| container.get("image").and_then(Value::as_str))
            {
                add(
                    path,
                    text,
                    image,
                    if container.is_string() {
                        "container"
                    } else {
                        "image"
                    },
                    &format!("/jobs/{job}/container"),
                    DependencyKind::ContainerImage,
                    &mut used,
                    &mut out,
                )?;
            }
            if let Some(services) = value.get("services").and_then(Value::as_mapping) {
                for (name, service) in services {
                    if let Some(image) = service.get("image").and_then(Value::as_str) {
                        add(
                            path,
                            text,
                            image,
                            "image",
                            &format!(
                                "/jobs/{job}/services/{}/image",
                                name.as_str().unwrap_or("?")
                            ),
                            DependencyKind::ContainerImage,
                            &mut used,
                            &mut out,
                        )?;
                    }
                }
            }
        }
    }
    if let Some(steps) = root
        .get("runs")
        .filter(|runs| runs.get("using").and_then(Value::as_str) == Some("composite"))
        .and_then(|runs| runs.get("steps"))
        .and_then(Value::as_sequence)
    {
        steps_extract(path, text, steps, "/runs/steps", &mut used, &mut out)?;
    }
    Ok(out)
}
fn steps_extract(
    path: &str,
    text: &str,
    steps: &[Value],
    pointer: &str,
    used: &mut std::collections::BTreeSet<usize>,
    out: &mut Vec<Dependency>,
) -> Result<(), String> {
    for (i, step) in steps.iter().enumerate() {
        if let Some(reference) = step.get("uses").and_then(Value::as_str) {
            add(
                path,
                text,
                reference,
                "uses",
                &format!("{pointer}/{i}/uses"),
                DependencyKind::Action,
                used,
                out,
            )?;
        }
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn add(
    path: &str,
    text: &str,
    reference: &str,
    key: &str,
    pointer: &str,
    kind: DependencyKind,
    used: &mut std::collections::BTreeSet<usize>,
    out: &mut Vec<Dependency>,
) -> Result<(), String> {
    if reference.starts_with("./") || reference.contains("${{") {
        return Ok(());
    }
    // YAML parsing selects only semantic uses/container nodes. The line locator
    // retains the original scalar (and its pin comment) for later editing.
    let pattern = Regex::new(&format!(
        r#"(?m)^\s*(?:-\s*)?{}:\s*(?<value>[^\n]+)"#,
        regex::escape(key)
    ))
    .map_err(|e| e.to_string())?;
    let located = pattern
        .captures_iter(text)
        .find_map(|c| {
            let m = c.name("value")?;
            let raw = m.as_str();
            let scalar = raw.split(" #").next()?.trim().trim_matches(['\'', '"']);
            (scalar == reference && !used.contains(&m.start())).then_some((m.start(), m.end(), raw))
        })
        .ok_or_else(|| {
            format!(
                "cannot locate {pointer}; flow-style or aliased YAML scalars are not supported yet"
            )
        })?;
    used.insert(located.0);
    let image = kind == DependencyKind::ContainerImage || reference.starts_with("docker://");
    let (name, package, current, digest, resolved) = if image {
        let value = reference.strip_prefix("docker://").unwrap_or(reference);
        let (value, digest) = value
            .split_once('@')
            .map_or((value, None), |(v, d)| (v, Some(d.to_owned())));
        let (package, current) = value
            .rsplit_once(':')
            .filter(|(_, tag)| !tag.contains('/'))
            .unwrap_or((value, "latest"));
        (package.into(), package.into(), current.into(), digest, None)
    } else {
        let Some((name, reference)) = reference.rsplit_once('@') else {
            return Ok(());
        };
        let parts: Vec<_> = name.split('/').collect();
        if parts.len() < 2 {
            return Ok(());
        }
        let digest = (reference.len() == 40 && reference.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| reference.to_owned());
        let comment = located
            .2
            .split_once(" #")
            .map(|(_, c)| c.trim())
            .and_then(|c| c.split_whitespace().next());
        let version = if digest.is_some() {
            comment
                .filter(|v| v.starts_with('v') && v[1..].starts_with(|c: char| c.is_ascii_digit()))
        } else {
            Some(reference)
        };
        (
            name.into(),
            format!("{}/{}", parts[0], parts[1]),
            reference.into(),
            digest,
            version.map(str::to_owned),
        )
    };
    out.push(Dependency {
        manager: Manager::GithubActions,
        file: path.into(),
        location: Location::new(text, located.0, located.1, pointer.into()),
        name,
        package,
        kind: if image {
            DependencyKind::ContainerImage
        } else {
            kind
        },
        current,
        resolved,
        digest,
        source: if image {
            Source::Docker
        } else {
            Source::GithubTags
        },
        scheme: if image { Scheme::Docker } else { Scheme::Loose },
        extract: None,
    });
    Ok(())
}
