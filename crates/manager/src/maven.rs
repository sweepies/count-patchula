//! `pom.xml`: parent, dependencies, managed dependencies, plugins and build
//! extensions. `${property}` versions are edited where the property is
//! defined in `<properties>`; only the first artifact referencing a property
//! is emitted, so artifacts sharing one never receive conflicting edits.
use crate::*;
use std::collections::BTreeSet;

/// A leaf element's trimmed text span, with its element path. Parent
/// elements carry an instance number so siblings stay distinct.
struct Leaf {
    path: Vec<String>,
    start: usize,
    end: usize,
}

/// A small XML scanner: element paths and leaf text spans, skipping comments,
/// CDATA, processing instructions and declarations.
fn leaves(text: &str) -> Result<Vec<Leaf>, String> {
    let mut out = Vec::new();
    let mut stack: Vec<(String, usize, bool)> = Vec::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut i = 0;
    while let Some(offset) = text[i..].find('<') {
        let open = i + offset;
        let rest = &text[open..];
        let skip = |end: &str| rest.find(end).map(|n| open + n + end.len());
        if rest.starts_with("<!--") {
            i = skip("-->").ok_or("unterminated XML comment")?;
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            i = skip("]]>").ok_or("unterminated CDATA")?;
            continue;
        }
        if rest.starts_with("<?") {
            i = skip("?>").ok_or("unterminated processing instruction")?;
            continue;
        }
        if rest.starts_with("<!") {
            i = skip(">").ok_or("unterminated declaration")?;
            continue;
        }
        let close = open + rest.find('>').ok_or("unterminated XML tag")?;
        let tag = &text[open + 1..close];
        i = close + 1;
        if let Some(name) = tag.strip_prefix('/') {
            let name = name.trim();
            let (top, body, nested) = stack.pop().ok_or("unbalanced XML")?;
            if top.split('#').next() != Some(name) {
                return Err(format!("mismatched </{name}>"));
            }
            if !nested {
                let raw = &text[body..open];
                let trimmed = raw.trim();
                let start = body + (raw.len() - raw.trim_start().len());
                let mut path: Vec<String> = stack.iter().map(|(n, _, _)| n.clone()).collect();
                path.push(top);
                out.push(Leaf {
                    path,
                    start,
                    end: start + trimmed.len(),
                });
            }
            continue;
        }
        let self_closing = tag.ends_with('/');
        let name = tag
            .trim_end_matches('/')
            .split_whitespace()
            .next()
            .ok_or("empty XML tag")?
            .to_string();
        if let Some(parent) = stack.last_mut() {
            parent.2 = true;
        }
        if self_closing {
            continue;
        }
        // Number each element instance under its parent path.
        let prefix: Vec<_> = stack.iter().map(|(n, _, _)| n.as_str()).collect();
        let key = format!("{}/{name}", prefix.join("/"));
        let count = counts.entry(key).or_default();
        *count += 1;
        stack.push((format!("{name}#{count}"), close + 1, false));
    }
    if !stack.is_empty() {
        return Err("unclosed XML element".into());
    }
    Ok(out)
}

fn bare(segment: &str) -> &str {
    segment.split('#').next().unwrap_or(segment)
}

pub(super) fn extract(path: &str, text: &Lines<'_>) -> Result<Vec<Dependency>, String> {
    let leaves = leaves(text)?;
    let value = |l: &Leaf| &text[l.start..l.end];
    // project/properties/<name>
    let properties: BTreeMap<&str, &Leaf> = leaves
        .iter()
        .filter(|l| {
            l.path.len() == 3 && bare(&l.path[0]) == "project" && bare(&l.path[1]) == "properties"
        })
        .map(|l| (bare(&l.path[2]), l))
        .collect();
    // Group the direct children of each coordinate element instance.
    let mut groups: BTreeMap<String, (DependencyKind, BTreeMap<&str, &Leaf>)> = BTreeMap::new();
    for leaf in &leaves {
        let Some((field, owner)) = leaf.path.split_last() else {
            continue;
        };
        let names: Vec<_> = owner.iter().map(|s| bare(s)).collect();
        let kind = match names.as_slice() {
            ["project", "parent"] => DependencyKind::Build,
            ["project", "dependencies", "dependency"]
            | [
                "project",
                "dependencyManagement",
                "dependencies",
                "dependency",
            ] => DependencyKind::Production,
            ["project", "build", "plugins", "plugin"]
            | ["project", "build", "pluginManagement", "plugins", "plugin"]
            | ["project", "build", "extensions", "extension"] => DependencyKind::Build,
            _ => continue,
        };
        groups
            .entry(owner.join("/"))
            .or_insert_with(|| (kind, BTreeMap::new()))
            .1
            .insert(bare(field), leaf);
    }
    let mut groups: Vec<_> = groups.into_values().collect();
    // Document order, so the first artifact using a property owns it.
    groups.sort_by_key(|(_, fields)| fields.values().map(|l| l.start).min());
    let mut claimed = BTreeSet::new();
    let mut out = Vec::new();
    for (kind, fields) in groups {
        let (Some(artifact), Some(version)) = (fields.get("artifactId"), fields.get("version"))
        else {
            continue;
        };
        let group = match fields.get("groupId") {
            Some(group) => value(group),
            None if kind == DependencyKind::Build => "org.apache.maven.plugins",
            None => continue,
        };
        let (group, artifact) = (group, value(artifact));
        if group.contains("${") || artifact.contains("${") {
            continue;
        }
        let raw = value(version);
        let (location, current) =
            if let Some(name) = raw.strip_prefix("${").and_then(|r| r.strip_suffix('}')) {
                let Some(property) = properties.get(name) else {
                    continue;
                };
                if !claimed.insert(name) {
                    continue;
                }
                (
                    Location::new(
                        text,
                        property.start,
                        property.end,
                        format!("/properties/{name}"),
                    ),
                    value(property),
                )
            } else {
                (
                    Location::new(
                        text,
                        version.start,
                        version.end,
                        format!("/{group}:{artifact}/version"),
                    ),
                    raw,
                )
            };
        if current.is_empty() || current.contains("${") {
            continue;
        }
        out.push(Dependency {
            cooldown: None,
            manager: Manager::Maven,
            file: path.into(),
            location,
            form: EditForm::Scalar(Syntax::Bare),
            name: format!("{group}:{artifact}"),
            package: format!("{group}:{artifact}"),
            kind,
            current: current.into(),
            resolved: (!current.starts_with(['[', '('])).then(|| current.into()),
            digest: None,
            source: Source::Maven,
            scheme: Scheme::Maven,
            extract: None,
            included_by: None,
            registry: None,
        });
    }
    Ok(out)
}

/// Whether `path` is a Maven settings file, by its name.
pub(crate) fn settings_file(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|leaf| leaf.to_ascii_lowercase().ends_with("settings.xml"))
}

/// Whether `path` is a file the Count reads for Maven repositories by its
/// name; POMs other files name are found by `referenced`.
pub(crate) fn repository_file(path: &str) -> bool {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    leaf == "pom.xml"
        || settings_file(path)
        || path == ".mvn/maven.config"
        || path.ends_with("/.mvn/maven.config")
}

/// `named`, relative to the folder of `from`, as a repository path; none
/// when it leaves the repository.
fn relative(from: &str, named: &str) -> Option<String> {
    let mut parts: Vec<&str> = from.split('/').collect();
    parts.pop();
    for part in named.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// The paths a module or parent named `named` from `from` can be: the file
/// itself, and `pom.xml` in it when it is a folder.
fn pom_at(from: &str, named: &str) -> Vec<String> {
    let Some(path) = relative(from, named.trim()) else {
        return Vec::new();
    };
    if path.to_ascii_lowercase().ends_with(".xml") {
        vec![path]
    } else {
        vec![format!("{path}/pom.xml"), path]
    }
}

/// An element's whole text, however comments split it; none when it holds
/// elements.
fn whole_text(node: roxmltree::Node<'_, '_>) -> Option<String> {
    let mut out = String::new();
    for child in node.children() {
        if child.is_element() {
            return None;
        }
        if let Some(text) = child.text().filter(|_| child.is_text()) {
            out.push_str(text);
        }
    }
    Some(out.trim().to_owned())
}

/// Short `.mvn/maven.config` options that take no value, which commons-cli
/// lets run together, as in `-Bs settings.xml`.
const FLAGS: &str = "BXeqoUNCcVvh";

/// An option's value written in the same word, after an optional `=`.
fn value(tail: &str) -> Option<&str> {
    let tail = tail.strip_prefix('=').unwrap_or(tail);
    (!tail.is_empty()).then_some(tail)
}

/// What one `.mvn/maven.config` option names: a settings file, a global
/// settings file or a POM, with its value when written in the same word.
fn file_option(word: &str) -> Option<(&'static str, Option<&str>)> {
    if let Some(long) = word.strip_prefix("--") {
        let (name, value) = match long.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (long, None),
        };
        // commons-cli takes an unambiguous prefix of a long option.
        return ["settings", "global-settings", "file"]
            .into_iter()
            .find(|option| name.len() >= 2 && option.starts_with(name))
            .map(|option| (option, value));
    }
    let short = word.strip_prefix('-')?;
    if matches!(short, "fae" | "ff" | "fn") {
        return None;
    }
    // Flags may run together before the option that takes a value.
    let short = short.trim_start_matches(|c| FLAGS.contains(c));
    if let Some(tail) = short.strip_prefix("gs") {
        return Some(("global-settings", value(tail)));
    }
    if let Some(tail) = short.strip_prefix('s') {
        return Some(("settings", value(tail)));
    }
    if let Some(tail) = short.strip_prefix('f') {
        return Some(("file", value(tail)));
    }
    None
}

/// The files Maven reads that the Count only learns of from other files:
/// POMs `.mvn/maven.config` (`-f`), a `<module>`, a `<subproject>` or a
/// parent's `<relativePath>` names, and settings `-s` names, each with the
/// file naming it, the paths it can be, and whether it must exist. A parent
/// POM that is not in the repository is fetched from a repository instead,
/// so it need not.
pub(crate) fn referenced(files: &Files) -> Vec<(String, Vec<String>, bool)> {
    let mut out = Vec::new();
    // An unread file names nothing more to read; the lookups it shapes fail
    // through `private_repositories`.
    for (path, text) in files
        .iter()
        .filter_map(|(path, text)| Some((path, text.read().ok()?)))
    {
        if path == ".mvn/maven.config" || path.ends_with("/.mvn/maven.config") {
            let base = path.strip_suffix(".mvn/maven.config").unwrap_or("");
            let mut words = text.split_whitespace();
            while let Some(word) = words.next() {
                let Some((option, value)) = file_option(word) else {
                    continue;
                };
                let named = value.or_else(|| words.next()).unwrap_or("");
                let from = format!("{base}x");
                let targets = match option {
                    "file" => pom_at(&from, named),
                    _ => relative(&from, named).into_iter().collect(),
                };
                out.push((path.clone(), targets, true));
            }
            continue;
        }
        if !path.to_ascii_lowercase().ends_with(".xml") {
            continue;
        }
        let Ok(document) = roxmltree::Document::parse(text) else {
            continue;
        };
        if document.root_element().tag_name().name() != "project" {
            continue;
        }
        for node in document.descendants().filter(|n| n.is_element()) {
            let parent = node.parent_element().map(|p| p.tag_name().name());
            let required = match (node.tag_name().name(), parent) {
                ("module", Some("modules")) | ("subproject", Some("subprojects")) => true,
                ("relativePath", Some("parent")) => false,
                _ => continue,
            };
            let Some(named) = whole_text(node) else {
                out.push((path.clone(), Vec::new(), true));
                continue;
            };
            if named.is_empty() {
                continue;
            }
            out.push((path.clone(), pom_at(path, &named), required));
        }
    }
    out
}

/// Why a repository's Maven lookups cannot be taken to be Maven Central's:
/// a repository or mirror besides Central and Google's in any POM (named
/// `pom.xml` or named by another), in any profile, or in a settings file in
/// the repository; settings or a POM `.mvn/maven.config` or a module names
/// that the Count does not read; or a file it cannot read. Maven tries
/// repositories in order and no file records which one served an artifact,
/// so any other repository anywhere makes every lookup fail.
pub(crate) fn private_repositories(files: &Files) -> Option<crate::origins::Repositories> {
    use crate::origins::Named;
    let mut out = crate::origins::Repositories::default();
    let referenced = referenced(files);
    for (from, targets, required) in &referenced {
        if *required && !targets.iter().any(|target| files.contains_key(target)) {
            out.close(format!(
                "a file {from} names, which the Count does not read"
            ));
            return Some(out);
        }
    }
    let poms: BTreeSet<&str> = referenced
        .iter()
        .flat_map(|(_, targets, _)| targets)
        .map(String::as_str)
        .filter(|target| files.contains_key(*target))
        .collect();
    for (path, text) in files
        .iter()
        .filter(|(path, _)| repository_file(path) || poms.contains(path.as_str()))
    {
        let text = match text.read() {
            Ok(text) => text,
            Err(unread) => {
                out.close(unread.reason(path));
                return Some(out);
            }
        };
        if path.ends_with("maven.config") {
            continue;
        }
        let Ok(document) = roxmltree::Document::parse(text) else {
            // Another tool's file that only ends in `settings.xml`; one that
            // looks like Maven's but does not parse here is not read.
            if settings_file(path) && !poms.contains(path.as_str()) && !text.contains("<settings") {
                continue;
            }
            out.close(format!("{path}, which the Count cannot read"));
            return Some(out);
        };
        let root = document.root_element().tag_name().name();
        if settings_file(path) && !poms.contains(path.as_str()) && root != "settings" {
            continue;
        }
        for node in document.descendants().filter(|n| n.is_element()) {
            let name = node.tag_name().name();
            // A repository where Maven reads one, not a plugin's setting of
            // the same name.
            let list = node.parent_element().map(|p| p.tag_name().name());
            if !matches!(
                (name, list),
                ("repository", Some("repositories"))
                    | ("pluginRepository", Some("pluginRepositories"))
                    | ("mirror", Some("mirrors"))
            ) {
                continue;
            }
            // Where the project deploys to is not where it resolves from.
            if node
                .ancestors()
                .any(|a| a.tag_name().name() == "distributionManagement")
            {
                continue;
            }
            let url = node
                .children()
                .find(|c| c.tag_name().name() == "url")
                .and_then(whole_text);
            let public = url.as_ref().is_some_and(|url| {
                Named::parse(url, &crate::gradle::MAVEN_PUBLIC) == Named::Public
            });
            if !public {
                let reason = match url.as_deref().map(crate::redacted) {
                    Some(url) => format!("a {name} in {path} at {url}"),
                    None => format!("a {name} in {path}"),
                };
                out.close(reason);
                return Some(out);
            }
        }
    }
    out.any().then_some(out)
}

pub(crate) static DESCRIPTOR: Descriptor = Descriptor;

pub(crate) struct Descriptor;

impl crate::Ecosystem for Descriptor {
    fn proposing(&self, dep: &Dependency) -> Proposing {
        Proposing {
            range: Range::Installed,
            runtime_toolchain: false,
            written: Written::Range,
            keeps_satisfied: false,
            unversioned: Unversioned {
                looked_up: false,
                opaque: false,
            },
            short_tags: ShortTags::Fixed,
            selector_resolved: false,
            resolved_via_listing: dep.kind == DependencyKind::Tool,
            pin: Pin {
                commit: false,
                needs_digest: matches!(dep.source, Source::Docker | Source::Oci)
                    && matches!(
                        dep.kind,
                        DependencyKind::Action
                            | DependencyKind::ReusableWorkflow
                            | DependencyKind::ContainerImage
                            | DependencyKind::Chart
                    ),
                from_catalog: false,
            },
            lock: Lock {
                keeps_ranges: false,
                movable: false,
                transitive_movable: dep.kind == DependencyKind::Transitive
                    && dep.source == Source::Crates,
                reported: false,
            },
            edits: Edits::Manifest,
            advisories: Advisories::BySource,
        }
    }
    fn selected(&self, _config: &Config, path: &str) -> bool {
        path.rsplit('/').next() == Some("pom.xml")
    }
    fn registry_file(&self, path: &str) -> bool {
        crate::roles::eco_roles(Manager::Maven, path).registry
    }
    fn more<'a>(&self, files: &'a Files) -> Option<crate::PathPredicate<'a>> {
        let referenced: std::collections::BTreeSet<String> = referenced(files)
            .into_iter()
            .flat_map(|(_, targets, _)| targets)
            .collect();
        Some(Box::new(move |path| referenced.contains(path)))
    }
    fn prepare<'a>(
        &self,
        _scanner: &'a Scanner,
        files: &'a Files,
        _shared: &'a Shared<'a>,
    ) -> Result<Prepared<'a>, String> {
        Ok(Prepared {
            extract: Some(Box::new(|path, text, out| {
                file_result(out, path, extract(path, text));
                Ok(())
            })),
            finish: Some(Box::new(move |out| {
                let Some(repositories) = private_repositories(files) else {
                    return Ok(());
                };
                out.warnings.extend(repositories.warnings.iter().cloned());
                let Some(reason) = repositories.closed else {
                    return Ok(());
                };
                for dep in out
                    .dependencies
                    .iter_mut()
                    .filter(|dep| dep.manager == Manager::Maven && dep.source == Source::Maven)
                {
                    dep.registry = origins::Named::Unusable(reason.clone()).registry();
                }
                Ok(())
            })),
        })
    }
}
