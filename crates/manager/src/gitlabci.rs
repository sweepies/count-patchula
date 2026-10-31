//! `.gitlab-ci.yml`: job and default `image` and `services` for `gitlabci`,
//! with CI/CD component references; `include: project:` refs for
//! `gitlabci-include`. Components and project refs are GitLab tags, looked up
//! as forge tags on `host/project`.
use crate::*;
use serde_yaml_ng::Value;

/// Top-level keys that are not jobs. Every other mapping, `default` and hidden
/// `.template` jobs included, may carry `image` and `services`.
const KEYWORDS: &[&str] = &[
    "include",
    "stages",
    "variables",
    "workflow",
    "spec",
    "cache",
    "before_script",
    "after_script",
    "image",
    "services",
];
/// Predefined variables that name the GitLab instance itself.
const SERVER: &[&str] = &[
    "$CI_SERVER_FQDN",
    "${CI_SERVER_FQDN}",
    "$CI_SERVER_HOST",
    "${CI_SERVER_HOST}",
];

/// A ref under a YAML anchor may serve several projects, whose tags differ.
const SHARED: &str = "the ref is shared through a YAML anchor, so no one project's tags apply";

fn commit(reference: &str) -> bool {
    reference.len() == 40 && reference.bytes().all(|b| b.is_ascii_hexdigit())
}
fn segment(part: &str) -> bool {
    !part.is_empty()
        && !matches!(part, "." | "..")
        && part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

struct Scan<'a> {
    path: &'a str,
    text: &'a str,
    locations: &'a BTreeMap<String, Location>,
    aliases: &'a crate::yaml_locations::Aliases,
    seen: std::collections::BTreeSet<usize>,
    out: Vec<Dependency>,
    skipped: &'a mut Vec<Skipped>,
}
impl Scan<'_> {
    /// Where a value is written, following aliases to their anchor. A value
    /// reached twice, through an anchor and its aliases, is only read once.
    fn locate(&mut self, pointer: &str) -> Option<Location> {
        let location = self.locations.get(&self.aliases.resolve(pointer))?.clone();
        self.seen.insert(location.start).then_some(location)
    }
    fn skip(&mut self, manager: Manager, location: &Location, value: &str, reason: &str) {
        self.skipped.push(Skipped {
            manager,
            file: self.path.into(),
            line: Some(location.line),
            value: value.into(),
            reason: reason.into(),
        });
    }
    fn image(&mut self, image: &str, pointer: &str) {
        let Some(location) = self.locate(pointer) else {
            return;
        };
        if image.contains('$') {
            self.skip(
                Manager::Gitlabci,
                &location,
                image,
                "the image contains a GitLab CI variable",
            );
            return;
        }
        if let Some(dep) =
            images::dependency(Manager::Gitlabci, self.path, self.text, image, location)
        {
            self.out.push(dep);
        }
    }
    /// `image: name:tag` or `image: {name: name:tag, ...}`.
    fn named(&mut self, value: &Value, pointer: &str) {
        if let Some(image) = value.as_str() {
            self.image(image, pointer);
        } else if let Some(image) = value.get("name").and_then(Value::as_str) {
            self.image(image, &format!("{pointer}/name"));
        }
    }
    fn job(&mut self, job: &Value, pointer: &str) {
        if let Some(image) = job.get("image") {
            self.named(image, &format!("{pointer}/image"));
        }
        if let Some(services) = job.get("services").and_then(Value::as_sequence) {
            for (i, service) in services.iter().enumerate() {
                self.named(service, &format!("{pointer}/services/{i}"));
            }
        }
    }
    fn component(&mut self, reference: &str, pointer: &str, host: &str) {
        let Some(location) = self.locate(pointer) else {
            return;
        };
        if self.aliases.shared(pointer) {
            self.skip(Manager::Gitlabci, &location, reference, SHARED);
            return;
        }
        let Some((address, version)) = reference.rsplit_once('@') else {
            return;
        };
        let Some((server, path)) = address.split_once('/') else {
            return;
        };
        let server = if SERVER.contains(&server) {
            host.to_owned()
        } else if server.contains('$') {
            self.skip(
                Manager::Gitlabci,
                &location,
                reference,
                "the component's host is a GitLab CI variable other than $CI_SERVER_FQDN",
            );
            return;
        } else {
            server.to_ascii_lowercase()
        };
        let Some((project, _)) = path.rsplit_once('/') else {
            return;
        };
        // `~latest` floats, and a commit is a pin without a version.
        if version.starts_with('~')
            || commit(version)
            || !version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.+".contains(&b))
            || project.split('/').count() < 2
            || !project.split('/').all(segment)
        {
            return;
        }
        // The version ends the scalar; escapes would shift it.
        let raw = &self.text[location.start..location.end];
        let inner = raw.trim_matches(['"', '\'']);
        if inner != reference {
            return;
        }
        let end = location.start + raw.len() - (raw.len() - inner.len()) / 2;
        let start = end - version.len();
        self.out.push(Dependency {
            manager: Manager::Gitlabci,
            file: self.path.into(),
            location: Location::new(self.text, start, end, pointer.into()),
            name: format!("{server}/{path}"),
            package: format!("{server}/{project}"),
            kind: DependencyKind::ReusableWorkflow,
            current: version.into(),
            resolved: Some(version.into()),
            digest: None,
            source: Source::ForgeTags,
            scheme: Scheme::Loose,
            extract: None,
            included_by: None,
        });
    }
    fn project(&mut self, entry: &Value, pointer: &str, host: &str) {
        let (Some(project), Some(reference)) = (
            entry.get("project").and_then(Value::as_str),
            entry.get("ref"),
        ) else {
            return;
        };
        let Some(location) = self.locate(&format!("{pointer}/ref")) else {
            return;
        };
        let current = reference.as_str().map_or_else(
            || self.text[location.start..location.end].to_owned(),
            str::to_owned,
        );
        if self.aliases.shared(&format!("{pointer}/ref")) {
            let value = format!("{project}@{current}");
            self.skip(Manager::GitlabciInclude, &location, &value, SHARED);
            return;
        }
        if project.contains('$') || current.contains('$') {
            self.skip(
                Manager::GitlabciInclude,
                &location,
                &format!("{project}@{current}"),
                "the include contains a GitLab CI variable",
            );
            return;
        }
        let project = project.trim_matches('/');
        if commit(&current) || project.split('/').count() < 2 || !project.split('/').all(segment) {
            return;
        }
        self.out.push(Dependency {
            manager: Manager::GitlabciInclude,
            file: self.path.into(),
            location,
            name: project.into(),
            package: format!("{host}/{project}"),
            kind: DependencyKind::ReusableWorkflow,
            current: current.clone(),
            resolved: Some(current),
            digest: None,
            source: Source::ForgeTags,
            scheme: Scheme::Loose,
            extract: None,
            included_by: None,
        });
    }
    /// `include` holds one entry or a list; plain strings are local or remote files.
    fn includes(&mut self, manager: Manager, document: &Value, doc: usize, host: &str) {
        let entries: Vec<(&Value, String)> = match document.get("include") {
            Some(Value::Sequence(items)) => items
                .iter()
                .enumerate()
                .map(|(i, v)| (v, format!("/{doc}/include/{i}")))
                .collect(),
            Some(entry @ Value::Mapping(_)) => vec![(entry, format!("/{doc}/include"))],
            _ => Vec::new(),
        };
        for (entry, pointer) in entries {
            if manager == Manager::GitlabciInclude {
                self.project(entry, &pointer, host);
            } else if let Some(component) = entry.get("component").and_then(Value::as_str) {
                self.component(component, &format!("{pointer}/component"), host);
            }
        }
    }
}

pub(super) fn extract(
    manager: Manager,
    path: &str,
    text: &str,
    host: &str,
    skipped: &mut Vec<Skipped>,
) -> Result<Vec<Dependency>, String> {
    let (locations, aliases) = crate::yaml_locations::parse_aliased(text)?;
    let mut scan = Scan {
        path,
        text,
        locations: &locations,
        aliases: &aliases,
        seen: std::collections::BTreeSet::new(),
        out: Vec::new(),
        skipped,
    };
    for (doc, document) in crate::yaml::docs(text)?.iter().enumerate() {
        let Some(map) = document.as_mapping() else {
            continue;
        };
        scan.includes(manager, document, doc, host);
        if manager == Manager::GitlabciInclude {
            continue;
        }
        scan.job(document, &format!("/{doc}"));
        for (key, value) in map {
            if let Some(key) = key.as_str()
                && !KEYWORDS.contains(&key)
                && value.is_mapping()
            {
                scan.job(value, &format!("/{doc}/{}", pointer_token(key)));
            }
        }
    }
    Ok(scan.out)
}
