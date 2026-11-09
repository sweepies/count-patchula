//! `.pre-commit-config.yaml`: each hook repository's `rev`, looked up as
//! GitHub tags or, on GitLab and Gitea/Forgejo, as forge tags.
use crate::*;

/// The lookup identity of a hook repository URL, or `None` for local hooks,
/// commit pins and hosts without a supported tag API.
fn identity(url: &str) -> Option<(Source, String)> {
    let rest = url
        .trim()
        .strip_prefix("https://")
        .or_else(|| url.trim().strip_prefix("http://"))?;
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let (host, path) = rest.split_once('/')?;
    crate::tag_source(&host.to_ascii_lowercase(), path)
}

pub(super) fn extract(path: &str, text: &str) -> Result<Vec<Dependency>, String> {
    let locations = crate::yaml_locations::parse(text)?;
    let Some(document) = crate::yaml::docs(text)?.into_iter().next() else {
        return Ok(Vec::new());
    };
    let Some(repos) = document.get("repos").and_then(|r| r.as_sequence()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (index, repo) in repos.iter().enumerate() {
        let (Some(url), Some(rev)) = (
            repo.get("repo").and_then(|v| v.as_str()),
            repo.get("rev").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        // Commit pins need their tag from a frozen comment; not yet supported.
        if commit_hash(rev) {
            continue;
        }
        let Some((source, package)) = identity(url) else {
            continue;
        };
        let pointer = format!("/0/repos/{index}/rev");
        let location = locations
            .get(&pointer)
            .cloned()
            .ok_or_else(|| format!("cannot locate pre-commit rev {pointer}"))?;
        out.push(Dependency {
            manager: Manager::PreCommit,
            file: path.into(),
            location,
            name: package.clone(),
            package,
            kind: DependencyKind::Tool,
            current: rev.into(),
            resolved: Some(rev.into()),
            digest: None,
            source,
            scheme: Scheme::Loose,
            extract: None,
            included_by: None,
            registry: None,
        });
    }
    Ok(out)
}

pub(crate) static DESCRIPTOR: Descriptor = Descriptor;

pub(crate) struct Descriptor;

impl crate::Ecosystem for Descriptor {
    fn selected(&self, _config: &Config, path: &str) -> bool {
        matches!(
            path.rsplit('/').next().unwrap_or(path),
            ".pre-commit-config.yaml" | ".pre-commit-config.yml"
        )
    }
    fn prepare<'a>(
        &self,
        _scanner: &'a Scanner,
        _files: &'a Files,
        _shared: &'a Shared<'a>,
    ) -> Result<Prepared<'a>, String> {
        Ok(Prepared {
            extract: Some(Box::new(|path, text, out| {
                file_result(out, path, extract(path, text));
                Ok(())
            })),
            finish: None,
        })
    }
}
