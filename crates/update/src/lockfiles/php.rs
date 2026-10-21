//! `composer.lock` through `composer update --no-install`, with PHP and
//! Composer from mise. Static PHP downloads a prebuilt static-php.dev binary;
//! compiled PHP builds `php@<version>` from source on first use.
use super::*;
use patchula_config::PhpBuild;

/// The mise tool specs for PHP and Composer. Both use the http backend for
/// the static build, so no compiler or system libraries are needed.
pub(super) fn tools(build: PhpBuild, php: &str, composer: &str) -> Vec<(String, String)> {
    let os = match std::env::consts::OS {
        "macos" => "macos",
        _ => "linux",
    };
    let php = match build {
        PhpBuild::Static => (
            format!(
                "http:php[url=https://dl.static-php.dev/static-php-cli/common/php-{{{{version}}}}-cli-{os}-{}.tar.gz]",
                std::env::consts::ARCH
            ),
            php.to_string(),
        ),
        PhpBuild::Compiled => ("php".into(), php.to_string()),
    };
    vec![
        php,
        (
            "http:composer[url=https://getcomposer.org/download/{{version}}/composer.phar,bin=composer]".into(),
            composer.to_string(),
        ),
    ]
}

impl Runner {
    pub(super) async fn composer_lock(
        &self,
        context: node::NodeContext<'_>,
        jobs: &[&Job],
    ) -> Result<(), String> {
        let node::NodeContext {
            driver,
            root,
            baseline,
            directory,
            config,
            versions,
            maintenance,
        } = context;
        let php = node::version(versions, "php")?;
        let full = php.split('.').count() == 3
            && php
                .split('.')
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
        if config.lockfiles.php == PhpBuild::Static && !full {
            return Err(format!(
                "static PHP needs an exact version, not {php}; pin php to x.y.z or set lockfiles.php = \"compiled\""
            ));
        }
        let cwd = root.join(directory);
        let lock = node::join(directory, "composer.lock");
        let packages: BTreeSet<_> = jobs.iter().filter_map(|j| j.package.clone()).collect();
        let mut args = vec!["update".to_string()];
        if !maintenance {
            args.extend(packages.iter().cloned());
            // Only the transitive changes the requested versions need.
            args.extend(["--with-all-dependencies".into(), "--minimal-changes".into()]);
        }
        args.extend(
            [
                "--no-install",
                "--no-scripts",
                "--no-plugins",
                "--no-interaction",
                "--no-progress",
                // The PHP version still constrains resolution; extensions and
                // system libraries describe the deployment, not this runner.
                "--ignore-platform-req=ext-*",
                "--ignore-platform-req=lib-*",
            ]
            .map(String::from),
        );
        let home = driver.join("composer");
        let path = |p: &str| home.join(p).to_string_lossy().into_owned();
        self.tool(
            driver,
            Invocation {
                tools: tools(
                    config.lockfiles.php,
                    &php,
                    &node::version(versions, "composer")?,
                ),
                bin: "composer".into(),
                cwd: Some(cwd),
                env: vec![
                    ("COMPOSER_HOME".into(), path("home")),
                    ("COMPOSER_CACHE_DIR".into(), path("cache")),
                    ("COMPOSER_NO_AUDIT".into(), "1".into()),
                    ("COMPOSER_ALLOW_SUPERUSER".into(), "1".into()),
                ],
                args,
            },
        )
        .await?;
        let before = baseline.get(&lock).map(|b| b.content.as_str());
        let after = std::fs::read_to_string(root.join(&lock)).ok();
        if !maintenance && before == after.as_deref() {
            return Err(format!(
                "composer.lock did not change after the manifest edit in {}",
                if directory.is_empty() {
                    "the repository root"
                } else {
                    directory
                }
            ));
        }
        Ok(())
    }
}
