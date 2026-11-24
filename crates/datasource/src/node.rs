//! nodejs.org's release index, which names Node's LTS lines: the `lts`
//! dist-tag `actions/node-versions` catalogs carry. The index is read into
//! its newest LTS version alone.
use super::*;

/// The GitHub repository whose LTS line nodejs.org, not its releases, names.
const NODE_VERSIONS: &str = "actions/node-versions";

/// The newest LTS version the index lists, without its `v`. Every entry
/// must name a version; one whose `lts` is a non-empty codename is an LTS
/// release.
fn newest_lts(
    body: &mut dyn std::io::Read,
) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    #[derive(Deserialize)]
    struct Release {
        version: String,
        #[serde(default)]
        lts: Option<Lts>,
    }
    /// The line's codename, or `false`.
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Lts {
        Codename(String),
        Other(serde::de::IgnoredAny),
    }
    let releases: Vec<Release> = patchula_http::conditional::json_document(body)
        .map_err(|_| patchula_http::CacheError::Document)?;
    let newest = releases
        .into_iter()
        .filter(|release| matches!(&release.lts, Some(Lts::Codename(name)) if !name.is_empty()))
        .map(|release| release.version.trim_start_matches('v').to_owned())
        .filter_map(|v| Some((Ecosystem::Loose.parse(&v).ok()?, v)))
        .max_by(|(a, _), (b, _)| Ecosystem::Loose.order(a, b))
        .map(|(_, v)| v);
    serde_json::to_vec(&newest).map_err(|_| patchula_http::CacheError::Document)
}
pub(crate) const NODE_LTS: patchula_http::Derived<patchula_http::CacheError> =
    patchula_http::Derived::new("node-lts", 1, newest_lts);

impl Client {
    /// The `lts` dist-tag `actions/node-versions` catalogs carry. Its
    /// releases are GitHub releases, which mise does not list, so the
    /// evidence is nodejs.org's (see `node_lts`). A failed read leaves the
    /// catalog without it: stable setup-node targets remain usable during a
    /// Node-index outage, and an explicit LTS policy fails closed without
    /// this evidence.
    pub(super) async fn node_lts_tags(&self, package: &str) -> BTreeMap<String, String> {
        if !package.eq_ignore_ascii_case(NODE_VERSIONS) {
            return BTreeMap::new();
        }
        self.node_lts().await.unwrap_or_default()
    }
    /// The LTS evidence nodejs.org's release index publishes.
    async fn node_lts(&self) -> Result<BTreeMap<String, String>> {
        let response = self
            .http
            .get_derived(
                "https://nodejs.org/dist/index.json",
                "application/json",
                &NODE_LTS,
            )
            .await?;
        let newest: Option<String> = serde_json::from_slice(&response.body)?;
        Ok(newest
            .map(|v| BTreeMap::from([("lts".into(), v)]))
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The newest LTS line wins by version, whatever order the index lists
    /// it in; a release without a codename is not LTS.
    #[test]
    fn the_index_reads_into_its_newest_lts_version() {
        let body = br#"[
            {"version":"v25.1.0","lts":false,"files":["linux-x64"]},
            {"version":"v22.11.0","lts":"Jod","npm":"10.9.0"},
            {"version":"v24.2.0","lts":"Krypton"},
            {"version":"v20.18.0","lts":"Iron"},
            {"version":"v23.0.0","lts":""}
        ]"#;
        let newest: Option<String> =
            serde_json::from_slice(&newest_lts(&mut &body[..]).unwrap()).unwrap();
        assert_eq!(newest.as_deref(), Some("24.2.0"));
        let none: Option<String> = serde_json::from_slice(
            &newest_lts(&mut &br#"[{"version":"v1.0.0","lts":false}]"#[..]).unwrap(),
        )
        .unwrap();
        assert_eq!(none, None);
        // An entry without a version fails the index, as it did unread.
        assert!(newest_lts(&mut &br#"[{"lts":"Iron"}]"#[..]).is_err());
    }
}
