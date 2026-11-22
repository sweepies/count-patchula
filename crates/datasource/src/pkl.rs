//! Pkl packages: the releases of the GitHub repository that publishes each
//! package, narrowed to that package's own tags and read as versions.
use super::*;
use patchula_config::PklPackage;

impl Client {
    pub(super) async fn pkl(&self, package: &str) -> Result<Catalog> {
        let stream = PklPackage::parse(package).ok_or(Error::Invalid)?;
        let releases = self
            .github_versions(Source::GithubReleases, &stream.repository)
            .await?;
        Ok(pkl_catalog(&stream, releases))
    }
}

/// `package`'s catalog from its repository's release catalog: only the
/// releases tagged as this package, each under its version. A monorepo such
/// as `apple/pkl-pantry` tags every package's releases in one repository.
pub fn pkl_catalog(package: &PklPackage, releases: Catalog) -> Catalog {
    Catalog {
        versions: releases
            .versions
            .into_iter()
            .filter_map(|release| {
                let version = package.version(&release.version)?.to_owned();
                Some(Version { version, ..release })
            })
            .collect(),
        dist_tags: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_package_keeps_only_its_own_tags() {
        let releases = Catalog {
            versions: [
                "pkl.toml@1.0.3",
                "k8s@1.2.0",
                "pkl.toml@1.1.0-rc.1",
                "pkl.toml@x",
            ]
            .into_iter()
            .map(|tag| Version {
                prerelease: tag.contains("rc"),
                ..Version::new(tag.into())
            })
            .collect(),
            dist_tags: BTreeMap::new(),
        };
        let package = PklPackage::parse("pkg.pkl-lang.org/pkl-pantry/pkl.toml").unwrap();
        let catalog = pkl_catalog(&package, releases);
        let versions: Vec<_> = catalog
            .versions
            .iter()
            .map(|v| (v.version.as_str(), v.prerelease))
            .collect();
        assert_eq!(versions, [("1.0.3", false), ("1.1.0-rc.1", true)]);
    }
}
