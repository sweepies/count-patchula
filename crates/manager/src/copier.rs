//! Copier answers files: the template in `_src_path`, at the tag in `_commit`,
//! looked up as GitHub tags or, on GitLab and Gitea/Forgejo, as forge tags.
//! Newer tags are only reported. Copier regenerates a project from its
//! template, so bumping `_commit` alone would drop the template's changes.
use crate::*;
use patchula_versioning::{Ecosystem, Versioning};

/// `.copier-answers.yml`, `.copier-answers.yaml` and named variants such as
/// `.copier-answers.backend.yml`.
pub(super) fn selected(leaf: &str) -> bool {
    leaf.strip_prefix(".copier-answers")
        .and_then(|rest| {
            rest.strip_suffix(".yml")
                .or_else(|| rest.strip_suffix(".yaml"))
        })
        .is_some_and(|middle| middle.is_empty() || (middle.len() > 1 && middle.starts_with('.')))
}

/// The host and repository path of a template URL, or why it has none.
fn location(src: &str) -> Result<(String, String), String> {
    let src = src.trim();
    let rest = src.strip_prefix("git+").unwrap_or(src);
    let (host, path) = if let Some(path) = rest.strip_prefix("gh:") {
        ("github.com", path)
    } else if let Some(path) = rest.strip_prefix("gl:") {
        ("gitlab.com", path)
    } else if let Some((scheme, rest)) = rest.split_once("://") {
        if !matches!(scheme, "https" | "http" | "ssh" | "git") {
            return Err(format!(
                "a {scheme}:// template has no tags the Count can look up"
            ));
        }
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let host = authority.rsplit('@').next().unwrap_or(authority);
        (host.split(':').next().unwrap_or(host), path)
    } else if let Some((user_host, path)) = rest.split_once(':')
        && let Some((_, host)) = user_host.split_once('@')
        && !host.contains('/')
    {
        (host, path)
    } else {
        return Err("a local template has no tags the Count can look up".into());
    };
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    Ok((host.to_ascii_lowercase(), path.to_owned()))
}

pub(super) fn extract(
    path: &str,
    text: &str,
    skipped: &mut Vec<Skipped>,
) -> Result<Vec<Dependency>, String> {
    let locations = yaml_locations::parse(text)?;
    let Some(document) = yaml::docs(text)?.into_iter().next() else {
        return Ok(Vec::new());
    };
    let Some(src) = document.get("_src_path").and_then(|v| v.as_str()) else {
        return Ok(Vec::new());
    };
    let skip = |reason: String| Skipped {
        manager: Manager::Copier,
        file: path.into(),
        line: locations.get("/0/_src_path").map(|l| l.line),
        value: src.into(),
        reason,
    };
    let source = location(src).and_then(|(host, repo)| {
        tag_source(&host, &repo).ok_or_else(|| {
            format!(
                "no tag source for {host}/{repo}: the Count looks up templates in GitHub, GitLab, Gitea and Forgejo repositories"
            )
        })
    });
    let (source, package) = match source {
        Ok(found) => found,
        Err(reason) => {
            skipped.push(skip(reason));
            return Ok(Vec::new());
        }
    };
    let Some(commit) = document.get("_commit").and_then(|v| v.as_str()) else {
        skipped.push(skip("the answers file records no `_commit`".into()));
        return Ok(Vec::new());
    };
    if Ecosystem::Pep440.parse(commit).is_err() {
        skipped.push(skip(format!(
            "`_commit` {commit} is not a release tag, so there is nothing to compare newer tags with"
        )));
        return Ok(Vec::new());
    }
    let location = locations
        .get("/0/_commit")
        .cloned()
        .ok_or("cannot locate `_commit`")?;
    Ok(vec![Dependency {
        manager: Manager::Copier,
        file: path.into(),
        location,
        name: src.into(),
        package,
        kind: DependencyKind::Template,
        current: commit.into(),
        resolved: None,
        digest: None,
        source,
        scheme: Scheme::Pep440,
        extract: None,
    }])
}
