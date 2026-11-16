//! The dependency sources npm and Yarn Berry can resolve without running
//! code a dependency chose, and the check that refuses every other one.
//!
//! Both tools prepare a git dependency by packing it, and packing runs the
//! dependency's `prepare` and pack scripts whatever `--ignore-scripts` or
//! `enableScripts` say: npm through pacote's git fetcher (pacote 22 is the
//! first to honour `ignoreScripts` there, and the pinned npm ships an older
//! one), Yarn through `prepareExternalProject`, which its git and GitHub
//! fetchers call. Yarn's `exec:` protocol runs a generator script the
//! workspace names. Neither tool can be told to skip that, so a run whose
//! manifests, overrides, catalogs or lock name such a source is refused
//! before the tool starts, and a run whose new lock records one, such as a
//! git dependency of a registry package, is thrown away.
//!
//! The check is an allowlist. A source passes only in a form known to fetch
//! without running anything: a registry version, range or dist-tag, an
//! `npm:` alias of one, `jsr:`, `workspace:`, `catalog:`, a local `file:`,
//! `link:` or `portal:` path or a relative path, a Yarn `patch:` of a source
//! that passes, or an http(s) tarball ending in `.tgz` or `.tar.gz` on a
//! host that is not a git forge. Any other spelling, including git URLs,
//! `github:` and other forge shorthands, `owner/repo`, forge archive URLs
//! and `exec:`, is refused, so a form the Count does not know never reaches
//! the tool.

/// Hosts whose URLs npm (hosted-git-info) or Yarn (its GitHub fetcher) read
/// as git repositories rather than plain tarballs.
const FORGES: &[&str] = &[
    "github.com",
    "www.github.com",
    "codeload.github.com",
    "gist.github.com",
    "gitlab.com",
    "www.gitlab.com",
    "bitbucket.org",
    "www.bitbucket.org",
    "git.sr.ht",
];

/// Manifest fields whose values are dependency specs, by name.
const DECLARATIONS: &[&str] = &[
    "dependencies",
    "devDependencies",
    "optionalDependencies",
    "peerDependencies",
];

/// Manifest fields whose string leaves, at any depth, are dependency specs
/// keyed by selectors.
const OVERRIDES: &[&str] = &["overrides", "resolutions"];

/// Whether `spec` is a version, range or dist-tag: nothing that names a
/// path, URL, protocol or repository.
fn plain(spec: &str) -> bool {
    spec.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                ' ' | '\t' | '.' | '-' | '+' | '_' | '<' | '>' | '=' | '|' | '^' | '~' | '*'
            )
    })
}

/// The range of a `name@range` descriptor, `@scope/name@range` included;
/// `None` when it names no range.
fn range_of(descriptor: &str) -> Option<&str> {
    let at = descriptor.get(1..)?.find('@')? + 1;
    Some(&descriptor[at + 1..])
}

/// `text` with `%XX` escapes decoded, as Yarn encodes the source a
/// `patch:` wraps; `None` when an escape is malformed.
fn percent_decoded(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The host and path of an `http:` or `https:` URL, without user, port,
/// query or fragment, lowercased.
fn http_parts(spec: &str) -> Option<(String, String)> {
    let lower = spec.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))?;
    let rest = rest.split(['#', '?']).next().unwrap_or(rest);
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let host = host.split(':').next().unwrap_or(host);
    Some((host.to_owned(), path.to_owned()))
}

/// Whether a dependency spec, as a manifest, override, catalog or lock
/// declares it, fetches without running code a dependency chose.
pub(super) fn allowed(spec: &str) -> bool {
    let spec = spec.trim();
    // Yarn appends binding parameters to a reference after `::`.
    let spec = spec.split_once("::").map_or(spec, |(head, _)| head);
    if plain(spec) {
        return true;
    }
    for local in [
        "workspace:",
        "catalog:",
        "file:",
        "link:",
        "portal:",
        "jsr:",
    ] {
        if spec.starts_with(local) {
            return true;
        }
    }
    if spec.starts_with("./")
        || spec.starts_with("../")
        || spec.starts_with('/')
        || spec.starts_with("~/")
        || spec == "."
        || spec == ".."
    {
        return true;
    }
    if let Some(alias) = spec.strip_prefix("npm:") {
        return plain(range_of(alias).unwrap_or(alias));
    }
    if let Some(patched) = spec.strip_prefix("patch:") {
        let source = patched.split('#').next().unwrap_or(patched);
        return percent_decoded(source)
            .as_deref()
            .and_then(range_of)
            .is_some_and(|range| !range.starts_with("patch:") && allowed(range));
    }
    if let Some((host, path)) = http_parts(spec) {
        return !FORGES.contains(&host.as_str())
            && (path.ends_with(".tgz") || path.ends_with(".tar.gz"));
    }
    false
}

/// Whether a lock's `resolved` or `version` value names a git source.
fn git_source(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    [
        "git+",
        "git:",
        "git@",
        "ssh:",
        "github:",
        "gitlab:",
        "bitbucket:",
        "gist:",
        "sourcehut:",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
        || http_parts(&lower)
            .is_some_and(|(host, path)| FORGES.contains(&host.as_str()) || path.ends_with(".git"))
        || lower
            .split('#')
            .next()
            .is_some_and(|head| head.ends_with(".git"))
}

/// Why `tool` refuses a run: `path` names `name` from `spec`.
fn refusal(tool: &str, path: &str, name: &str, spec: &str) -> String {
    format!(
        "{path} names {name} from {spec}, a source {tool} cannot lock without running code the dependency chose (a git, forge or exec: source); only registry, local and plain tarball sources are locked"
    )
}

/// The first string leaf of `value`, at any depth, whose spec is refused,
/// with its key. Override values may also refer to a direct dependency's
/// spec with `$name`.
fn refused_leaf<'a>(key: &'a str, value: &'a serde_json::Value) -> Option<(&'a str, &'a str)> {
    match value {
        serde_json::Value::String(spec) => {
            (!spec.starts_with('$') && !allowed(spec)).then_some((key, spec.as_str()))
        }
        serde_json::Value::Object(map) => {
            map.iter().find_map(|(key, value)| refused_leaf(key, value))
        }
        _ => None,
    }
}

/// Why `tool` refuses `path`, a `package.json`: a declaration or override
/// whose source runs code. A manifest the Count cannot read is refused too,
/// since nothing proves it clean.
pub(super) fn manifest(tool: &str, path: &str, text: &str) -> Option<String> {
    let Ok(serde_json::Value::Object(object)) = serde_json::from_str(text) else {
        return Some(format!(
            "{path} is not a JSON object the Count can read, so nothing proves {tool} would lock it without running code"
        ));
    };
    for field in DECLARATIONS {
        if let Some(serde_json::Value::Object(map)) = object.get(*field) {
            for (name, spec) in map {
                match spec.as_str() {
                    Some(spec) if allowed(spec) => {}
                    Some(spec) => return Some(refusal(tool, path, name, spec)),
                    None => return Some(refusal(tool, path, name, &spec.to_string())),
                }
            }
        }
    }
    for field in OVERRIDES {
        if let Some(value) = object.get(*field)
            && let Some((name, spec)) = refused_leaf(field, value)
        {
            return Some(refusal(tool, path, name, spec));
        }
    }
    None
}

/// Why Yarn refuses `path`, a `.yarnrc.yml`: a catalog entry or package
/// extension whose source runs code.
pub(super) fn yarnrc(path: &str, text: &str) -> Option<String> {
    let Ok(value) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(text) else {
        return Some(format!(
            "{path} is not YAML the Count can read, so nothing proves Yarn would lock without running code"
        ));
    };
    fn leaf(key: &str, value: &serde_yaml_ng::Value) -> Option<(String, String)> {
        match value {
            serde_yaml_ng::Value::String(spec) => {
                (!allowed(spec)).then(|| (key.to_owned(), spec.clone()))
            }
            serde_yaml_ng::Value::Mapping(map) => map
                .iter()
                .find_map(|(key, value)| leaf(key.as_str().unwrap_or_default(), value)),
            _ => None,
        }
    }
    for field in ["catalog", "catalogs", "packageExtensions"] {
        if let Some(value) = value.get(field)
            && let Some((name, spec)) = leaf(field, value)
        {
            return Some(refusal("Yarn", path, &name, &spec));
        }
    }
    None
}

/// What a lock says about its sources.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Lock {
    /// Every source it records passes.
    Clean,
    /// Why it is refused.
    Refused(String),
    /// The Count cannot read it, so nothing proves it clean.
    Unreadable,
}

/// What `path`, a `package-lock.json` or `npm-shrinkwrap.json`, says to npm:
/// refused for an entry resolved from git or a dependency spec that runs
/// code.
pub(super) fn npm_lock(path: &str, text: &str) -> Lock {
    fn walk(path: &str, key: &str, value: &serde_json::Value) -> Option<String> {
        let serde_json::Value::Object(map) = value else {
            return None;
        };
        for (field, value) in map {
            match value {
                serde_json::Value::String(text)
                    if matches!(field.as_str(), "resolved" | "version") && git_source(text) =>
                {
                    return Some(refusal("npm", path, key, text));
                }
                serde_json::Value::Object(inner)
                    if DECLARATIONS.contains(&field.as_str()) || field == "requires" =>
                {
                    for (name, spec) in inner {
                        match spec {
                            serde_json::Value::String(spec) if !allowed(spec) => {
                                return Some(refusal("npm", path, name, spec));
                            }
                            // Lockfile v1 nests entries under `dependencies`.
                            serde_json::Value::Object(_) => {
                                if let Some(reason) = walk(path, name, spec) {
                                    return Some(reason);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                serde_json::Value::Object(_) => {
                    if let Some(reason) = walk(path, field, value) {
                        return Some(reason);
                    }
                }
                _ => {}
            }
        }
        None
    }
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value) => walk(path, "", &value).map_or(Lock::Clean, Lock::Refused),
        Err(_) => Lock::Unreadable,
    }
}

/// What `path`, a Berry `yarn.lock`, says to Yarn: refused for a
/// descriptor, resolution or dependency whose source runs code.
pub(super) fn berry_lock(path: &str, text: &str) -> Lock {
    let Ok(serde_yaml_ng::Value::Mapping(entries)) =
        serde_yaml_ng::from_str::<serde_yaml_ng::Value>(text)
    else {
        return Lock::Unreadable;
    };
    berry_entries(path, &entries).map_or(Lock::Clean, Lock::Refused)
}

fn berry_entries(path: &str, entries: &serde_yaml_ng::Mapping) -> Option<String> {
    for (key, entry) in entries {
        let Some(key) = key.as_str() else {
            continue;
        };
        if key == "__metadata" {
            continue;
        }
        for descriptor in key.split(',').map(str::trim) {
            match range_of(descriptor) {
                Some(range) if allowed(range) => {}
                _ => return Some(refusal("Yarn", path, descriptor, descriptor)),
            }
        }
        if let Some(resolution) = entry.get("resolution").and_then(|v| v.as_str()) {
            match range_of(resolution) {
                Some(reference) if allowed(reference) => {}
                _ => return Some(refusal("Yarn", path, key, resolution)),
            }
        }
        for field in ["dependencies", "peerDependencies"] {
            if let Some(serde_yaml_ng::Value::Mapping(map)) = entry.get(field) {
                for (name, spec) in map {
                    let name = name.as_str().unwrap_or_default();
                    match spec.as_str() {
                        Some(spec) if allowed(spec) => {}
                        _ => {
                            return Some(refusal(
                                "Yarn",
                                path,
                                name,
                                &serde_yaml_ng::to_string(spec).unwrap_or_default(),
                            ));
                        }
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_local_and_tarball_sources_pass() {
        for spec in [
            "",
            "*",
            "latest",
            "next",
            "1.2.3",
            "^1.2.3",
            "~1.2",
            ">=1.0.0 <2.0.0",
            "1.x || ^2.0.0-beta.1",
            "1.0.0 - 2.0.0",
            "npm:^1.0.0",
            "npm:other@^1.0.0",
            "npm:@scope/other@1.0.0",
            "npm:1.0.0::__archiveUrl=https%3A%2F%2Fregistry.example%2Fx.tgz",
            "jsr:@std/path@^1",
            "workspace:^",
            "workspace:packages/a",
            "catalog:",
            "catalog:react18",
            "file:./local",
            "file:../vendor/x-1.0.0.tgz",
            "file:./local#./local::hash=abc&locator=app%40workspace%3A.",
            "link:../x",
            "portal:../x",
            "./local",
            "../sibling",
            "patch:lodash@npm%3A4.17.21#~/.yarn/patches/lodash.patch::version=4.17.21&hash=1",
            "patch:typescript@npm%3A^5.0.0#optional!builtin<compat/typescript>",
            "https://registry.example.com/x/-/x-1.0.0.tgz",
            "http://127.0.0.1:4873/x/-/x-1.0.0.tar.gz",
            "https://cdn.example.com/x.tgz?sig=1",
        ] {
            assert!(allowed(spec), "{spec:?} must pass");
        }
    }

    #[test]
    fn git_forge_and_exec_sources_are_refused() {
        for spec in [
            "git+https://example.com/x.git",
            "git+ssh://git@example.com/x.git#v1",
            "git://example.com/x.git",
            "git+file:///tmp/x",
            "git@github.com:owner/repo.git",
            "ssh://git@example.com/x.git",
            "github:owner/repo",
            "github:owner/repo#0123456789abcdef0123456789abcdef01234567",
            "gitlab:owner/repo",
            "bitbucket:owner/repo",
            "gist:abc",
            "owner/repo",
            "owner/repo#semver:^1",
            "https://github.com/owner/repo",
            "https://github.com/owner/repo.git",
            "https://github.com/owner/repo/tarball/main",
            "https://github.com/owner/repo/archive/v1.tar.gz",
            "https://codeload.github.com/owner/repo/tar.gz/main",
            "https://gitlab.com/owner/repo/-/archive/v1/repo-v1.tar.gz",
            "https://example.com/owner/repo.git",
            "https://example.com/x",
            "exec:./generate.js",
            "patch:evil@github%3Aowner/repo#./x.patch",
            "patch:evil@patch%3Ax%40npm%253A1.0.0%23a#b",
            "npm:x@github:owner/repo",
            "unknown:thing",
        ] {
            assert!(!allowed(spec), "{spec:?} must be refused");
        }
    }

    #[test]
    fn manifests_are_refused_for_git_declarations_and_overrides() {
        let clean = r#"{"dependencies":{"a":"^1"},"devDependencies":{"b":"workspace:*"},"overrides":{"c":{".":"1.0.0","d":"$a"}},"resolutions":{"**/e":"npm:2.0.0"}}"#;
        assert_eq!(manifest("npm", "package.json", clean), None);
        for text in [
            r#"{"dependencies":{"evil":"github:owner/evil"}}"#,
            r#"{"optionalDependencies":{"evil":"owner/evil"}}"#,
            r#"{"peerDependencies":{"evil":"git+https://example.com/evil.git"}}"#,
            r#"{"overrides":{"a":{"evil":"github:owner/evil"}}}"#,
            r#"{"resolutions":{"**/evil":"https://github.com/owner/evil.git"}}"#,
            r#"{"dependencies":{"evil":"exec:./gen.js"}}"#,
            r#"{"dependencies":{"evil":1}}"#,
            "not json",
        ] {
            assert!(
                manifest("npm", "app/package.json", text).is_some(),
                "{text}"
            );
        }
        let reason = manifest(
            "Yarn",
            "app/package.json",
            r#"{"dependencies":{"evil":"github:owner/evil"}}"#,
        )
        .unwrap();
        assert!(
            reason.contains("app/package.json names evil from github:owner/evil")
                && reason.contains("Yarn"),
            "{reason}"
        );
    }

    #[test]
    fn yarnrc_catalogs_and_package_extensions_are_checked() {
        assert_eq!(
            yarnrc(
                ".yarnrc.yml",
                "catalog:\n  a: ^1\ncatalogs:\n  r18:\n    react: 18.0.0\npackageExtensions:\n  \"x@*\":\n    dependencies:\n      y: ^1\n    peerDependenciesMeta:\n      z:\n        optional: true\n"
            ),
            None
        );
        for text in [
            "catalog:\n  evil: github:owner/evil\n",
            "catalogs:\n  g:\n    evil: owner/evil\n",
            "packageExtensions:\n  \"x@*\":\n    dependencies:\n      evil: \"git+https://example.com/e.git\"\n",
        ] {
            assert!(yarnrc(".yarnrc.yml", text).is_some(), "{text}");
        }
    }

    #[test]
    fn npm_locks_are_refused_for_git_entries_at_any_depth() {
        let clean = r#"{"lockfileVersion":3,"packages":{"":{"dependencies":{"a":"^1"}},"node_modules/a":{"version":"1.0.0","resolved":"https://npm.pkg.github.com/download/@o/a/1.0.0/abc","dependencies":{"b":"~2"}},"node_modules/l":{"resolved":"local","link":true}}}"#;
        assert_eq!(npm_lock("package-lock.json", clean), Lock::Clean);
        for text in [
            r#"{"lockfileVersion":3,"packages":{"node_modules/evil":{"version":"1.0.0","resolved":"git+ssh://git@github.com/owner/evil.git#0123"}}}"#,
            r#"{"lockfileVersion":3,"packages":{"node_modules/a":{"version":"1.0.0","dependencies":{"evil":"github:owner/evil"}}}}"#,
            r#"{"lockfileVersion":1,"dependencies":{"a":{"version":"1.0.0","dependencies":{"evil":{"version":"github:owner/evil#0123"}}}}}"#,
            r#"{"lockfileVersion":1,"dependencies":{"a":{"version":"1.0.0","requires":{"evil":"owner/evil"}}}}"#,
        ] {
            assert!(
                matches!(npm_lock("package-lock.json", text), Lock::Refused(_)),
                "{text}"
            );
        }
        assert_eq!(npm_lock("package-lock.json", "{"), Lock::Unreadable);
    }

    #[test]
    fn berry_locks_are_refused_for_git_entries() {
        let clean = "__metadata:\n  version: 8\n\n\"a@npm:^1.0.0, a@npm:^1.1.0\":\n  version: 1.1.0\n  resolution: \"a@npm:1.1.0\"\n  dependencies:\n    b: \"npm:^2.0.0\"\n  checksum: abc\n  languageName: node\n  linkType: hard\n\n\"app@workspace:.\":\n  version: 0.0.0-use.local\n  resolution: \"app@workspace:.\"\n  languageName: unknown\n  linkType: soft\n\n\"local-fixture@file:./local::locator=app%40workspace%3A.\":\n  version: 1.0.0\n  resolution: \"local-fixture@file:./local#./local::hash=1&locator=app%40workspace%3A.\"\n  languageName: node\n  linkType: hard\n";
        assert_eq!(berry_lock("yarn.lock", clean), Lock::Clean);
        for text in [
            "__metadata:\n  version: 8\n\n\"evil@https://github.com/owner/evil.git\":\n  version: 1.0.0\n  resolution: \"evil@https://github.com/owner/evil.git#commit=0123\"\n",
            "__metadata:\n  version: 8\n\n\"a@npm:^1.0.0\":\n  version: 1.0.0\n  resolution: \"a@npm:1.0.0\"\n  dependencies:\n    evil: \"github:owner/evil\"\n",
            "__metadata:\n  version: 8\n\n\"gen@exec:./gen.js::locator=app%40workspace%3A.\":\n  version: 0.0.0\n  resolution: \"gen@exec:./gen.js#./gen.js::hash=1&locator=app%40workspace%3A.\"\n",
        ] {
            assert!(
                matches!(berry_lock("yarn.lock", text), Lock::Refused(_)),
                "{text}"
            );
        }
        assert_eq!(berry_lock("yarn.lock", "plain text\n"), Lock::Unreadable);
    }
}
