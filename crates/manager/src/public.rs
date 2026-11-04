//! Whether a file lets a lockfile tool fetch only from its ecosystem's
//! well-known public registry — the `public_only` verdicts that decide
//! whether a tool run may share the `public` cache partition. The runner
//! checks every file a run can read; a file with no verdict, or one the
//! Count cannot read, is not public.
//!
//! A file is public-only when every registry, index or fetch source it
//! names is the public registry, a well-known public forge, or a local
//! source that fetches nothing (a path, a link, a workspace member).
//! Anything else fails closed: private content must never enter a cache
//! shared with other installations, and a fetch the Count cannot rule on
//! might be one.
use crate::Files;
use crate::origins::Public;

/// Well-known public forges: their content is public by construction, so
/// a fetch there can neither hold private packages nor answer as another
/// installation's private registry.
const PUBLIC_HOSTS: &[&str] = &["github.com", "gitlab.com", "bitbucket.org", "codeberg.org"];

/// The host a fetch `url` reaches: the authority's host without its
/// userinfo or port, for the schemes tools fetch over.
pub(crate) fn url_host(url: &str) -> Option<String> {
    let rest = ["https://", "http://", "ssh://", "git://", "ftp://"]
        .iter()
        .find_map(|scheme| {
            url.get(..scheme.len())
                .filter(|prefix| prefix.eq_ignore_ascii_case(scheme))
                .map(|_| &url[scheme.len()..])
        })?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Whether `value`, a URL or specifier a manifest or lockfile names for a
/// fetch, can only reach public content: the ecosystem's public registry
/// or a well-known public host, or a local source that fetches nothing.
/// Anything else fails closed.
pub(crate) fn fetch_ok(value: &str, public: &Public) -> bool {
    let value = value.trim().trim_matches(['"', '\'']);
    // SCP-style `git@host:path`.
    if let Some(rest) = value.strip_prefix("git@") {
        return rest
            .split(':')
            .next()
            .is_some_and(|host| PUBLIC_HOSTS.contains(&host));
    }
    // Yarn locators `name@protocol:reference`: check the reference.
    let value = value
        .rsplit_once('@')
        .filter(|(before, after)| {
            !before.is_empty() && !before.contains("://") && after.contains(':')
        })
        .map_or(value, |(_, after)| after);
    for prefix in ["git+", "sparse+", "registry+", "tar+"] {
        if let Some(rest) = value.strip_prefix(prefix) {
            return fetch_ok(rest, public);
        }
    }
    if (public.is_public)(value) {
        return true;
    }
    let Some((scheme, rest)) = value.split_once(':') else {
        // No scheme: a version, a hash or a local path.
        return true;
    };
    match scheme.to_ascii_lowercase().as_str() {
        // Local sources fetch nothing.
        "file" | "link" | "workspace" | "portal" | "path" | "catalog" | "patch" | "npm"
        | "exec" | "root" | "virtual" | "editable" | "directory" | "local" | "static"
        | "symlink" => true,
        // The public forges.
        "github" | "gitlab" | "bitbucket" | "gist" => true,
        "https" | "http" | "ssh" | "git" | "ftp" => {
            let Some(authority) = rest
                .strip_prefix("//")
                .and_then(|rest| rest.split(['/', '?', '#']).next())
            else {
                return false;
            };
            // A URL carrying a password is not a public fetch even on a
            // public host; `$VAR` and `%VAR%` placeholders resolve at run
            // time and hold nothing.
            if let Some((userinfo, _)) = authority.rsplit_once('@')
                && userinfo.split(':').nth(1).is_some_and(|password| {
                    !password.is_empty() && !password.starts_with(['$', '%'])
                })
            {
                return false;
            }
            url_host(value).is_some_and(|host| PUBLIC_HOSTS.contains(&host.as_str()))
        }
        _ => false,
    }
}

/// Whether `path` can steer what a lockfile tool fetches, and if so
/// whether every fetch it can name is public-only. Files are decided one
/// at a time; verdicts on the settings files around a file come from the
/// runner checking those too, so each verdict covers the file alone —
/// plus, for a Gemfile, the lockfile sitting beside it that its `source`
/// lines and Bundler's resolution share.
pub(crate) fn verdict(path: &str, text: &str, files: &Files) -> Option<bool> {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    let flag = match leaf {
        "package.json" => crate::npm::manifest_public(text),
        ".npmrc" | ".yarnrc.yml" => crate::npm::settings_public(leaf, text),
        "pnpm-workspace.yaml" => crate::npm::workspace_public(text),
        "package-lock.json" | "npm-shrinkwrap.json" => crate::npm::json_lock_public(text, false),
        "bun.lock" => crate::npm::json_lock_public(text, true),
        "yarn.lock" => crate::npm::yarn_lock_public(text),
        "pnpm-lock.yaml" | "aube-lock.yaml" => crate::npm::yaml_lock_public(text),
        "pyproject.toml" | "uv.toml" | "poetry.toml" | "pdm.toml" | "pixi.toml" | "hatch.toml" => {
            crate::pypi::settings::public(path, text)
        }
        "uv.lock" | "poetry.lock" => crate::pypi::settings::lock_public(path, text),
        "Gemfile" => crate::bundler::public_only(path, text, files),
        "Gemfile.lock" => crate::bundler::lock_public(text),
        "composer.json" => crate::composer::manifest_public(text),
        "composer.lock" => crate::composer::lock_public(text),
        "Cargo.toml" => crate::cargo::manifest_public(text),
        "Cargo.lock" => crate::cargo::lock_public(text),
        _ if crate::nuget::config_file(path) => crate::nuget::config_public(text),
        _ if crate::nuget::msbuild_file(path) => crate::nuget::msbuild_public(text),
        _ => return None,
    };
    // A file that could not be read can name anything.
    Some(flag && text != crate::UNREADABLE_SETTINGS)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Files are decided one at a time: a file is public-only when every
    /// fetch it can name is the public registry, a public forge or a
    /// local source, and it fails closed when the Count cannot read it.
    #[test]
    fn verdicts_fail_closed_on_anything_not_public() {
        let files = Files::from([
            ("package.json".into(), "{}".into()),
            (".npmrc".into(), "registry=https://registry.npmjs.org/\n".into()),
            ("package-lock.json".into(), "{\"packages\":{\"\":{\"resolved\":\"https://registry.npmjs.org/x/-/x-1.0.0.tgz\"}}}".into()),
        ]);
        assert_eq!(verdict("package.json", "{}", &files), Some(true));
        assert_eq!(
            verdict(".npmrc", "registry=https://registry.npmjs.org/\n", &files),
            Some(true)
        );
        // A private or unknown registry is not public.
        for text in [
            "registry=https://npm.acme.dev/\n",
            "registry=${NPM_REGISTRY}\n",
            // A file the Count could not read.
            crate::UNREADABLE_SETTINGS,
        ] {
            assert_eq!(verdict(".npmrc", text, &files), Some(false), "{text:?}");
        }
        // A lockfile `resolved` URL on a non-public host is not public.
        let lock = "{\"packages\":{\"\":{\"resolved\":\"https://npm.acme.dev/x.tgz\"}}}";
        assert_eq!(verdict("package-lock.json", lock, &files), Some(false));
        let lock = "{\"packages\":{\"\":{\"resolved\":\"https://registry.npmjs.org/x.tgz\"}}}";
        assert_eq!(verdict("package-lock.json", lock, &files), Some(true));
        // A Gemfury-style index URL is not public.
        let pyproject = "[tool.uv]\nindex-url = \"https://pypi.fury.io/acme/simple\"\n";
        assert_eq!(verdict("pyproject.toml", pyproject, &files), Some(false));
        let pyproject = "[project]\ndependencies = [\"requests\"]\n";
        assert_eq!(verdict("pyproject.toml", pyproject, &files), Some(true));
        // A non-steering file has no verdict.
        assert_eq!(verdict("README.md", "hi", &files), None);
    }

    #[test]
    fn fetch_urls_split_public_from_private() {
        let npm = crate::origins::Public {
            source: patchula_config::Source::Npm,
            label: "npm",
            is_public: |url| {
                crate::web_host(url).is_some_and(|host| {
                    matches!(host.as_str(), "registry.npmjs.org" | "registry.yarnpkg.com")
                })
            },
        };
        for (value, expect) in [
            (
                "https://registry.npmjs.org/lodash/-/lodash-4.17.21.tgz",
                true,
            ),
            ("https://npm.acme.dev/lodash/-/lodash-4.17.21.tgz", false),
            ("git+ssh://git@github.com/acme/x.git", true),
            ("git+ssh://git@gitlab.acme.dev/acme/x.git", false),
            ("git@github.com:acme/x.git", true),
            ("ssh://git@github.com/acme/x.git", true),
            ("https://github.com/acme/x.git", true),
            ("https://gitlab.acme.dev/acme/x.git", false),
            ("lodash@npm:4.17.21", true),
            ("x@jsr:@scope/x", false),
            ("file:../x", true),
            ("workspace:*", true),
            ("portal:../x", true),
            ("sha512-deadbeef", true),
            ("1.2.3", true),
            ("https://user@bitbucket.org/acme/x.git", true),
            ("https://ci:s3cret@github.com/acme/x.git", false), // betterleaks:allow
            ("https://ci:${TOKEN}@github.com/acme/x.git", true),
        ] {
            assert_eq!(fetch_ok(value, &npm), expect, "{value}");
        }
    }
}
