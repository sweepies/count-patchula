use crate::*;
use regex::Regex;
use serde_yaml_ng::Value;

fn version_comment(value: &str) -> bool {
    static VERSION: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^v?[0-9]+(?:\.[0-9]+){0,2}(?:[-+][0-9A-Za-z.-]+)?$").expect("version comment")
    });
    VERSION.is_match(value)
}

pub(super) fn extract(path: &str, text: &str) -> Result<Vec<Dependency>, String> {
    let root: Value = serde_yaml_ng::from_str(text).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut used = std::collections::BTreeSet::new();
    if let Some(jobs) = root.get("jobs").and_then(Value::as_mapping) {
        for (job, value) in jobs {
            let job = job.as_str().unwrap_or("?");
            if let Some(runs_on) = value.get("runs-on") {
                let scope = runner_scope(text, job)?;
                runners(
                    path,
                    text,
                    runs_on,
                    &format!("/jobs/{job}/runs-on"),
                    &scope,
                    &mut used,
                    &mut out,
                )?;
            }
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
            setup_input(path, text, reference, step, &format!("{pointer}/{i}"), out)?;
        }
    }
    Ok(())
}

fn setup_input(
    path: &str,
    text: &str,
    reference: &str,
    step: &Value,
    pointer: &str,
    out: &mut Vec<Dependency>,
) -> Result<(), String> {
    let action = reference.split('@').next().unwrap_or(reference);
    let (key, name, package, extract) = match action {
        "oven-sh/setup-bun" => ("bun-version", "bun", "bun", None),
        "actions/setup-node" => (
            "node-version",
            "node",
            "actions/node-versions",
            Some(r"^v?(?<version>\d+\.\d+\.\d+)(?:-|$)"),
        ),
        "actions/setup-go" => (
            "go-version",
            "go",
            "actions/go-versions",
            Some(r"^v?(?<version>\d+\.\d+\.\d+)(?:-|$)"),
        ),
        "golangci/golangci-lint-action" => (
            "version",
            "golangci/golangci-lint",
            "golangci/golangci-lint",
            None,
        ),
        "goreleaser/goreleaser-action" => (
            "version",
            "goreleaser/goreleaser",
            "goreleaser/goreleaser",
            None,
        ),
        _ => return Ok(()),
    };
    let Some(value) = step.get("with").and_then(|v| v.get(key)).and_then(|v| {
        v.as_str()
            .map(str::to_owned)
            .or_else(|| v.as_u64().map(|n| n.to_string()))
    }) else {
        return Ok(());
    };
    if value.contains("${{") {
        return Ok(());
    }
    let pointer = format!("/0{pointer}/with/{key}");
    let locations = crate::yaml_locations::parse(text)?;
    let location = locations
        .get(&pointer)
        .cloned()
        .ok_or_else(|| format!("cannot locate setup input {pointer}"))?;
    out.push(Dependency {
        manager: Manager::GithubActions,
        file: path.into(),
        location,
        name: name.into(),
        package: package.into(),
        kind: DependencyKind::Tool,
        current: value.clone(),
        resolved: (value != "latest").then_some(value),
        digest: None,
        source: if action == "oven-sh/setup-bun" {
            Source::Npm
        } else {
            Source::GithubReleases
        },
        scheme: if action == "oven-sh/setup-bun" {
            Scheme::Semver
        } else {
            Scheme::Loose
        },
        extract: extract.map(str::to_owned),
    });
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
            comment.filter(|v| version_comment(v))
        } else {
            Some(reference)
        };
        (
            format!("{}/{}", parts[0], parts[1]),
            format!("{}/{}", parts[0], parts[1]),
            version.unwrap_or(reference).into(),
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

fn runners(
    path: &str,
    text: &str,
    value: &Value,
    pointer: &str,
    scope: &std::ops::Range<usize>,
    used: &mut std::collections::BTreeSet<usize>,
    out: &mut Vec<Dependency>,
) -> Result<(), String> {
    if let Some(labels) = value.as_sequence() {
        // A self-hosted job may use OS-looking custom labels.
        if labels.iter().any(|v| v.as_str() == Some("self-hosted")) {
            return Ok(());
        }
        for (i, label) in labels.iter().enumerate() {
            runners(
                path,
                text,
                label,
                &format!("{pointer}/{i}"),
                scope,
                used,
                out,
            )?;
        }
        return Ok(());
    }
    if value.is_mapping() {
        // Runner groups select organizational infrastructure, not hosted images.
        if value.get("group").is_none()
            && let Some(labels) = value.get("labels")
        {
            runners(
                path,
                text,
                labels,
                &format!("{pointer}/labels"),
                scope,
                used,
                out,
            )?;
        }
        return Ok(());
    }
    let Some(label) = value.as_str().filter(|s| !s.contains("${{")) else {
        return Ok(());
    };
    let Some((os, tail)) = label
        .split_once('-')
        .filter(|(os, _)| matches!(*os, "ubuntu" | "macos" | "windows"))
    else {
        return Ok(());
    };
    let (version, suffix) = tail.split_once('-').map_or((tail, ""), |(v, s)| (v, s));
    if version != "latest" && !version.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return Ok(());
    }
    if version.is_empty() || !matches!(suffix, "" | "arm" | "intel" | "vs2026" | "vs2026-arm") {
        return Ok(());
    }
    let package = if suffix.is_empty() {
        os.into()
    } else {
        format!("{os}-{suffix}")
    };
    let start = text[scope.clone()]
        .match_indices(label)
        .find_map(|(start, _)| {
            let start = scope.start + start;
            let line_start = text[..start].rfind('\n').map_or(0, |p| p + 1);
            let prefix = &text[line_start..start];
            let in_comment = prefix.contains('#');
            (!in_comment
                && !used.contains(&start)
                && (prefix.split_once(':').is_some_and(|(key, _)| {
                    matches!(key.trim().trim_matches(['\'', '"']), "runs-on" | "labels")
                }) || prefix.trim_start().starts_with('-')))
            .then_some(start)
        })
        .ok_or_else(|| {
            format!("cannot locate runner {pointer}; aliased runner labels are unsupported")
        })?;
    used.insert(start);
    out.push(Dependency {
        manager: Manager::GithubActions,
        file: path.into(),
        location: Location::new(text, start, start + label.len(), pointer.into()),
        name: package.clone(),
        package,
        kind: DependencyKind::Runner,
        current: version.into(),
        resolved: (version != "latest").then(|| version.into()),
        digest: None,
        source: Source::GithubRunners,
        scheme: Scheme::Loose,
        extract: None,
    });
    Ok(())
}

// Restrict literal locations to this job's runs-on subtree. Identical labels
// elsewhere (especially self-hosted jobs and run text) must never be edited.
fn runner_scope(text: &str, job: &str) -> Result<std::ops::Range<usize>, String> {
    let lines: Vec<_> = text
        .split_inclusive('\n')
        .scan(0, |offset, line| {
            let start = *offset;
            *offset += line.len();
            Some((start, line))
        })
        .collect();
    let jobs = lines
        .iter()
        .position(|(_, line)| {
            !line.starts_with(char::is_whitespace)
                && line
                    .split('#')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .strip_suffix(':')
                    .is_some_and(|key| key.trim_matches(['\'', '"']) == "jobs")
        })
        .ok_or("cannot locate block jobs mapping")?;
    let jobs_end = (jobs + 1..lines.len())
        .find(|&i| {
            let line = lines[i].1;
            !line.trim().is_empty()
                && !line.starts_with(char::is_whitespace)
                && !line.starts_with('#')
        })
        .unwrap_or(lines.len());
    let job_indent = (jobs + 1..jobs_end)
        .filter_map(|i| {
            let line = lines[i].1;
            (!line.trim().is_empty() && !line.trim_start().starts_with('#'))
                .then(|| line.len() - line.trim_start().len())
        })
        .min()
        .ok_or("cannot locate job mapping")?;
    let job_start = (jobs + 1..jobs_end)
        .find(|&i| {
            let raw = lines[i].1;
            let line = raw.split('#').next().unwrap_or("").trim();
            raw.len() - raw.trim_start().len() == job_indent
                && line
                    .strip_suffix(':')
                    .is_some_and(|key| key.trim_matches(['\'', '"']) == job)
        })
        .ok_or_else(|| format!("cannot locate block job {job}"))?;
    let indent = lines[job_start].1.len() - lines[job_start].1.trim_start().len();
    let job_end = (job_start + 1..lines.len())
        .find(|&i| {
            let line = lines[i].1;
            !line.trim().is_empty()
                && !line.trim_start().starts_with('#')
                && line.len() - line.trim_start().len() <= indent
        })
        .unwrap_or(lines.len());
    let runner = (job_start + 1..job_end)
        .filter(|&i| {
            lines[i]
                .1
                .trim_start()
                .split_once(':')
                .is_some_and(|(key, _)| key.trim_matches(['\'', '"']) == "runs-on")
        })
        .min_by_key(|&i| lines[i].1.len() - lines[i].1.trim_start().len())
        .ok_or("cannot locate literal runs-on; aliases are unsupported")?;
    let indent = lines[runner].1.len() - lines[runner].1.trim_start().len();
    let end = (runner + 1..job_end)
        .find(|&i| {
            let line = lines[i].1;
            !line.trim().is_empty()
                && !line.trim_start().starts_with('#')
                && line.len() - line.trim_start().len() <= indent
        })
        .map_or_else(
            || lines.get(job_end).map_or(text.len(), |l| l.0),
            |i| lines[i].0,
        );
    Ok(lines[runner].0..end)
}

/// Render an action scalar at its extracted location, retaining subpaths,
/// quoting, whitespace, and the rest of an existing version comment.
pub fn replacement(
    dep: &Dependency,
    text: &str,
    value: &str,
    digest: Option<&str>,
) -> Result<String, String> {
    let raw = text
        .get(dep.location.start..dep.location.end)
        .ok_or("invalid action location")?;
    let (scalar, comment) = raw
        .split_once(" #")
        .map_or((raw, None), |(a, b)| (a, Some(b)));
    let at = scalar.rfind('@').ok_or("action scalar has no ref")?;
    let old_ref = scalar[at + 1..].trim_end_matches(['\'', '"', ' ', '\t', '\r']);
    let end = at + 1 + old_ref.len();
    let mut result = format!(
        "{}{}{}",
        &scalar[..at + 1],
        digest.unwrap_or(value),
        &scalar[end..]
    );
    if let Some(comment) = comment {
        result.push_str(" #");
        let old = comment.split_whitespace().next().unwrap_or("");
        if version_comment(old) && (dep.digest.is_none() || dep.resolved.as_deref() == Some(old)) {
            let lead = comment.len() - comment.trim_start().len();
            result.push_str(&comment[..lead]);
            result.push_str(value);
            result.push_str(&comment[lead + old.len()..]);
        } else if digest.is_some() && dep.digest.is_none() {
            let lead = comment.len() - comment.trim_start().len();
            result.push_str(&comment[..lead]);
            result.push_str(value);
            result.push(' ');
            result.push_str(comment.trim_start());
        } else {
            result.push_str(comment);
        }
    } else if digest.is_some() && dep.resolved.is_some() {
        let suffix = result.split_off(result.trim_end_matches([' ', '\t', '\r']).len());
        result.push_str(" # ");
        result.push_str(value);
        result.push_str(&suffix);
    }
    Ok(result)
}

pub fn runner_replacement(dep: &Dependency, text: &str, value: &str) -> Result<String, String> {
    if dep.kind != DependencyKind::Runner {
        return Err("not a runner dependency".into());
    }
    let label = text
        .get(dep.location.start..dep.location.end)
        .ok_or("invalid runner location")?;
    Ok(label.replacen(&dep.current, value, 1))
}
