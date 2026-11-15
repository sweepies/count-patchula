use crate::*;
use regex::Regex;
use serde_yaml_ng::Value;

pub(crate) fn version_comment(value: &str) -> bool {
    static VERSION: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^v?[0-9]+(?:\.[0-9]+){0,2}(?:[-+][0-9A-Za-z.-]+)?$").expect("version comment")
    });
    VERSION.is_match(value)
}

/// Forgejo's documented default `DEFAULT_ACTIONS_URL`. Gitea defaults to
/// GitHub, so only files under `.forgejo/` resolve bare refs here.
const FORGEJO_ACTIONS: &str = "data.forgejo.org";

pub(super) fn extract(path: &str, text: &Lines<'_>) -> Result<Vec<Dependency>, String> {
    let bare = path.starts_with(".forgejo/").then_some(FORGEJO_ACTIONS);
    let forge = path.starts_with(".forgejo/") || path.starts_with(".gitea/");
    let root: Value = serde_yaml_ng::from_str(text).map_err(|e| e.to_string())?;
    // Every scalar is located through the file's YAML structure, parsed
    // once: a matching line inside a `run: |` script is text, never a step.
    let mut file = Workflow {
        path,
        text,
        bare,
        locations: crate::yaml_locations::parse(text)?,
        out: Vec::new(),
    };
    let mut used = std::collections::BTreeSet::new();
    if let Some(jobs) = root.get("jobs").and_then(Value::as_mapping) {
        for (job, value) in jobs {
            let job = job.as_str().unwrap_or("?");
            let at = format!("/jobs/{}", pointer_token(job));
            // Forgejo and Gitea runner labels name the instance's own runners.
            if let Some(runs_on) = value.get("runs-on").filter(|_| !forge) {
                let scope = runner_scope(text, job)?;
                runners(
                    path,
                    text,
                    runs_on,
                    &format!("/jobs/{job}/runs-on"),
                    &scope,
                    &mut used,
                    &mut file.out,
                )?;
            }
            if let Some(reference) = value.get("uses").and_then(Value::as_str) {
                file.add(
                    reference,
                    &format!("{at}/uses"),
                    &format!("{at}/uses"),
                    DependencyKind::ReusableWorkflow,
                )?;
            }
            if let Some(steps) = value.get("steps").and_then(Value::as_sequence) {
                file.steps(steps, &format!("{at}/steps"))?;
            }
            if let Some(container) = value.get("container")
                && let Some(image) = container
                    .as_str()
                    .or_else(|| container.get("image").and_then(Value::as_str))
            {
                file.add(
                    image,
                    &format!("{at}/container"),
                    &if container.is_string() {
                        format!("{at}/container")
                    } else {
                        format!("{at}/container/image")
                    },
                    DependencyKind::ContainerImage,
                )?;
            }
            if let Some(services) = value.get("services").and_then(Value::as_mapping) {
                for (name, service) in services {
                    if let Some(image) = service.get("image").and_then(Value::as_str) {
                        let pointer = format!(
                            "{at}/services/{}/image",
                            pointer_token(name.as_str().unwrap_or("?"))
                        );
                        file.add(image, &pointer, &pointer, DependencyKind::ContainerImage)?;
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
        file.steps(steps, "/runs/steps")?;
    }
    Ok(file.out)
}

/// One workflow or action file being extracted.
struct Workflow<'a, 'b> {
    path: &'a str,
    text: &'a Lines<'b>,
    bare: Option<&'a str>,
    /// Every scalar's span, by its pointer under the document (`/0`).
    locations: BTreeMap<String, Location>,
    out: Vec<Dependency>,
}

impl Workflow<'_, '_> {
    fn steps(&mut self, steps: &[Value], pointer: &str) -> Result<(), String> {
        for (i, step) in steps.iter().enumerate() {
            if let Some(reference) = step.get("uses").and_then(Value::as_str) {
                let uses = format!("{pointer}/{i}/uses");
                self.add(reference, &uses, &uses, DependencyKind::Action)?;
                self.setup_input(reference, step, &format!("{pointer}/{i}"))?;
            }
        }
        Ok(())
    }

    fn setup_input(&mut self, reference: &str, step: &Value, pointer: &str) -> Result<(), String> {
        let path = self.path;
        let action = reference.split('@').next().unwrap_or(reference);
        let action = action
            .strip_prefix("https://")
            .or_else(|| action.strip_prefix("http://"))
            .and_then(|rest| rest.split_once('/'))
            .map_or(action, |(_, path)| path);
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
        let pointer = format!("/0{pointer}/with/{}", pointer_token(key));
        let location = self
            .locations
            .get(&pointer)
            .cloned()
            .ok_or_else(|| format!("cannot locate setup input {pointer}"))?;
        self.out.push(Dependency {
            cooldown: None,
            manager: Manager::GithubActions,
            file: path.into(),
            location,
            form: EditForm::Scalar(Syntax::Yaml),
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
            included_by: None,
            registry: None,
        });
        Ok(())
    }

    /// The span of the scalar at `at` (a pointer without the document's
    /// `/0`), run on to the end of its line so the version comment is
    /// edited with it. Only a block-style scalar qualifies: nothing but
    /// whitespace and a comment may follow it, and no flow collection may
    /// open before it, where a comment added on pinning would swallow the
    /// collection's closing bracket.
    fn located(&self, at: &str) -> Result<(usize, usize), String> {
        let unsupported = || {
            format!("cannot locate {at}; flow-style or aliased YAML scalars are not supported yet")
        };
        let location = self
            .locations
            .get(&format!("/0{at}"))
            .ok_or_else(unsupported)?;
        let text: &str = self.text;
        let line_start = text[..location.start].rfind('\n').map_or(0, |n| n + 1);
        let line_end = text[location.end..]
            .find('\n')
            .map_or(text.len(), |n| location.end + n);
        let rest = &text[location.end..line_end];
        let tail = rest.trim_start_matches([' ', '\t']);
        let block = !text[line_start..location.start].contains(['{', '['])
            && (tail.trim_end_matches('\r').is_empty()
                || (tail.starts_with('#') && tail.len() < rest.len()));
        if !block {
            return Err(unsupported());
        }
        Ok((location.start, line_end))
    }

    /// `pointer` is the one the dependency records; `at` the one its
    /// scalar sits at.
    fn add(
        &mut self,
        reference: &str,
        pointer: &str,
        at: &str,
        kind: DependencyKind,
    ) -> Result<(), String> {
        let (path, text, bare) = (self.path, self.text, self.bare);
        if reference.starts_with("./") || reference.contains("${{") {
            return Ok(());
        }
        let (start, end) = self.located(at)?;
        let raw = &text[start..end];
        let image = kind == DependencyKind::ContainerImage || reference.starts_with("docker://");
        let (name, package, source, current, digest, resolved) = if image {
            let value = reference.strip_prefix("docker://").unwrap_or(reference);
            let (value, digest) = value
                .split_once('@')
                .map_or((value, None), |(v, d)| (v, Some(d.to_owned())));
            let (package, current) = value
                .rsplit_once(':')
                .filter(|(_, tag)| !tag.contains('/'))
                .unwrap_or((value, "latest"));
            (
                package.into(),
                package.into(),
                Source::Docker,
                current.into(),
                digest,
                None,
            )
        } else {
            // The first `@` ends the name, as the runner and gh-actions-lock
            // read it: a ref such as `release@2024` keeps its own `@`.
            let Some((name, reference)) = reference.split_once('@') else {
                return Ok(());
            };
            let Some((name, package, source)) = repository(name, bare) else {
                return Ok(());
            };
            let digest = commit_hash(reference).then(|| reference.to_owned());
            let comment = raw
                .split_once(" #")
                .map(|(_, c)| c.trim())
                .and_then(|c| c.split_whitespace().next());
            let version = if digest.is_some() {
                comment.filter(|v| version_comment(v))
            } else {
                Some(reference)
            };
            (
                name,
                package,
                source,
                version.unwrap_or(reference).into(),
                digest,
                version.map(str::to_owned),
            )
        };
        self.out.push(Dependency {
            cooldown: None,
            manager: Manager::GithubActions,
            file: path.into(),
            location: Location::new(text, start, end, pointer.into()),
            form: if image {
                EditForm::Image {
                    syntax: Syntax::Yaml,
                    digest_pointer: None,
                    comment: true,
                }
            } else {
                EditForm::Scalar(Syntax::Yaml)
            },
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
            source,
            scheme: if image { Scheme::Docker } else { Scheme::Loose },
            extract: None,
            included_by: None,
            registry: None,
        });
        Ok(())
    }
}

/// The name, lookup package and source of an action repository. A URL names
/// its host; a bare `owner/repo` resolves on GitHub, or on `bare` where the
/// runner's documented default action host is another forge. Hosts without a
/// supported tag API are skipped.
fn repository(name: &str, bare: Option<&str>) -> Option<(String, String, Source)> {
    let (host, path) = match name
        .strip_prefix("https://")
        .or_else(|| name.strip_prefix("http://"))
    {
        Some(rest) => {
            let (host, path) = rest.split_once('/')?;
            (Some(host.to_ascii_lowercase()), path)
        }
        None => (None, name),
    };
    let mut parts = path.split('/');
    let repo = format!(
        "{}/{}",
        parts.next().filter(|p| !p.is_empty())?,
        parts.next().filter(|p| !p.is_empty())?
    );
    match (host.as_deref(), bare) {
        (Some("github.com"), _) | (None, None) => Some((repo.clone(), repo, Source::GithubTags)),
        (None, Some(bare)) => Some((repo.clone(), format!("{bare}/{repo}"), Source::ForgeTags)),
        (Some(host), _) => crate::gitea_host(host).then(|| {
            let package = format!("{host}/{repo}");
            (package.clone(), package, Source::ForgeTags)
        }),
    }
}

fn runners(
    path: &str,
    text: &Lines<'_>,
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
    let (version, suffix) = tail.split_once('-').unwrap_or((tail, ""));
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
        cooldown: None,
        manager: Manager::GithubActions,
        file: path.into(),
        location: Location::new(text, start, start + label.len(), pointer.into()),
        form: EditForm::Scalar(Syntax::Yaml),
        name: package.clone(),
        package,
        kind: DependencyKind::Runner,
        current: version.into(),
        resolved: (version != "latest").then(|| version.into()),
        digest: None,
        source: Source::GithubRunners,
        scheme: Scheme::Loose,
        extract: None,
        included_by: None,
        registry: None,
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

/// Render an action or APM scalar at its extracted location, retaining
/// subpaths, quoting, whitespace, and the rest of an existing version comment.
/// An APM entry separates its ref with `#` rather than `@`.
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
    let separator = if dep.manager == Manager::Apm {
        '#'
    } else {
        '@'
    };
    // An action's ref starts after the first `@`, where extraction split
    // it; an APM ref after the last `#`.
    let at = if dep.manager == Manager::Apm {
        scalar.rfind(separator)
    } else {
        scalar.find(separator)
    }
    .ok_or("action scalar has no ref")?;
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

pub(crate) static DESCRIPTOR: Descriptor = Descriptor;

pub(crate) struct Descriptor;

impl crate::Ecosystem for Descriptor {
    fn proposing(&self, dep: &Dependency) -> Proposing {
        Proposing {
            written: if dep.kind == DependencyKind::Tool {
                Written::VersionAtPrecision
            } else {
                Written::TagAtPrecision
            },
            unversioned: Unversioned {
                looked_up: matches!(dep.source, Source::GithubTags | Source::ForgeTags),
                opaque: true,
            },
            short_tags: if matches!(dep.source, Source::GithubTags | Source::ForgeTags) {
                ShortTags::Floating
            } else {
                ShortTags::Fixed
            },
            selector_resolved: dep.kind == DependencyKind::Tool,
            resolved_via_listing: dep.kind == DependencyKind::Tool,
            pin: Pin {
                commit: matches!(dep.source, Source::GithubTags | Source::ForgeTags),
                needs_digest: matches!(
                    dep.kind,
                    DependencyKind::Action
                        | DependencyKind::ReusableWorkflow
                        | DependencyKind::ContainerImage
                ),
                from_catalog: true,
            },
            advisories: if matches!(
                dep.kind,
                DependencyKind::Action | DependencyKind::ReusableWorkflow
            ) {
                Advisories::GithubAction
            } else {
                Advisories::BySource
            },
            ..Proposing::BASE
        }
    }
    /// The lockfile is read into the snapshot without being extracted
    /// from: which workflows it lists is the pinning fact, and its
    /// `uses:` entries are the tool's, not dependencies to update.
    fn registry_file(&self, path: &str) -> bool {
        path == crate::roles::ACTIONS_LOCK
    }
    fn selected(&self, _config: &Config, path: &str) -> bool {
        let leaf = crate::roles::leaf(path);
        ([
            ".github/workflows/",
            ".gitea/workflows/",
            ".forgejo/workflows/",
        ]
        .iter()
        .any(|dir| path.starts_with(dir))
            && matches!(leaf.rsplit('.').next(), Some("yml" | "yaml")))
            || matches!(leaf, "action.yml" | "action.yaml")
    }
    fn prepare<'a>(
        &self,
        _scanner: &'a Scanner,
        _files: &'a Files,
        _shared: &'a Shared<'a>,
    ) -> Result<Prepared<'a>, String> {
        Ok(Prepared::per_file(extract))
    }
}
