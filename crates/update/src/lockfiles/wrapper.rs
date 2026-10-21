//! The Gradle wrapper's `distributionSha256Sum` follows its edited
//! `distributionUrl`, from the checksum Gradle publishes beside each
//! distribution. Only Gradle's own distribution hosts are contacted.
use super::*;

const HOSTS: [&str; 2] = ["services.gradle.org", "downloads.gradle.org"];

fn official(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|u| {
        u.scheme() == "https"
            && u.host_str().is_some_and(|h| HOSTS.contains(&h))
            && u.path().starts_with("/distributions/gradle-")
            && u.path().ends_with(".zip.sha256")
    })
}

impl Runner {
    async fn checksum(&self, url: &str) -> Result<String, String> {
        let mut url = format!("{url}.sha256");
        for _ in 0..3 {
            if !official(&url) {
                return Err(format!("refusing Gradle checksum from {url}"));
            }
            let request = reqwest::Request::new(
                reqwest::Method::GET,
                reqwest::Url::parse(&url).map_err(|e| e.to_string())?,
            );
            let response = self.http.send(request).await.map_err(|e| e.to_string())?;
            if response.status.is_redirection() {
                url = response
                    .headers
                    .get("location")
                    .and_then(|l| l.to_str().ok())
                    .ok_or("Gradle checksum redirect without a location")?
                    .to_string();
                continue;
            }
            if !response.status.is_success() {
                return Err(format!("Gradle checksum {url}: HTTP {}", response.status));
            }
            let sum = String::from_utf8_lossy(&response.body)
                .trim()
                .to_ascii_lowercase();
            if sum.len() != 64 || !sum.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("Gradle published an invalid checksum".into());
            }
            return Ok(sum);
        }
        Err("too many Gradle checksum redirects".into())
    }
    pub(super) async fn gradle_wrapper(&self, root: &Path, directory: &str) -> Result<(), String> {
        let path = root.join(directory).join("gradle-wrapper.properties");
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        if !text
            .lines()
            .any(|l| l.trim_start().starts_with("distributionSha256Sum"))
        {
            return Ok(());
        }
        let url = text
            .lines()
            .find_map(|l| l.trim().strip_prefix("distributionUrl="))
            .ok_or("gradle-wrapper.properties has no distributionUrl")?
            .replace("\\:", ":");
        let sum = self.checksum(&url).await?;
        let updated: String = text
            .split_inclusive('\n')
            .map(|line| {
                if line.trim_start().starts_with("distributionSha256Sum") {
                    let ending = &line[line.trim_end_matches(['\r', '\n']).len()..];
                    format!("distributionSha256Sum={sum}{ending}")
                } else {
                    line.to_string()
                }
            })
            .collect();
        std::fs::write(&path, updated).map_err(|e| e.to_string())
    }
}
