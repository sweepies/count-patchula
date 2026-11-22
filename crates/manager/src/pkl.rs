//! Pkl modules: the `package://` URIs their `amends`, `extends` and
//! `import` clauses name, looked up in the `pkl` source.
//!
//! A package URI names a GitHub release asset, directly or through the
//! `pkg.pkl-lang.org` redirector:
//!
//! - `package://github.com/<owner>/<repo>/releases/download/<tag>/<name>@<version>`
//! - `package://pkg.pkl-lang.org/github.com/<owner>/<repo>/<name>@<version>`
//! - `package://pkg.pkl-lang.org/<project>/<name>@<version>`
//!
//! The package is the URI without its scheme and version, which
//! `PklPackage` reads back. A release URI writes the version twice, in the
//! tag and after the `@`: each is an occurrence of the one declaration, so
//! the report merges them and one update edits both.
use crate::*;
use patchula_config::{PklPackage, VERSION_PLACEHOLDER, pkl_semver};

const UNSUPPORTED_HOST: &str = "only github.com and pkg.pkl-lang.org package URIs are supported";
const UNSUPPORTED_PATH: &str = "package URI does not name a GitHub release";
const CHECKSUM: &str = "package URIs that pin a checksum are not supported yet";
const ESCAPED: &str = "package URIs with escapes or interpolation are not supported";
const TAG_VERSION: &str = "release tag does not hold the package version exactly once";
const VERSION: &str = "package version is not a semantic version";

/// A package URI read: the package, its name and version, and the byte
/// ranges of each copy of the version within the URI.
#[derive(Debug)]
struct Read {
    package: String,
    name: String,
    version: String,
    spans: Vec<std::ops::Range<usize>>,
}

/// The package `uri` imports, or why it is skipped. `None` for an import
/// that is no package URI.
fn read(uri: &str) -> Option<Result<Read, &'static str>> {
    let rest = uri.strip_prefix("package://")?;
    Some((|| {
        if uri.contains('\\') {
            return Err(ESCAPED);
        }
        let base = &rest[..rest.find('#').unwrap_or(rest.len())];
        let (host, path) = base.split_once('/').ok_or(UNSUPPORTED_HOST)?;
        if !matches!(host, "github.com" | "pkg.pkl-lang.org") {
            return Err(UNSUPPORTED_HOST);
        }
        if path.contains("::") {
            return Err(CHECKSUM);
        }
        let (path_base, version) = path.rsplit_once('@').ok_or(UNSUPPORTED_PATH)?;
        if !pkl_semver(version) {
            return Err(VERSION);
        }
        // Byte offsets in `uri`: the path starts after `package://<host>/`.
        let path_at = "package://".len() + host.len() + 1;
        let version_at = path_at + path_base.len() + 1;
        let parts: Vec<&str> = path_base.split('/').collect();
        let name = parts.last().copied().unwrap_or_default();
        let mut spans = Vec::with_capacity(2);
        spans.push(version_at..version_at + version.len());
        let package = match (host, parts.as_slice()) {
            ("github.com", [owner, repo, "releases", "download", tag, _]) => {
                // Every start, overlapping ones included: `1.1.1.1.1` holds
                // `1.1.1` three times, and no single one is the version.
                let mut found = tag
                    .char_indices()
                    .filter(|(at, _)| tag[*at..].starts_with(version))
                    .map(|(at, _)| at);
                let (Some(at), None) = (found.next(), found.next()) else {
                    return Err(TAG_VERSION);
                };
                let tag_at = path_at + path_base.len() - name.len() - 1 - tag.len();
                spans.insert(0, tag_at + at..tag_at + at + version.len());
                let (prefix, suffix) = (&tag[..at], &tag[at + version.len()..]);
                format!(
                    "github.com/{owner}/{repo}/releases/download/{prefix}{VERSION_PLACEHOLDER}{suffix}/{name}"
                )
            }
            ("pkg.pkl-lang.org", ["github.com", _, _, _] | [_, _]) => {
                format!("pkg.pkl-lang.org/{path_base}")
            }
            _ => return Err(UNSUPPORTED_PATH),
        };
        if PklPackage::parse(&package).is_none() {
            return Err(UNSUPPORTED_PATH);
        }
        Ok(Read {
            package,
            name: name.into(),
            version: version.into(),
            spans,
        })
    })())
}

/// A skipped package URI as a report shows it: without its query, fragment
/// and checksum, and hidden when what is left holds a `:`, which a password
/// always follows (a port is hidden with it), or an `@` before the first
/// `/`, which ends a user. `name@version` puts an `@` in every package path,
/// so the generic URL redaction, which reads any `@` as a possible user,
/// would hide them all.
fn shown(uri: &str) -> String {
    let rest = uri.strip_prefix("package://").unwrap_or(uri);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let path = path.split("::").next().unwrap_or(path);
    if authority.contains([':', '@']) || path.contains(':') {
        "<unreadable URL>".into()
    } else {
        format!("package://{authority}/{path}")
    }
}

/// The package URIs `text` imports; one this manager cannot read is
/// recorded in `skipped` with the reason.
pub(super) fn extract(path: &str, text: &Lines<'_>, skipped: &mut Vec<Skipped>) -> Vec<Dependency> {
    let mut out = Vec::new();
    for (index, import) in patchula_config::pkl_imports(text).into_iter().enumerate() {
        let start = import.range.start;
        let found = match read(&import.target) {
            None => continue,
            Some(Ok(found)) => found,
            Some(Err(reason)) => {
                skipped.push(Skipped {
                    manager: Manager::Pkl,
                    file: path.into(),
                    line: Some(text.line_column(start).0),
                    value: shown(&import.target),
                    reason: reason.into(),
                });
                continue;
            }
        };
        let occurrences = found.spans.len();
        for (n, span) in found.spans.into_iter().enumerate() {
            let pointer = if occurrences == 1 {
                format!("/imports/{index}")
            } else {
                format!("/imports/{index}/{n}")
            };
            out.push(Dependency {
                cooldown: None,
                manager: Manager::Pkl,
                file: path.into(),
                location: Location::new(text, start + span.start, start + span.end, pointer),
                form: EditForm::Scalar(Syntax::InnerToken),
                name: found.name.clone(),
                package: found.package.clone(),
                kind: DependencyKind::Production,
                current: found.version.clone(),
                resolved: Some(found.version.clone()),
                digest: None,
                source: Source::Pkl,
                scheme: Scheme::Semver,
                extract: None,
                included_by: None,
                registry: None,
            });
        }
    }
    out
}

pub(crate) static DESCRIPTOR: Descriptor = Descriptor;

pub(crate) struct Descriptor;

impl crate::Ecosystem for Descriptor {
    /// A package URI names one exact version, so an update writes the
    /// release's version as the catalog lists it, never a range: `widen`
    /// would otherwise write `2.9.0 || 2.13.0` into the URI.
    fn proposing(&self, _dep: &Dependency) -> Proposing {
        Proposing {
            written: Written::Tag,
            ..Proposing::BASE
        }
    }
    /// Every Pkl module but the repository's own config, whose imports
    /// config loading already refuses unless they are local modules.
    fn selected(&self, _config: &Config, path: &str) -> bool {
        path != patchula_config::CONFIG_PATH && crate::roles::leaf(path).ends_with(".pkl")
    }
    fn prepare<'a>(
        &self,
        _scanner: &'a Scanner,
        _files: &'a Files,
        _shared: &'a Shared<'a>,
    ) -> Result<Prepared<'a>, String> {
        Ok(Prepared {
            extract: Some(Box::new(|path, text, out| {
                let dependencies = extract(path, text, &mut out.skipped);
                out.dependencies.extend(dependencies);
                Ok(())
            })),
            finish: None,
        })
    }
}
