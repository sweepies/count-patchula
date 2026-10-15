use super::*;

impl Client {
    pub(super) async fn mise(&self, package: &str) -> Result<Catalog> {
        match package {
            "node" | "core:node" => self.node().await,
            _ => {
                let r = rustovate_mise::resolve(package).ok_or(Error::Invalid)?;
                // Box the recursion: aliases resolve to actual release sources.
                if r.source == Source::Mise && r.package == package {
                    return Err(Error::Invalid);
                }
                let mut catalog = Box::pin(self.lookup(r.source, &r.package)).await?;
                if let Some(extract) = r.extract {
                    let regex = regex::Regex::new(&extract).map_err(|_| Error::Invalid)?;
                    catalog.versions.retain_mut(|v| {
                        let Some(value) = regex
                            .captures(&v.version)
                            .and_then(|c| c.name("version"))
                            .map(|v| v.as_str().to_owned())
                        else {
                            return false;
                        };
                        v.version = value;
                        true
                    });
                }
                Ok(catalog)
            }
        }
    }
    pub(super) async fn node(&self) -> Result<Catalog> {
        let response = self
            .http
            .get("https://nodejs.org/dist/index.json", "application/json")
            .await?;
        let values: Vec<Value> = serde_json::from_slice(&response.body)?;
        let mut versions = Vec::with_capacity(values.len());
        let mut lts = Vec::new();
        for value in values {
            let version = value["version"]
                .as_str()
                .ok_or(Error::Invalid)?
                .trim_start_matches('v');
            let date = value["date"].as_str().ok_or(Error::Invalid)?;
            if value["lts"].as_str().is_some_and(|s| !s.is_empty()) {
                lts.push(version.to_owned());
            }
            versions.push(Version {
                version: version.into(),
                published_at: Some(format!("{date}T00:00:00Z")),
                digest: None,
                deprecated: false,
                yanked: false,
            });
        }
        sort(&mut versions, Ecosystem::Loose);
        let latest_lts = lts
            .into_iter()
            .filter_map(|v| Some((Ecosystem::Loose.parse(&v).ok()?, v)))
            .max_by(|(a, _), (b, _)| Ecosystem::Loose.order(a, b))
            .map(|(_, v)| v);
        Ok(Catalog {
            versions,
            dist_tags: latest_lts
                .map(|v| BTreeMap::from([("lts".into(), v)]))
                .unwrap_or_default(),
        })
    }
}
