//! NuGet: `PackageReference`/`PackageVersion` items in MSBuild files
//! (projects, `Directory.Packages.props`, `Directory.Build.props`), local
//! tools in `.config/dotnet-tools.json` and the SDK in `global.json`.
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
