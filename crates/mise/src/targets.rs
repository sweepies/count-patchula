//! Hosts a tool's options may send mise to. Any host is followed, as the
//! repository declares it, apart from the cloud instance-metadata
//! endpoints: those hand out the credentials of the machine the Count runs
//! on, and what mise fetches becomes the versions in the repository's pull
//! request.
use crate::Error;
use std::net::{IpAddr, Ipv6Addr};
use toml_edit::Value;
use url::{Host, Url};

/// Names that reach a metadata endpoint without an address in them,
/// refused before any lookup: a resolver with search domains can answer
/// them with another address, while the name mise fetches reaches the
/// endpoint.
const METADATA_NAMES: &[&str] = &["metadata.google.internal", "metadata"];
/// AWS's IPv6 instance-metadata address.
const AWS_IPV6_METADATA: Ipv6Addr = Ipv6Addr::new(0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254);

/// Whether `ip` is link-local (169.254.0.0/16, which holds the AWS, GCP,
/// Azure and ECS metadata endpoints, or fe80::/10) or AWS's IPv6 metadata
/// address, IPv4-mapped forms included.
fn metadata_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => {
            v6.to_ipv4_mapped().is_some_and(|v4| v4.is_link_local())
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || v6 == AWS_IPV6_METADATA
        }
    }
}

/// The URLs among a value's strings, nested ones included, that mise could
/// fetch: http and https, which every backend's fetches use.
fn urls(value: &Value, out: &mut Vec<Url>) {
    match value {
        Value::String(s) => {
            if let Ok(url) = Url::parse(s.value())
                && matches!(url.scheme(), "http" | "https" | "ws" | "wss")
            {
                out.push(url);
            }
        }
        Value::Array(values) => values.iter().for_each(|v| urls(v, out)),
        Value::InlineTable(table) => table.iter().for_each(|(_, v)| urls(v, out)),
        _ => {}
    }
}

/// Refuses `tool` when an option names an instance-metadata endpoint,
/// checking the addresses each host name resolves to now. mise resolves
/// the name again when it fetches and follows redirects, so this does not
/// stop a host that changes its answer in between or redirects there; the
/// machine's own network policy is what closes those.
pub(crate) async fn check(tool: &str) -> Result<(), Error> {
    let Some(options) = crate::policy::options(tool) else {
        return Ok(());
    };
    let mut found = Vec::new();
    options.iter().for_each(|(_, v)| urls(v, &mut found));
    let refuse = |why: String| {
        Err(Error::Refused(format!(
            "tool `{tool}` is not looked up: {why}"
        )))
    };
    const WHY: &str = "an option points mise at a cloud instance-metadata endpoint, \
                       which hands out the credentials of the machine the Count runs on";
    for url in found {
        let port = url.port_or_known_default().unwrap_or(80);
        let addresses: Vec<IpAddr> = match url.host() {
            None => continue,
            Some(Host::Ipv4(v4)) => vec![IpAddr::V4(v4)],
            Some(Host::Ipv6(v6)) => vec![IpAddr::V6(v6)],
            Some(Host::Domain(name)) => {
                let bare = name.trim_end_matches('.').to_ascii_lowercase();
                if METADATA_NAMES.contains(&bare.as_str()) {
                    return refuse(WHY.into());
                }
                // The name exactly as mise requests it: a trailing dot makes
                // it absolute, and stripping it would let search domains
                // answer for a name mise resolves without them.
                match tokio::net::lookup_host((name, port)).await {
                    Ok(found) => found.map(|address| address.ip()).collect(),
                    Err(_) => {
                        return refuse(format!(
                            "an option names the host `{name}`, which does not resolve, so the \
                             Count cannot check that it is not a cloud instance-metadata endpoint"
                        ));
                    }
                }
            }
        };
        if addresses.into_iter().any(metadata_address) {
            return refuse(WHY.into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_local_and_metadata_addresses() {
        for ip in [
            "169.254.169.254",
            "169.254.170.2",
            "169.254.0.1",
            "fe80::1",
            "febf::1",
            "fd00:ec2::254",
            "::ffff:169.254.169.254",
        ] {
            assert!(metadata_address(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "10.0.0.1",
            "192.168.1.1",
            "127.0.0.1",
            "8.8.8.8",
            "::1",
            "fd00:ec2::253",
            "fec0::1",
            "2001:db8::1",
        ] {
            assert!(!metadata_address(ip.parse().unwrap()), "{ip}");
        }
    }

    #[tokio::test]
    async fn metadata_endpoints_are_refused_in_any_spelling() {
        for tool in [
            r#"http:t[version_list_url="http://169.254.169.254/latest/meta-data/"]"#,
            r#"http:t[version_list_url="http://2852039166/"]"#,
            r#"http:t[version_list_url="http://0xa9.0xfe.0xa9.0xfe/"]"#,
            r#"http:t[version_list_url="http://[fd00:ec2::254]/"]"#,
            r#"s3:t[endpoint="http://169.254.170.2/v2/credentials"]"#,
            r#"github:o/r[api_url="http://Metadata.Google.Internal./computeMetadata/v1"]"#,
            r#"http:t[platforms={linux-x64={version_list_url="http://metadata/"}}]"#,
            r#"http:t[version_list_url="http://metadata.google.internal./computeMetadata/v1"]"#,
            r#"http:t[version_list_url="http://metadata./"]"#,
        ] {
            let error = check(tool).await.unwrap_err().to_string();
            assert!(
                error.contains("points mise at a cloud instance-metadata endpoint"),
                "{tool}: {error}"
            );
        }
    }

    #[tokio::test]
    async fn a_host_that_does_not_resolve_is_refused() {
        let error = check(r#"http:t[version_list_url="http://nothing.invalid/"]"#)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("does not resolve"), "{error}");
    }

    #[tokio::test]
    async fn other_hosts_and_values_pass() {
        for tool in [
            r#"http:t[version_list_url="http://127.0.0.1:8080/versions"]"#,
            r#"http:t[version_list_url="http://10.1.2.3/versions"]"#,
            r#"s3:t[version_list_url="s3://bucket/versions.json"]"#,
            r#"rust[components="clippy,rustfmt",profile="minimal"]"#,
            "github:github/gh-actions-lock[bin=gh-actions-lock]",
            "jq",
        ] {
            assert!(check(tool).await.is_ok(), "{tool}");
        }
    }
}
