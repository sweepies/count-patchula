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

/// Whether `path` is a project file by extension: any ending in `proj`,
/// compared case-insensitively — `.csproj`, `.CSPROJ`, `.vcxproj`,
/// `.sqlproj`, `.proj`.
fn project_file(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .rsplit_once('.')
        .is_some_and(|(_, ext)| ext.to_ascii_lowercase().ends_with("proj"))
}

pub(super) fn selected(path: &str) -> bool {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    project_file(path)
        || leaf.ends_with(".sln")
        || leaf.ends_with(".slnx")
        || matches!(
            leaf,
            "Directory.Packages.props"
                | "Directory.Build.props"
                | "global.json"
                | "packages.lock.json"
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
pub(crate) const NUGET_ORG: Public = Public {
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
/// folders, another config file or another folder to read configs from,
/// which override what the repository's `nuget.config` files say.
pub(crate) const RESTORE_OVERRIDES: [&str; 6] = [
    "restoresources",
    "restoreadditionalproject",
    "restoreconfigfile",
    "restorefallbackfolders",
    "restorerootconfigdirectory",
    "restoresolutiondirectory",
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
        ".pubxml",
    ]
    .iter()
    .any(|suffix| leaf.ends_with(suffix))
}

/// `text` with XML character references (`&#83;`, `&#x53;`) decoded, so a
/// property name written with them is still found.
pub(crate) fn decoded(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("&#") {
        out.push_str(&rest[..at]);
        let reference = &rest[at + 2..];
        // Only the digits after `&#` are read, however many leading zeros
        // they have; a run of `&#` without them stays linear.
        let bytes = reference.as_bytes();
        let hex = matches!(bytes.first(), Some(b'x' | b'X'));
        let start = usize::from(hex);
        let digits = bytes[start..]
            .iter()
            .take_while(|b| {
                if hex {
                    b.is_ascii_hexdigit()
                } else {
                    b.is_ascii_digit()
                }
            })
            .count();
        let end = start + digits;
        let decoded = (digits > 0 && bytes.get(end) == Some(&b';'))
            .then(|| {
                let digits = reference[start..end].trim_start_matches('0');
                let digits = if digits.is_empty() { "0" } else { digits };
                u32::from_str_radix(digits, if hex { 16 } else { 10 }).ok()
            })
            .flatten()
            .and_then(char::from_u32)
            .map(|c| (c, end + 1));
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
    mapping: Option<Mapping>,
}

/// One config's `packageSourceMapping`, as `(source key, pattern)`.
type Mapping = Vec<(String, String)>;

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

/// A source or mapping key NuGet and the Count compare alike: NuGet trims
/// mapping keys but not source keys, and folds case beyond ASCII.
fn plain_key(key: &str) -> bool {
    !key.is_empty() && key.trim() == key && key.is_ascii()
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
                        "add" => {
                            let key = attribute(child, "key")?;
                            if !plain_key(&key) {
                                return Err(());
                            }
                            // NuGet keeps quotes the Count would trim.
                            let value = attribute(child, "value")?;
                            if value.trim() != value || value.contains(['"', '\'']) {
                                return Err(());
                            }
                            out.sources.push((key, value));
                        }
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
                let mut mapping = Mapping::default();
                let mut keys = BTreeSet::new();
                for (index, child) in elements(section)?.into_iter().enumerate() {
                    match child.tag_name().name() {
                        // Only a mapping one config alone holds is read, so
                        // a leading `clear` drops nothing.
                        "clear" if index == 0 => {}
                        "packageSource" => {
                            let key = attribute(child, "key")?;
                            if !plain_key(&key) || !keys.insert(key.to_ascii_lowercase()) {
                                return Err(());
                            }
                            // A source with no patterns, which NuGet may
                            // read as one that serves nothing, is not read.
                            let packages = elements(child)?;
                            if packages.is_empty() {
                                return Err(());
                            }
                            for package in packages {
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

/// The package source configuration of a repository, read once: what a
/// restore in each folder with a config sees, and whether an MSBuild file
/// overrides the configs.
pub(crate) struct Feeds<'a> {
    /// For each folder with a config, as a path prefix (`""` for the root),
    /// the configs from the root down to it, or why the Count cannot read
    /// one of them.
    chains: BTreeMap<String, Result<Chain, String>>,
    /// An MSBuild file that sets restore sources itself, or cannot be read.
    overridden: Option<String>,
    /// The files the scan read, for the central-package-management checks.
    files: &'a Files,
    /// Project files central package management can govern.
    projects: Vec<String>,
    /// Every `Directory.Packages.props`, for the "a closer one" check.
    central: Vec<String>,
}

/// What the configs from the root down to a folder say. Every source any of
/// them adds counts, with the nuget.org source NuGet's default user config
/// adds, so a source NuGet would drop only ever makes a lookup fail.
#[derive(Clone, Debug)]
struct Chain {
    sources: Vec<(String, Named)>,
    mappings: Vec<Mapping>,
}

impl Default for Chain {
    fn default() -> Self {
        Self {
            sources: vec![("nuget.org".into(), Named::Public)],
            mappings: Vec::new(),
        }
    }
}

impl<'a> Feeds<'a> {
    pub(crate) fn new(files: &'a Files) -> Self {
        let mut configs = BTreeMap::<String, Result<Config, String>>::new();
        for (path, text) in files.iter().filter(|(path, _)| config_file(path)) {
            let whole = path.as_str();
            let (dir, leaf) = whole.rsplit_once('/').unwrap_or(("", whole));
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
        // Folders sort after the folders above them, so each chain extends
        // the nearest one above.
        let mut chains = BTreeMap::<String, Result<Chain, String>>::new();
        for (dir, config) in &configs {
            let above = Self::nearest(&chains, dir.trim_end_matches('/'));
            let chain = match (above, config) {
                (Err(reason), _) => Err(reason),
                (_, Err(reason)) => Err(reason.clone()),
                (Ok(above), Ok(config)) => {
                    let mut chain = above.clone();
                    chain.sources.extend(
                        config
                            .sources
                            .iter()
                            .map(|(key, value)| (key.clone(), Named::parse(value, &NUGET_ORG))),
                    );
                    chain.mappings.extend(config.mapping.clone());
                    Ok(chain)
                }
            };
            chains.insert(dir.clone(), chain);
        }
        let overridden = files
            .iter()
            .find(|(path, text)| {
                msbuild_file(path)
                    && (crate::unreadable_text(text.as_str()) || {
                        let text = decoded(text).to_ascii_lowercase();
                        RESTORE_OVERRIDES
                            .iter()
                            .any(|property| text.contains(property))
                    })
            })
            .map(|(path, _)| format!("restore sources {path} may set"));
        let mut projects = Vec::new();
        let mut central = Vec::new();
        for path in files.keys() {
            let leaf = path.rsplit('/').next().unwrap_or(path);
            if project_file(path) {
                projects.push(path.clone());
            } else if leaf == "Directory.Packages.props" {
                central.push(path.clone());
            }
        }
        Self {
            chains,
            overridden,
            files,
            projects,
            central,
        }
    }

    /// Whether some project at or below `config_dir` is governed by the
    /// `Directory.Packages.props` in `props_dir`, or a `.sln`/`.slnx` at
    /// or below `config_dir` names one it governs anywhere — NuGet may
    /// read a solution's folder config for it.
    fn governs(&self, props_dir: &str, config_dir: &str) -> bool {
        self.projects.iter().any(|project| {
            let pdir = &project[..project.rfind('/').map_or(0, |i| i + 1)];
            pdir.starts_with(config_dir) && self.governs_project(props_dir, project)
        }) || self.files.iter().any(|(path, text)| {
            let leaf = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
            (leaf.ends_with(".sln") || leaf.ends_with(".slnx"))
                && path.starts_with(config_dir)
                && (crate::unreadable_text(text)
                    || solution_projects(text).any(|reference| {
                        match resolve_path(
                            &path[..path.rfind('/').map_or(0, |i| i + 1)],
                            &reference,
                        ) {
                            Some(target) if self.projects.contains(&target) => {
                                self.governs_project(props_dir, &target)
                            }
                            // A project the Count cannot place may be
                            // governed.
                            _ => true,
                        }
                    }))
        })
    }

    /// Whether the `Directory.Packages.props` in `props_dir` governs
    /// `project`: the project sits below it, below no closer one that
    /// does not itself import — a closer one with an `Import`, the usual
    /// `GetPathOfFileAbove`, passes its governance down — and nothing
    /// opts the project out. A condition, a variable, an import that may
    /// override or an unreadable file counts (fail closed).
    fn governs_project(&self, props_dir: &str, project: &str) -> bool {
        let pdir = &project[..project.rfind('/').map_or(0, |i| i + 1)];
        pdir.starts_with(props_dir)
            && !self.central.iter().any(|props| {
                let dir = &props[..props.rfind('/').map_or(0, |i| i + 1)];
                dir.len() > props_dir.len() && pdir.starts_with(dir) && self.self_contained(props)
            })
            && !self.opted_out(project)
    }

    /// Whether the `Directory.Packages.props` at `path` stands alone, so
    /// it stops one above from governing the projects under it: it holds
    /// no `Import` element at all. One the Count cannot read may import.
    fn self_contained(&self, props: &str) -> bool {
        let Some(text) = self.files.get(props) else {
            return true;
        };
        if crate::unreadable_text(text) {
            return false;
        }
        let Ok(document) = roxmltree::Document::parse(text) else {
            return false;
        };
        !document
            .descendants()
            .any(|node| node.is_element() && node.tag_name().name().eq_ignore_ascii_case("import"))
    }

    /// Whether a literal `ImportDirectoryPackagesProps` or
    /// `ManagePackageVersionsCentrally` `false` opts `project` out of
    /// central package management, set by the project itself or, since
    /// MSBuild imports only the nearest, by the nearest
    /// `Directory.Build.props` above it.
    fn opted_out(&self, project: &str) -> bool {
        if let Some(verdict) = self.files.get(project).map(|text| opt_out(text))
            && let Some(opted) = verdict
        {
            return opted;
        }
        let mut dir = &project[..project.rfind('/').map_or(0, |i| i + 1)];
        loop {
            if let Some(text) = self.files.get(&format!("{dir}Directory.Build.props")) {
                return opt_out(text).unwrap_or(false);
            }
            if dir.is_empty() {
                return false;
            }
            let parent = dir.trim_end_matches('/');
            dir = &parent[..parent.rfind('/').map_or(0, |i| i + 1)];
        }
    }

    /// The chain of the nearest folder at or above `dir` (a path prefix, or
    /// a path without its last `/`) with a config; the default one when none
    /// has.
    fn nearest(
        chains: &BTreeMap<String, Result<Chain, String>>,
        dir: &str,
    ) -> Result<Chain, String> {
        let mut prefix = dir;
        loop {
            let key = if prefix.is_empty() {
                String::new()
            } else {
                format!("{}/", prefix.trim_end_matches('/'))
            };
            if let Some(chain) = chains.get(&key) {
                return chain.clone();
            }
            match prefix.trim_end_matches('/').rsplit_once('/') {
                Some((parent, _)) => prefix = parent,
                None if !prefix.is_empty() => prefix = "",
                None => return Ok(Chain::default()),
            }
        }
    }

    /// Where `package` comes from for a dependency declared in `file`: for
    /// a restore there, and in any folder below it with a config of its own,
    /// since `dotnet tool restore` reads its manifest from any folder below.
    /// A shared MSBuild file such as `Directory.Packages.props` can be
    /// imported from anywhere, so it counts every folder with a config.
    pub(crate) fn origin(&self, file: &str, package: &str) -> Origin {
        if let Some(reason) = &self.overridden {
            return Origin::Index(Named::Unusable(reason.clone()));
        }
        let leaf = file.rsplit('/').next().unwrap_or(file).to_ascii_lowercase();
        let shared = leaf.ends_with(".props") || leaf.ends_with(".targets");
        let central = leaf == "directory.packages.props";
        let folder = &file[..file.rfind('/').map_or(0, |i| i + 1)];
        // A tool manifest sits in the `.config` folder of the project it
        // serves.
        let manifest = (folder == ".config/" || folder.ends_with("/.config/"))
            .then(|| &folder[..folder.len() - ".config/".len()]);
        let dir = match manifest {
            _ if central => folder,
            _ if shared => "",
            Some(project) => project,
            None => folder,
        };
        let here = Self::nearest(&self.chains, dir);
        // A project restores in its own folder alone; the others count every
        // folder below too — a central props file only those that govern a
        // project under them.
        let below = self
            .chains
            .iter()
            .filter(|(config, _)| {
                (shared || manifest.is_some())
                    && config.starts_with(dir)
                    && config.len() > dir.len()
                    && (!central || self.governs(folder, config))
            })
            .map(|(_, chain)| chain.clone());
        let origins = std::iter::once(here).chain(below).map(|chain| match chain {
            Ok(chain) => Self::origin_in(&chain, package),
            Err(reason) => Origin::Index(Named::Unusable(reason)),
        });
        Origin::of(origins).expect("one folder at least")
    }

    /// Where `package` comes from for a restore that sees `chain`.
    fn origin_in(chain: &Chain, package: &str) -> Origin {
        let unusable = |reason: &str| Origin::Index(Named::Unusable(reason.to_owned()));
        if chain
            .sources
            .iter()
            .all(|(_, named)| *named == Named::Public)
        {
            return Origin::Index(Named::Public);
        }
        let mappings = &chain.mappings;
        if mappings.is_empty() {
            return unusable(
                "several package sources and no packageSourceMapping in nuget.config to say which serves it",
            );
        }
        // NuGet merges mappings from several configs, replacing, dropping
        // and clearing in ways the Count does not follow, so only a mapping
        // that one config alone holds is read.
        let [mapping] = mappings.as_slice() else {
            return unusable(
                "packageSourceMapping in several nuget.config files, which the Count does not merge",
            );
        };
        let patterns: Vec<&(String, String)> = mapping.iter().collect();
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
        let best = patterns.iter().filter_map(|(_, p)| specificity(p)).max();
        let keys: Vec<&str> = patterns
            .iter()
            .filter(|(_, p)| best.is_some() && specificity(p) == best)
            .map(|(key, _)| key.as_str())
            .collect();
        let candidates = chain
            .sources
            .iter()
            .filter(|(key, _)| keys.iter().any(|k| k.eq_ignore_ascii_case(key)))
            .map(|(_, named)| Origin::Index(named.clone()));
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

/// What `text`, a project file or `Directory.Build.props`, says about
/// central package management: `Some(true)` when it opts out — the last
/// `ManagePackageVersionsCentrally` or `ImportDirectoryPackagesProps`
/// element is a literal `false` in a `PropertyGroup` with no `Condition`
/// on it or an ancestor, and no later `Import` could set it back —
/// `Some(false)` when it says anything else, and `None` when it is
/// silent. A file the Count cannot parse counts as saying something.
fn opt_out(text: &str) -> Option<bool> {
    let Ok(document) = roxmltree::Document::parse(text) else {
        return Some(false);
    };
    let named = |name: &str| -> Vec<roxmltree::Node<'_, '_>> {
        document
            .descendants()
            .filter(|node| node.is_element() && node.tag_name().name().eq_ignore_ascii_case(name))
            .collect()
    };
    let mut seen = false;
    let mut opted = false;
    for name in [
        "ManagePackageVersionsCentrally",
        "ImportDirectoryPackagesProps",
    ] {
        let Some(last) = named(name).into_iter().last() else {
            continue;
        };
        seen = true;
        let literal = last
            .text()
            .is_some_and(|text| text.trim().eq_ignore_ascii_case("false"));
        let grouped = last.parent().is_some_and(|parent| {
            parent.is_element()
                && parent
                    .tag_name()
                    .name()
                    .eq_ignore_ascii_case("propertygroup")
        });
        let conditioned = std::iter::once(last).chain(last.ancestors()).any(|node| {
            node.attributes()
                .any(|a| a.name().eq_ignore_ascii_case("condition"))
        });
        let imported = named("Import")
            .iter()
            .any(|import| import.range().start > last.range().start);
        opted = opted || (literal && grouped && !conditioned && !imported);
    }
    if opted {
        Some(true)
    } else {
        seen.then_some(false)
    }
}

/// The project paths a `.sln` or `.slnx` names: quoted references whose
/// extension ends in `proj`, case-insensitively, with `\` read as `/`.
fn solution_projects(text: &str) -> impl Iterator<Item = String> + '_ {
    static QUOTED: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#""([^"]*)""#).expect("quoted pattern"));
    QUOTED.captures_iter(text).filter_map(|c| {
        let path = c.get(1).unwrap().as_str().replace('\\', "/");
        path.rsplit_once('.')
            .is_some_and(|(_, ext)| ext.to_ascii_lowercase().ends_with("proj"))
            .then_some(path)
    })
}

/// `reference`, a path relative to `dir`, resolved to a repository path;
/// `None` when it leaves the repository.
fn resolve_path(dir: &str, reference: &str) -> Option<String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for part in reference.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// Whether `text`, already decoded and lowercased, holds a `<name`
/// element start: the tag followed by whitespace, `/` or `>`, so a
/// property such as `<TargetFramework` does not count as `<Target`.
pub(crate) fn has_element(text: &str, name: &str) -> bool {
    let open = format!("<{name}");
    text.match_indices(&open).any(|(at, _)| {
        text.as_bytes()
            .get(at + open.len())
            .is_some_and(|b| b.is_ascii_whitespace() || matches!(b, b'/' | b'>'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn character_references_are_decoded_and_long_runs_stay_cheap() {
        assert_eq!(decoded("Restore&#83;ources"), "RestoreSources");
        assert_eq!(decoded("Restore&#x53;ources"), "RestoreSources");
        assert_eq!(decoded("a &#; b &#xZZ; c &#"), "a &#; b &#xZZ; c &#");
        assert_eq!(decoded("é&#233;é"), "ééé");
        assert_eq!(decoded("Restore&#0000000083;ources"), "RestoreSources");
        assert_eq!(decoded("Restore&#x000000053;ources"), "RestoreSources");
        assert_eq!(decoded("&#99999999999;"), "&#99999999999;");
        let hostile = "&#".repeat(200_000);
        assert_eq!(decoded(&hostile), hostile);
    }

    #[test]
    fn keys_nuget_would_trim_or_fold_are_not_read() {
        let source = |key: &str| {
            format!(
                "<configuration><packageSources><add key=\"{key}\" value=\"https://nuget.acme.dev/v3/index.json\" /></packageSources></configuration>"
            )
        };
        assert!(config(&source("Acme Feed")).is_ok());
        for key in ["feed ", " feed", "", "fëed"] {
            assert!(config(&source(key)).is_err(), "{key:?}");
        }
        let mapping = "<configuration><packageSourceMapping><packageSource key=\"feed \"><package pattern=\"Acme.*\" /></packageSource></packageSourceMapping></configuration>";
        assert!(config(mapping).is_err());
    }

    #[test]
    fn msbuild_properties_that_move_the_config_override_it() {
        for property in [
            "RestoreSources",
            "RestoreAdditionalProjectFallbackFolders",
            "RestoreRootConfigDirectory",
            "RestoreSolutionDirectory",
            "RESTORECONFIGFILE",
        ] {
            let files = Files::from([(
                "Directory.Build.props".to_owned(),
                format!(
                    "<Project><PropertyGroup><{property}>x</{property}></PropertyGroup></Project>"
                ),
            )]);
            assert!(Feeds::new(&files).overridden.is_some(), "{property}");
        }
        let files = Files::from([(
            "build/publish.pubxml".to_owned(),
            crate::UNREADABLE_SETTINGS.to_owned(),
        )]);
        assert!(Feeds::new(&files).overridden.is_some());
    }
}
