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
use std::collections::BTreeSet;
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

/// The start of MSBuild property names that set restore sources, fallback
/// folders or another config file, which override what the repository's
/// `nuget.config` files say.
const RESTORE_OVERRIDES: [&str; 4] = [
    "restoresources",
    "restoreadditionalproject",
    "restoreconfigfile",
    "restorefallbackfolders",
];

/// Whether `path` is an MSBuild file or response file that could set a
/// restore property.
pub(crate) fn msbuild_file(path: &str) -> bool {
    let leaf = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    [
        ".props",
        ".targets",
        ".rsp",
        "proj",
        ".projitems",
        ".user",
        ".msbuild",
    ]
    .iter()
    .any(|suffix| leaf.ends_with(suffix))
}

/// `text` with XML character references (`&#83;`, `&#x53;`) decoded, so a
/// property name written with them is still found.
fn decoded(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("&#") {
        out.push_str(&rest[..at]);
        let reference = &rest[at + 2..];
        let decoded = reference.split_once(';').and_then(|(code, _)| {
            let value = match code.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => code.parse().ok()?,
            };
            Some((char::from_u32(value)?, code.len() + 1))
        });
        match decoded {
            Some((c, width)) => {
                out.push(c);
                rest = &reference[width..];
            }
            None => {
                out.push_str("&#");
                rest = reference;
            }
        }
    }
    out.push_str(rest);
    out
}

/// What one `nuget.config` says about package sources.
#[derive(Clone, Debug, Default)]
struct Config {
    /// Every source it adds, as `(key, value)`.
    sources: Vec<(String, String)>,
    /// Its `packageSourceMapping`, as `(source key, pattern)`, when it has one.
    mapping: Option<Vec<(String, String)>>,
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

/// A pattern NuGet and the Count read alike: a package ID, or a prefix
/// ending in its only `*`, in plain ASCII without spaces.
fn plain_pattern(pattern: &str) -> bool {
    !pattern.is_empty()
        && pattern
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b'"' | b'\'' | b'<' | b'>'))
        && pattern.find('*').is_none_or(|at| at == pattern.len() - 1)
}

/// Read a `nuget.config` strictly: any element the Count does not know, and
/// anything NuGet would read differently from the Count, is an error, so
/// the lookups it could affect fail.
fn config(text: &str) -> Result<Config, ()> {
    let document = roxmltree::Document::parse(text).map_err(|_| ())?;
    let root = document.root_element();
    if root.tag_name().name() != "configuration" {
        return Err(());
    }
    let mut out = Config::default();
    for section in elements(root)? {
        match section.tag_name().name() {
            // Sources a closer config clears or disables still count, so
            // `clear` and `disabledPackageSources` change nothing here.
            "packageSources" => {
                for child in elements(section)? {
                    match child.tag_name().name() {
                        "clear" => {}
                        "add" => out
                            .sources
                            .push((attribute(child, "key")?, attribute(child, "value")?)),
                        _ => return Err(()),
                    }
                }
            }
            "disabledPackageSources" => {
                for child in elements(section)? {
                    if !matches!(child.tag_name().name(), "clear" | "add") {
                        return Err(());
                    }
                }
            }
            // NuGet drops a repeated section or source key in one file, and
            // a `clear` after mappings in it; none is read here.
            "packageSourceMapping" => {
                if out.mapping.is_some() {
                    return Err(());
                }
                let mut mapping = Vec::new();
                let mut keys = BTreeSet::new();
                for (index, child) in elements(section)?.into_iter().enumerate() {
                    match child.tag_name().name() {
                        "clear" if index == 0 => {}
                        "packageSource" => {
                            let key = attribute(child, "key")?;
                            if !keys.insert(key.to_ascii_lowercase()) {
                                return Err(());
                            }
                            for package in elements(child)? {
                                let pattern = attribute(package, "pattern")?;
                                if package.tag_name().name() != "package"
                                    || !plain_pattern(&pattern)
                                {
                                    return Err(());
                                }
                                mapping.push((key.clone(), pattern));
                            }
                        }
                        _ => return Err(()),
                    }
                }
                out.mapping = Some(mapping);
            }
            "config" => {
                for child in elements(section)? {
                    let key = attribute(child, "key")?;
                    if child.tag_name().name() != "add" || !CONFIG_KEYS.contains(&key.as_str()) {
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
    /// An MSBuild file that sets restore sources itself, or cannot be read.
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
                Err(format!("several NuGet config files beside {path}"))
            } else {
                config(text).map_err(|()| format!("{path}, which the Count cannot read"))
            };
            if !matches!(configs.get(&dir), Some(Err(_))) {
                configs.insert(dir, read);
            }
        }
        let overridden = files
            .iter()
            .find(|(path, text)| {
                msbuild_file(path)
                    && (text.as_str() == crate::UNREADABLE_SETTINGS || {
                        let text = decoded(text).to_ascii_lowercase();
                        RESTORE_OVERRIDES
                            .iter()
                            .any(|property| text.contains(property))
                    })
            })
            .map(|(path, _)| format!("restore sources {path} may set"));
        Self {
            configs,
            overridden,
        }
    }

    /// Where `package` comes from for a dependency declared in `file`: for
    /// a restore there, and in any folder below it with a config of its own,
    /// since central and shared MSBuild files apply to projects below them
    /// and `dotnet tool restore` reads its manifest from any folder below.
    fn origin(&self, file: &str, package: &str) -> Origin {
        if let Some(reason) = &self.overridden {
            return Origin::Index(Named::Unusable(reason.clone()));
        }
        let mut dir = &file[..file.rfind('/').map_or(0, |i| i + 1)];
        if let Some(project) = dir.strip_suffix(".config/") {
            dir = project;
        }
        let below = self
            .configs
            .keys()
            .filter(|config| config.starts_with(dir) && config.len() > dir.len());
        let origins = std::iter::once(dir)
            .chain(below.map(String::as_str))
            .map(|dir| self.origin_in(dir, package));
        Origin::of(origins).expect("one folder at least")
    }

    /// Where `package` comes from for a restore in `dir`, from the configs
    /// in it and above it. Every source they add counts, with the nuget.org
    /// source NuGet's default user config adds, so a source NuGet would
    /// drop only ever makes the lookup fail.
    fn origin_in(&self, dir: &str, package: &str) -> Origin {
        let unusable = |reason: &str| Origin::Index(Named::Unusable(reason.to_owned()));
        let mut chain = Vec::new();
        for (folder, config) in &self.configs {
            if dir.starts_with(folder.as_str()) {
                match config {
                    Ok(config) => chain.push(config),
                    Err(reason) => return unusable(reason),
                }
            }
        }
        let sources: Vec<(&str, Named)> = std::iter::once(("nuget.org", Named::Public))
            .chain(chain.iter().flat_map(|config| {
                config
                    .sources
                    .iter()
                    .map(|(key, value)| (key.as_str(), Named::parse(value, &NUGET_ORG)))
            }))
            .collect();
        if sources.iter().all(|(_, named)| *named == Named::Public) {
            return Origin::Index(Named::Public);
        }
        let mappings: Vec<&Vec<(String, String)>> = chain
            .iter()
            .filter_map(|config| config.mapping.as_ref())
            .collect();
        if mappings.is_empty() {
            return unusable(
                "several package sources and no packageSourceMapping in nuget.config to say which serves it",
            );
        }
        let patterns: Vec<&(String, String)> = mappings.iter().flat_map(|m| m.iter()).collect();
        // NuGet applies an exact ID's sources to a prefix of the same text
        // too; the Count does not, so such a pair fails.
        let exact: BTreeSet<String> = patterns
            .iter()
            .filter(|(_, p)| !p.ends_with('*'))
            .map(|(_, p)| p.to_ascii_lowercase())
            .collect();
        if patterns.iter().any(|(_, p)| {
            p.strip_suffix('*')
                .is_some_and(|prefix| exact.contains(&prefix.to_ascii_lowercase()))
        }) {
            return unusable("packageSourceMapping patterns NuGet reads differently");
        }
        let specificity = |pattern: &str| match pattern.strip_suffix('*') {
            Some(prefix) => package
                .get(..prefix.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
                .then_some(prefix.len()),
            None => pattern.eq_ignore_ascii_case(package).then_some(usize::MAX),
        };
        // One config's mapping says which pattern wins. Mappings from
        // several configs merge in ways the Count does not follow, so every
        // pattern that matches counts.
        let keys: Vec<&str> = if mappings.len() == 1 {
            let best = patterns.iter().filter_map(|(_, p)| specificity(p)).max();
            patterns
                .iter()
                .filter(|(_, p)| best.is_some() && specificity(p) == best)
                .map(|(key, _)| key.as_str())
                .collect()
        } else {
            patterns
                .iter()
                .filter(|(_, p)| specificity(p).is_some())
                .map(|(key, _)| key.as_str())
                .collect()
        };
        let candidates = sources
            .into_iter()
            .filter(|(key, _)| keys.iter().any(|k| k.eq_ignore_ascii_case(key)))
            .map(|(_, named)| Origin::Index(named));
        Origin::of(candidates).unwrap_or_else(|| {
            unusable("no package source in nuget.config's packageSourceMapping for it")
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
