//! Operator registry credentials, matched to requests by URL prefix.
use reqwest::{Url, header::HeaderValue};

/// How a registry entry authenticates. Debug never shows the secret.
#[derive(Clone, PartialEq, Eq)]
pub enum Auth {
    Basic { username: String, password: String },
    Bearer { token: String },
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Basic { username, .. } => f
                .debug_struct("Basic")
                .field("username", username)
                .finish_non_exhaustive(),
            Self::Bearer { .. } => f.debug_struct("Bearer").finish_non_exhaustive(),
        }
    }
}

impl Auth {
    /// The `Authorization` value, marked sensitive so it is never logged.
    pub fn header(&self) -> Option<HeaderValue> {
        let text = match self {
            Self::Basic { username, password } => {
                use base64::Engine;
                format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD
                        .encode(format!("{username}:{password}"))
                )
            }
            Self::Bearer { token } => format!("Bearer {token}"),
        };
        let mut value = HeaderValue::from_str(&text).ok()?;
        value.set_sensitive(true);
        Some(value)
    }
}

/// One operator registry entry: requests under `prefix` carry `auth`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credential {
    id: String,
    prefix: Url,
    pub auth: Auth,
}

impl Credential {
    /// `prefix` must be an https URL without user, query or fragment. Its
    /// path matches whole segments: `/charts` covers `/charts/index.yaml`
    /// but not `/charts-old`.
    pub fn new(prefix: &str, auth: Auth) -> Option<Self> {
        let url = Url::parse(prefix).ok()?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return None;
        }
        let valid = match &auth {
            Auth::Basic { username, password } => {
                !username.is_empty() && !username.contains(':') && !password.is_empty()
            }
            Auth::Bearer { token } => !token.is_empty(),
        };
        (valid && auth.header().is_some()).then(|| Self {
            id: url.as_str().trim_end_matches('/').to_owned(),
            prefix: url,
            auth,
        })
    }
    /// The entry's name in cache keys: its normalised prefix, never the secret.
    pub fn id(&self) -> &str {
        &self.id
    }
    fn covers(&self, url: &Url) -> bool {
        if url.scheme() != self.prefix.scheme()
            || url.host_str() != self.prefix.host_str()
            || url.port_or_known_default() != self.prefix.port_or_known_default()
        {
            return false;
        }
        let base = self.prefix.path().trim_end_matches('/');
        url.path()
            .strip_prefix(base)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    }
    fn len(&self) -> usize {
        self.prefix.path().trim_end_matches('/').len()
    }
}

/// The entry for `url`: the one with the longest matching prefix.
pub(crate) fn matching<'a>(credentials: &'a [Credential], url: &Url) -> Option<&'a Credential> {
    credentials
        .iter()
        .filter(|credential| credential.covers(url))
        .max_by_key(|credential| credential.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bearer(prefix: &str) -> Credential {
        Credential::new(
            prefix,
            Auth::Bearer {
                token: "secret".into(),
            },
        )
        .unwrap()
    }

    #[test]
    fn prefixes_match_whole_segments_on_the_same_origin_and_the_longest_wins() {
        let entries = [
            bearer("https://acme.jfrog.io/artifactory"),
            bearer("https://acme.jfrog.io/artifactory/api/helm/charts/"),
        ];
        let id = |url: &str| matching(&entries, &Url::parse(url).unwrap()).map(Credential::id);
        assert_eq!(
            id("https://acme.jfrog.io/artifactory/api/helm/charts/index.yaml"),
            Some("https://acme.jfrog.io/artifactory/api/helm/charts")
        );
        assert_eq!(
            id("https://acme.jfrog.io/artifactory/api/npm/x"),
            Some("https://acme.jfrog.io/artifactory")
        );
        assert_eq!(
            id("https://acme.jfrog.io/artifactory"),
            Some("https://acme.jfrog.io/artifactory")
        );
        for other in [
            "https://acme.jfrog.io/artifactory-old/x",
            "https://acme.jfrog.io/",
            "https://evil.jfrog.io/artifactory/x",
            "https://acme.jfrog.io:8443/artifactory/x",
            "http://acme.jfrog.io/artifactory/x",
        ] {
            assert_eq!(id(other), None, "{other}");
        }
        let host = [bearer("https://registry-1.docker.io")];
        assert!(
            matching(
                &host,
                &Url::parse("https://registry-1.docker.io/v2/x").unwrap()
            )
            .is_some()
        );
    }

    #[test]
    fn invalid_entries_are_refused_and_secrets_stay_out_of_debug() {
        for prefix in [
            "http://acme.jfrog.io",
            "https://user@acme.jfrog.io",
            "https://acme.jfrog.io/?a=b",
            "https://acme.jfrog.io/#x",
            "acme.jfrog.io",
        ] {
            assert!(
                Credential::new(prefix, Auth::Bearer { token: "t".into() }).is_none(),
                "{prefix}"
            );
        }
        let basic = |username: &str, password: &str| {
            Credential::new(
                "https://acme.jfrog.io",
                Auth::Basic {
                    username: username.into(),
                    password: password.into(),
                },
            )
        };
        assert!(basic("", "p").is_none());
        assert!(basic("a:b", "p").is_none());
        assert!(basic("u", "").is_none());
        let entry = basic("ci", "hunter2").unwrap();
        assert!(!format!("{entry:?}").contains("hunter2"));
        assert!(entry.auth.header().unwrap().is_sensitive());
        let bearer = Credential::new(
            "https://acme.jfrog.io",
            Auth::Bearer {
                token: "bad\ntoken".into(),
            },
        );
        assert!(bearer.is_none());
    }
}
