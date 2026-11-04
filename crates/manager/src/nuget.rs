//! NuGet: `PackageReference`/`PackageVersion` items in MSBuild files
//! (projects, `Directory.Packages.props`, `Directory.Build.props`), local
//! tools in `.config/dotnet-tools.json` and the SDK in `global.json`.
//!
//! NuGet's lockfile does not record which source a package came from, and
//! with several sources NuGet takes a package from whichever answers. So a
//! package comes from nuget.org only when the `nuget.config` files that
//! apply name no other source, or when their `packageSourceMapping`, NuGet's
//! own statement of which source serves which packages, maps it there
//! alone. Anything else fails its lookup, through `crate::origins`.
use crate::origins::{Named, Origin, Origins, Public};
use crate::*;
use jsonc_parser::common::Ranged;
use regex::Regex;
use std::sync::LazyLock;

fn package(
    path: &str,
    text: &str,
    name: &str,
    start: usize,
    version: &str,
    pointer: String,
    kind: DependencyKind,
) -> Dependency {
    Dependency {
        manager: Manager::Nuget,
        file: path.into(),
        location: Location::new(text, start, start + version.len(), pointer),
        name: name.into(),
        package: name.into(),
        kind,
        current: version.into(),
        resolved: (!version.starts_with(['[', '('])).then(|| version.into()),
        digest: None,
        source: Source::Nuget,
        scheme: Scheme::Nuget,
        extract: None,
        included_by: None,
        registry: None,
    }
}
/// MSBuild properties, floating versions and empty values are not pins.
fn pinned(version: &str) -> bool {
    !version.is_empty() && !version.contains(['$', '*', ' '])
}

fn msbuild(path: &str, text: &str) -> Vec<Dependency> {
    static ITEM: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)<(PackageReference|PackageVersion|GlobalPackageReference)\b([^>]*?)(/?)>")
            .expect("item pattern")
    });
    static ATTRIBUTE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"([A-Za-z]+)\s*=\s*"([^"]*)""#).expect("attribute pattern"));
    static CHILD: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)<Version>\s*([^<]*?)\s*</Version>").expect("child pattern")
    });
    // Items inside comments are not part of the build.
    let mut comments = Vec::new();
    let mut offset = 0;
    while let Some(start) = text[offset..].find("<!--") {
        let start = offset + start;
        let end = text[start..]
            .find("-->")
            .map_or(text.len(), |e| start + e + 3);
        comments.push(start..end);
        offset = end;
    }
    let commented = |at: usize| comments.iter().any(|r| r.contains(&at));
    let mut out = Vec::new();
    for item in ITEM.captures_iter(text) {
        let whole = item.get(0).unwrap();
        if commented(whole.start()) {
            continue;
        }
        let attributes = item.get(2).unwrap();
        let mut name = None;
        let mut version = None;
        for a in ATTRIBUTE.captures_iter(attributes.as_str()) {
            let value = a.get(2).unwrap();
            match &a[1] {
                "Include" | "Update" => name = Some(value.as_str().to_string()),
                "Version" | "VersionOverride" => {
                    version = Some((
                        attributes.start() + value.start(),
                        value.as_str().to_string(),
                    ))
                }
                _ => {}
            }
        }
        let Some(name) = name.filter(|n| !n.contains(['$', ';', '*'])) else {
            continue;
        };
        if version.is_none() && item[3].is_empty() {
            // <PackageReference Include="X"><Version>1.0</Version></PackageReference>
            let close = format!("</{}>", &item[1]);
            if let Some(end) = text[whole.end()..].find(&close) {
                let body = &text[whole.end()..whole.end() + end];
                if let Some(c) = CHILD.captures(body) {
                    let v = c.get(1).unwrap();
                    version = Some((whole.end() + v.start(), v.as_str().to_string()));
                }
            }
        }
        let Some((start, version)) = version.filter(|(_, v)| pinned(v)) else {
            continue;
        };
        let kind = if &item[1] == "GlobalPackageReference" {
            DependencyKind::Development
        } else {
            DependencyKind::Production
        };
        out.push(package(
            path,
            text,
            &name,
            start,
            &version,
            format!("/{}/{name}", &item[1]),
            kind,
        ));
    }
    out
}

fn jsonc(text: &str) -> Result<jsonc_parser::ast::Value<'_>, String> {
    let options = jsonc_parser::ParseOptions {
        allow_comments: true,
        allow_trailing_commas: true,
        ..Default::default()
    };
    jsonc_parser::parse_to_ast(text, &Default::default(), &options)
        .map_err(|e| e.to_string())?
        .value
        .ok_or_else(|| "empty JSON".into())
}
/// The inner span of a JSON string without escapes.
fn inner(text: &str, value: &jsonc_parser::ast::StringLit) -> Option<usize> {
    let (start, end) = (value.start(), value.end());
    (end - start == value.value.len() + 2 && !text[start..end].contains('\\')).then_some(start + 1)
}

fn tools(path: &str, text: &str) -> Result<Vec<Dependency>, String> {
    let root = jsonc(text)?;
    let mut out = Vec::new();
    if let Some(tools) = root.as_object().and_then(|o| o.get_object("tools")) {
        for prop in &tools.properties {
            let name = prop.name.as_str();
            if let Some(version) = prop.value.as_object().and_then(|o| o.get_string("version"))
                && let Some(start) = inner(text, version)
                && pinned(&version.value)
            {
                out.push(package(
                    path,
                    text,
                    name,
                    start,
                    &version.value,
                    format!("/tools/{name}/version"),
                    DependencyKind::Tool,
                ));
            }
        }
    }
    Ok(out)
}

fn sdk(path: &str, text: &str) -> Result<Vec<Dependency>, String> {
    let root = jsonc(text)?;
    let Some(version) = root
        .as_object()
        .and_then(|o| o.get_object("sdk"))
        .and_then(|s| s.get_string("version"))
    else {
        return Ok(Vec::new());
    };
    let Some(start) = inner(text, version).filter(|_| {
        version.value.split('.').count() == 3
            && version
                .value
                .split('.')
                .all(|p| p.bytes().all(|b| b.is_ascii_digit()))
    }) else {
        return Ok(Vec::new());
    };
    // Published SDK releases only: tags can precede a release, and SDK
    // versions use feature bands (patch 100 and up), unlike runtime tags.
    let (source, package, extract) = (
        Source::GithubReleases,
        "dotnet/sdk".to_string(),
        Some(r"^v(?<version>\d+\.\d+\.[1-9]\d{2}(?:-.+)?)$".to_string()),
    );
    Ok(vec![Dependency {
        manager: Manager::Nuget,
        file: path.into(),
        location: Location::new(
            text,
            start,
            start + version.value.len(),
            "/sdk/version".into(),
        ),
        name: "dotnet-sdk".into(),
        package,
        kind: DependencyKind::Toolchain,
        current: version.value.to_string(),
        resolved: Some(version.value.to_string()),
        digest: None,
        source,
        scheme: Scheme::Loose,
        extract,
        included_by: None,
        registry: None,
    }])
}

pub(super) fn selected(path: &str) -> bool {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    leaf.ends_with(".csproj")
        || leaf.ends_with(".fsproj")
        || leaf.ends_with(".vbproj")
        || matches!(
            leaf,
            "Directory.Packages.props" | "Directory.Build.props" | "global.json"
        )
        || path.ends_with(".config/dotnet-tools.json")
}

pub(super) fn extract(path: &str, text: &str) -> Result<Vec<Dependency>, String> {
    if path.ends_with("dotnet-tools.json") {
        tools(path, text)
    } else if path.ends_with("global.json") {
        sdk(path, text)
    } else {
        Ok(msbuild(path, text))
    }
}

/// nuget.org, for `crate::origins`.
const NUGET_ORG: Public = Public {
    source: Source::Nuget,
    label: "https://api.nuget.org/v3/index.json",
    is_public: |url| {
        crate::web_host(url).is_some_and(|host| {
            matches!(
                host.as_str(),
                "api.nuget.org" | "www.nuget.org" | "nuget.org"
            )
        })
    },
};

/// The names NuGet reads a config file under, in each folder.
const CONFIG_NAMES: [&str; 3] = ["nuget.config", "NuGet.config", "NuGet.Config"];

/// Whether `path` could be a NuGet config file, under any spelling.
pub(crate) fn config_file(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|leaf| leaf.eq_ignore_ascii_case("nuget.config"))
}

/// MSBuild properties that set restore sources or another config file, which
/// override what the repository's `nuget.config` files say.
const RESTORE_OVERRIDES: [&str; 4] = [
    "restoresources",
    "restoreadditionalprojectsources",
    "restoreconfigfile",
    "restorefallbackfolders",
];

/// Whether `path` is an MSBuild file or response file that could set a
/// restore property.
pub(crate) fn msbuild_file(path: &str) -> bool {
    let leaf = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    [".props", ".targets", ".rsp", "proj"]
        .iter()
        .any(|suffix| leaf.ends_with(suffix))
}

/// One change a config file makes to a collection, in file order.
#[derive(Clone, Debug)]
enum Change {
    Clear,
    Add(String, String),
    Remove(String),
}

/// What one `nuget.config` says about package sources.
#[derive(Clone, Debug, Default)]
struct Config {
    sources: Vec<Change>,
    /// `Add(key, "true" | "false")`.
    disabled: Vec<Change>,
    /// `Add(source key, pattern)`.
    mapping: Vec<Change>,
}

/// Sections of a `nuget.config` that do not say where packages come from.
/// `fallbackPackageFolders`, which can supply packages, is not among them.
const OTHER_SECTIONS: [&str; 10] = [
    "packageSourceCredentials",
    "apikeys",
    "packageRestore",
    "trustedSigners",
    "auditSources",
    "solution",
    "bindingRedirects",
    "packageManagement",
    "activePackageSource",
    "clientCertificates",
];
/// `config` keys, none of which change where packages come from.
const CONFIG_KEYS: [&str; 10] = [
    "dependencyVersion",
    "globalPackagesFolder",
    "repositoryPath",
    "defaultPushSource",
    "http_proxy",
    "http_proxy.user",
    "http_proxy.password",
    "no_proxy",
    "maxHttpRequestsPerSource",
    "signatureValidationMode",
];

/// An element's child elements, ignoring comments and blank text; an error
/// for any other content.
fn elements<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
) -> Result<Vec<roxmltree::Node<'a, 'input>>, ()> {
    node.children()
        .filter(|child| {
            !child.is_comment()
                && !(child.is_text() && child.text().is_some_and(|t| t.trim().is_empty()))
        })
        .map(|child| child.is_element().then_some(child).ok_or(()))
        .collect()
}

fn attribute(node: roxmltree::Node<'_, '_>, name: &str) -> Result<String, ()> {
    node.attribute(name).map(str::to_owned).ok_or(())
}

/// A collection section's `clear`, `add` and `remove` children, with the
/// attribute `add` keeps its value in.
fn changes(node: roxmltree::Node<'_, '_>, value: &str) -> Result<Vec<Change>, ()> {
    elements(node)?
        .into_iter()
        .map(|child| match child.tag_name().name() {
            "clear" => Ok(Change::Clear),
            "add" => Ok(Change::Add(
                attribute(child, "key")?,
                attribute(child, value)?,
            )),
            "remove" => Ok(Change::Remove(attribute(child, "key")?)),
            _ => Err(()),
        })
        .collect()
}

/// Read a `nuget.config` strictly: any element the Count does not know is
/// an error, so its packages' lookups fail.
fn config(text: &str) -> Result<Config, ()> {
    let document = roxmltree::Document::parse(text).map_err(|_| ())?;
    let root = document.root_element();
    if root.tag_name().name() != "configuration" {
        return Err(());
    }
    let mut out = Config::default();
    for section in elements(root)? {
        match section.tag_name().name() {
            "packageSources" => out.sources.extend(changes(section, "value")?),
            "disabledPackageSources" => out.disabled.extend(changes(section, "value")?),
            "packageSourceMapping" => {
                for child in elements(section)? {
                    match child.tag_name().name() {
                        "clear" => out.mapping.push(Change::Clear),
                        "packageSource" => {
                            let key = attribute(child, "key")?;
                            for package in elements(child)? {
                                if package.tag_name().name() != "package" {
                                    return Err(());
                                }
                                out.mapping
                                    .push(Change::Add(key.clone(), attribute(package, "pattern")?));
                            }
                        }
                        _ => return Err(()),
                    }
                }
            }
            "config" => {
                for change in changes(section, "value")? {
                    if let Change::Add(key, _) | Change::Remove(key) = &change
                        && !CONFIG_KEYS.contains(&key.as_str())
                    {
                        return Err(());
                    }
                }
            }
            name if OTHER_SECTIONS.contains(&name) => {}
            _ => return Err(()),
        }
    }
    Ok(out)
}

/// The package source configuration of a repository, read once: each
/// folder's `nuget.config`, and whether an MSBuild file overrides them.
pub(crate) struct Feeds {
    /// Each folder with a config, as a path prefix (`""` for the root), and
    /// what it says, or why the Count cannot read it.
    configs: BTreeMap<String, Result<Config, String>>,
    /// An MSBuild file that sets restore sources itself.
    overridden: Option<String>,
}

impl Feeds {
    pub(crate) fn new(files: &Files) -> Self {
        let mut configs = BTreeMap::<String, Result<Config, String>>::new();
        for (path, text) in files.iter().filter(|(path, _)| config_file(path)) {
            let (dir, leaf) = path
                .rsplit_once('/')
                .map_or(("", path.as_str()), |(d, l)| (d, l));
            let dir = if dir.is_empty() {
                String::new()
            } else {
                format!("{dir}/")
            };
            let read = if !CONFIG_NAMES.contains(&leaf) {
                Err(format!("{path}, a name NuGet reads on some systems only"))
            } else if configs.contains_key(&dir) {
                Err(format!(
                    "several NuGet config files in {}",
                    if dir.is_empty() { "the root" } else { &dir }
                ))
            } else {
                config(text).map_err(|()| format!("{path}, which the Count cannot read"))
            };
            match configs.get(&dir) {
                Some(Err(_)) => {}
                _ => {
                    configs.insert(dir, read);
                }
            }
        }
        let overridden = files
            .iter()
            .find(|(path, text)| {
                msbuild_file(path) && {
                    let text = text.to_ascii_lowercase();
                    RESTORE_OVERRIDES
                        .iter()
                        .any(|property| text.contains(property))
                }
            })
            .map(|(path, _)| format!("restore sources set in {path}"));
        Self {
            configs,
            overridden,
        }
    }

    /// Where `package` comes from for a dependency declared in `file`: for
    /// a project there, and for any project in a folder below it with a
    /// config of its own, since central and shared MSBuild files apply to
    /// projects below them.
    fn origin(&self, file: &str, package: &str) -> Origin {
        if let Some(reason) = &self.overridden {
            return Origin::Index(Named::Unusable(reason.clone()));
        }
        let dir = &file[..file.rfind('/').map_or(0, |i| i + 1)];
        let below = self
            .configs
            .keys()
            .filter(|config| config.starts_with(dir) && config.len() > dir.len());
        let origins = std::iter::once(dir)
            .chain(below.map(String::as_str))
            .map(|dir| self.origin_in(dir, package));
        Origin::of(origins).expect("one folder at least")
    }

    /// Where `package` comes from for a restore in `dir`, applying each
    /// config from the root down, after the nuget.org source NuGet's default
    /// user config adds.
    fn origin_in(&self, dir: &str, package: &str) -> Origin {
        let unusable = |reason: &str| Origin::Index(Named::Unusable(reason.to_owned()));
        let mut sources: Vec<(String, Named)> = vec![("nuget.org".into(), Named::Public)];
        let mut disabled = BTreeMap::<String, bool>::new();
        let mut mapping: Vec<(String, String)> = Vec::new();
        let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
        for (folder, config) in &self.configs {
            if !dir.starts_with(folder.as_str()) {
                continue;
            }
            let config = match config {
                Ok(config) => config,
                Err(reason) => return unusable(reason),
            };
            for change in &config.sources {
                match change {
                    Change::Clear => sources.clear(),
                    Change::Add(key, value) => {
                        sources.retain(|(k, _)| !same(k, key));
                        sources.push((key.clone(), Named::parse(value, &NUGET_ORG)));
                    }
                    Change::Remove(key) => sources.retain(|(k, _)| !same(k, key)),
                }
            }
            for change in &config.disabled {
                match change {
                    Change::Clear => disabled.clear(),
                    Change::Add(key, value) => {
                        let off = match value.to_ascii_lowercase().as_str() {
                            "true" => true,
                            "false" => false,
                            _ => {
                                return unusable("a disabled package source the Count cannot read");
                            }
                        };
                        disabled.insert(key.to_ascii_lowercase(), off);
                    }
                    Change::Remove(key) => {
                        disabled.remove(&key.to_ascii_lowercase());
                    }
                }
            }
            for change in &config.mapping {
                match change {
                    Change::Clear => mapping.clear(),
                    Change::Add(key, pattern) => mapping.push((key.clone(), pattern.clone())),
                    Change::Remove(_) => {
                        return unusable("a package source mapping the Count cannot read");
                    }
                }
            }
        }
        let active: Vec<(String, Named)> = sources
            .into_iter()
            .filter(|(key, _)| {
                !disabled
                    .get(&key.to_ascii_lowercase())
                    .copied()
                    .unwrap_or(false)
            })
            .collect();
        if active.is_empty() {
            return unusable("no package source in nuget.config");
        }
        if active.iter().all(|(_, named)| *named == Named::Public) {
            return Origin::Index(Named::Public);
        }
        if mapping.is_empty() {
            return unusable(
                "several package sources and no packageSourceMapping in nuget.config to say which serves it",
            );
        }
        // The most specific pattern: an exact ID, else the longest prefix.
        let specificity = |pattern: &str| match pattern.strip_suffix('*') {
            Some(prefix) => package
                .get(..prefix.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
                .then_some(prefix.len()),
            None => same(pattern, package).then_some(usize::MAX),
        };
        let Some(best) = mapping.iter().filter_map(|(_, p)| specificity(p)).max() else {
            return unusable("no packageSourceMapping pattern in nuget.config for it");
        };
        let keys: Vec<&str> = mapping
            .iter()
            .filter(|(_, p)| specificity(p) == Some(best))
            .map(|(key, _)| key.as_str())
            .collect();
        let candidates = active
            .into_iter()
            .filter(|(key, _)| keys.iter().any(|k| same(k, key)))
            .map(|(_, named)| Origin::Index(named));
        Origin::of(candidates).unwrap_or_else(|| {
            unusable("a packageSourceMapping in nuget.config to no package source")
        })
    }

    /// Give each NuGet package among `deps` its registry.
    pub(crate) fn assign(&self, deps: &mut Vec<Dependency>) {
        let mut origins = Origins::default();
        for dep in deps.iter().filter(|d| d.source == Source::Nuget) {
            let origin = self.origin(&dep.file, &dep.package);
            origins.declared.insert(dep.package.clone(), origin);
        }
        // Every NuGet package has an index origin, so none is dropped.
        let dropped = origins.assign(deps, &NUGET_ORG, |dep| dep.package.clone());
        debug_assert!(dropped.is_empty());
    }
}
