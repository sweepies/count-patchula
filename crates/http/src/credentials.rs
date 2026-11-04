//! Operator registry credentials, matched to requests by URL prefix.
use reqwest::{Url, header::HeaderValue};

/// How a registry entry authenticates. Debug never shows the secret.
#[derive(Clone, PartialEq, Eq)]
pub enum Auth {
    Basic {
        username: String,
        password: String,
    },
    Bearer {
        token: String,
    },
    /// An entry whose secret could not be read: its prefix still claims the
    /// requests under it, which then carry nothing, so a broader entry's
    /// credentials never stand in for it.
    Withheld,
    /// An entry without credentials, such as an open mirror.
    Anonymous,
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Basic { username, .. } => f
                .debug_struct("Basic")
                .field("username", username)
                .finish_non_exhaustive(),
            Self::Bearer { .. } => f.debug_struct("Bearer").finish_non_exhaustive(),
            Self::Withheld => f.write_str("Withheld"),
            Self::Anonymous => f.write_str("Anonymous"),
        }
    }
}

/// RFC 6750 `b64token`: what a bearer token may contain.
fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "-._~+/=".contains(c)
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
            Self::Withheld | Self::Anonymous => return None,
        };
        let mut value = HeaderValue::from_str(&text).ok()?;
        value.set_sensitive(true);
        Some(value)
    }
    /// Whether requests carry credentials.
    pub fn sends(&self) -> bool {
        matches!(self, Self::Basic { .. } | Self::Bearer { .. })
    }
    fn valid(&self) -> bool {
        match self {
            Self::Basic { username, password } => {
                !username.is_empty()
                    && !username.contains(':')
                    && !password.is_empty()
                    && self.header().is_some()
            }
            Self::Bearer { token } => !token.is_empty() && token.chars().all(token_char),
            Self::Withheld | Self::Anonymous => true,
        }
    }
    /// A one-way digest of who this authenticates as, so a changed account or
    /// secret gets its own cache entries.
    fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        let identity = match self {
            Self::Basic { username, password } => format!("basic\0{username}\0{password}"),
            Self::Bearer { token } => format!("bearer\0{token}"),
            Self::Withheld => return "withheld".into(),
            Self::Anonymous => return "anonymous".into(),
        };
        Sha256::digest(identity.as_bytes())
            .iter()
            .take(8)
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

/// The normalised form of an entry's prefix, or None if it is not a usable
/// https URL prefix.
pub fn prefix(prefix: &str) -> Option<String> {
    let url = Url::parse(prefix).ok()?;
    (url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none())
    .then(|| url.as_str().trim_end_matches('/').to_owned())
}

/// One operator registry entry: requests under `prefix` carry `auth`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credential {
    prefix: String,
    scope: String,
    url: Url,
    pub auth: Auth,
    /// The public registry prefix this entry stands in for, if it is a mirror.
    mirror_of: Option<(String, Url)>,
    /// Whether lockfile tools get these credentials too.
    tools: bool,
}

/// Whether `prefix` covers `url`: same origin, and the path continues the
/// prefix's by whole segments.
fn covers(prefix: &Url, url: &Url) -> bool {
    if url.scheme() != prefix.scheme()
        || url.host_str() != prefix.host_str()
        || url.port_or_known_default() != prefix.port_or_known_default()
    {
        return false;
    }
    let base = prefix.path().trim_end_matches('/');
    url.path()
        .strip_prefix(base)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

impl Credential {
    /// `prefix` must be an https URL without user, query or fragment. Its
    /// path matches whole segments: `/charts` covers `/charts/index.yaml`
    /// but not `/charts-old`.
    pub fn new(prefix: &str, auth: Auth) -> Option<Self> {
        let normalised = self::prefix(prefix)?;
        let url = Url::parse(&normalised).ok()?;
        auth.valid().then(|| Self {
            scope: format!("{normalised}#{}", auth.fingerprint()),
            url,
            prefix: normalised,
            auth,
            mirror_of: None,
            tools: false,
        })
    }
    /// Let lockfile tools use the entry's credentials too.
    pub fn for_tools(mut self) -> Self {
        self.tools = true;
        self
    }
    /// Whether lockfile tools get the entry's credentials.
    pub fn tools(&self) -> bool {
        self.tools
    }
    /// Make the entry a mirror of the public registry under `public`: requests
    /// there are sent to this entry's prefix instead. None if `public` is not
    /// a usable prefix or is the entry's own.
    pub fn mirroring(mut self, public: &str) -> Option<Self> {
        let normalised = self::prefix(public)?;
        let url = Url::parse(&normalised).ok()?;
        if normalised == self.prefix {
            return None;
        }
        self.mirror_of = Some((normalised, url));
        Some(self)
    }
    /// The public registry prefix this entry mirrors.
    pub fn mirror_of(&self) -> Option<&str> {
        self.mirror_of.as_ref().map(|(prefix, _)| prefix.as_str())
    }
    /// The entry's normalised prefix, for messages.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }
    /// The entry's name in cache keys: its prefix and a one-way digest of its
    /// credentials, never the secret.
    pub fn scope(&self) -> &str {
        &self.scope
    }
    pub(crate) fn covers(&self, url: &Url) -> bool {
        covers(&self.url, url)
    }
    /// Whether `other`'s prefix lies within this entry's, or is the same.
    pub fn encloses(&self, other: &Credential) -> bool {
        covers(&self.url, &other.url)
    }
    /// The host the prefix names, and its port when it is not https's own.
    pub fn host(&self) -> (&str, Option<u16>) {
        (self.url.host_str().unwrap_or_default(), self.url.port())
    }
    fn len(&self) -> usize {
        self.url.path().trim_end_matches('/').len()
    }
}

/// Where a request for `url` goes: through the mirror whose public prefix
/// covers it, the longest if several do, or None to go as it is. A mirror's
/// own URL is never rewritten again.
pub(crate) fn mirrored(credentials: &[Credential], url: &Url) -> Option<Url> {
    let (entry, public) = credentials
        .iter()
        .filter_map(|entry| Some((entry, entry.mirror_of.as_ref()?)))
        .filter(|(_, (_, public))| covers(public, url))
        .max_by_key(|(_, (_, public))| public.path().trim_end_matches('/').len())?;
    let rest = url.as_str().strip_prefix(public.0.as_str())?;
    Url::parse(&format!("{}{rest}", entry.prefix)).ok()
}

/// The entry a request for `url` would use: the one with the longest
/// matching prefix, mirrors and entries without credentials included.
pub fn entry_for<'a>(credentials: &'a [Credential], url: &str) -> Option<&'a Credential> {
    matching(credentials, &Url::parse(url).ok()?)
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
        let id = |url: &str| matching(&entries, &Url::parse(url).unwrap()).map(Credential::prefix);
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
    fn invalid_entries_are_refused_and_secrets_stay_out_of_debug_and_scopes() {
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
        assert!(!entry.scope().contains("hunter2"));
        assert!(entry.auth.header().unwrap().is_sensitive());
        for token in ["bad\ntoken", "two words", "quote\"d", ""] {
            assert!(
                Credential::new(
                    "https://acme.jfrog.io",
                    Auth::Bearer {
                        token: token.into()
                    }
                )
                .is_none(),
                "{token:?}"
            );
        }
        // Another secret or account for the same prefix gets its own scope.
        let rotated = basic("ci", "hunter3").unwrap();
        assert_eq!(rotated.prefix(), entry.prefix());
        assert_ne!(rotated.scope(), entry.scope());
        let withheld = Credential::new("https://acme.jfrog.io", Auth::Withheld).unwrap();
        assert!(withheld.auth.header().is_none());
        assert_ne!(withheld.scope(), entry.scope());
    }

    #[test]
    fn mirrors_rewrite_whole_segments_of_their_public_prefix_once() {
        let npm = Credential::new(
            "https://acme.jfrog.io/artifactory/api/npm/npm-remote/",
            Auth::Anonymous,
        )
        .unwrap()
        .mirroring("https://registry.npmjs.org/")
        .unwrap();
        let simple = bearer("https://acme.jfrog.io/pypi")
            .mirroring("https://pypi.org/simple")
            .unwrap();
        let entries = [npm.clone(), simple];
        let to =
            |url: &str| mirrored(&entries, &Url::parse(url).unwrap()).map(|url| url.to_string());
        assert_eq!(
            to("https://registry.npmjs.org/@scope%2fpkg"),
            Some("https://acme.jfrog.io/artifactory/api/npm/npm-remote/@scope%2fpkg".into())
        );
        assert_eq!(
            to("https://pypi.org/simple/requests/"),
            Some("https://acme.jfrog.io/pypi/requests/".into())
        );
        for unmirrored in [
            "https://pypi.org/pypi/requests/json",
            "https://registry.npmjs.org.evil.dev/x",
            "https://acme.jfrog.io/artifactory/api/npm/npm-remote/react",
        ] {
            assert_eq!(to(unmirrored), None, "{unmirrored}");
        }
        assert_eq!(npm.mirror_of(), Some("https://registry.npmjs.org"));
        assert!(!npm.auth.sends());
        assert!(
            Credential::new("https://acme.jfrog.io", Auth::Anonymous)
                .unwrap()
                .mirroring("https://acme.jfrog.io/")
                .is_none()
        );
    }
}
