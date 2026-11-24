//! Config patterns, filters and regexes, compiled as the config is read so
//! nothing compiles them again and none can fail later.
use globset::{GlobBuilder, GlobMatcher};
use patchula_versioning::Ecosystem;
use regex::Regex;
use serde::{Deserialize, Serialize};

/// A path or name pattern from the config, compiled as it is read: `!`
/// negates, `regex:` is a regular expression, and anything else is a glob
/// (`glob:` optional). It keeps its text, which is how it serializes.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Pattern {
    text: String,
    negated: bool,
    matcher: Matcher,
}
#[derive(Clone, Debug)]
enum Matcher {
    Glob(GlobMatcher),
    Regex(Regex),
}
impl TryFrom<String> for Pattern {
    type Error = String;
    fn try_from(text: String) -> Result<Self, String> {
        let (negated, value) = text
            .strip_prefix('!')
            .map_or((false, text.as_str()), |v| (true, v));
        if value.is_empty() {
            return Err("pattern must not be empty".into());
        }
        let matcher = if let Some(v) = value.strip_prefix("regex:") {
            Matcher::Regex(Regex::new(v).map_err(|e| format!("invalid regex: {e}"))?)
        } else {
            let glob = value.strip_prefix("glob:").unwrap_or(value);
            if glob.is_empty() {
                return Err("glob must not be empty".into());
            }
            Matcher::Glob(
                GlobBuilder::new(glob)
                    .literal_separator(true)
                    .build()
                    .map_err(|e| format!("invalid glob: {e}"))?
                    .compile_matcher(),
            )
        };
        Ok(Self {
            text,
            negated,
            matcher,
        })
    }
}
impl From<Pattern> for String {
    fn from(pattern: Pattern) -> Self {
        pattern.text
    }
}
impl Pattern {
    /// The pattern as written.
    pub fn as_str(&self) -> &str {
        &self.text
    }
    /// Whether the pattern, ignoring its `!`, matches `text`.
    fn hits(&self, text: &str) -> bool {
        match &self.matcher {
            Matcher::Glob(p) => p.is_match(text),
            Matcher::Regex(p) => p.is_match(text),
        }
    }
    /// Whether a package known by two names passes this one pattern:
    /// either name matching counts, and a negated pattern excludes both.
    pub fn admits_any(&self, left: &str, right: &str) -> bool {
        (self.hits(left) || self.hits(right)) != self.negated
    }
}
/// Patterns are told apart, and ordered, by their text.
impl PartialEq for Pattern {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
    }
}
impl Eq for Pattern {}
impl PartialOrd for Pattern {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Pattern {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.text.cmp(&other.text)
    }
}

/// A list of patterns: any positive one admits, and a negated one always
/// excludes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Patterns(Vec<Pattern>);
impl Patterns {
    /// Patterns from text that is not part of a config, such as a flag.
    pub fn compile(values: &[String]) -> Result<Self, String> {
        values
            .iter()
            .map(|value| Pattern::try_from(value.clone()))
            .collect::<Result<_, _>>()
            .map(Self)
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn iter(&self) -> std::slice::Iter<'_, Pattern> {
        self.0.iter()
    }
    fn positive(&self) -> impl Iterator<Item = &Pattern> {
        self.0.iter().filter(|p| !p.negated)
    }
    fn negative(&self) -> impl Iterator<Item = &Pattern> {
        self.0.iter().filter(|p| p.negated)
    }
    /// Empty positive list means any; exclusions always win.
    pub fn matches(&self, text: &str) -> bool {
        let mut positive = self.positive().peekable();
        (positive.peek().is_none() || positive.any(|p| p.hits(text)))
            && !self.negative().any(|p| p.hits(text))
    }
    /// Evaluate aliases as one identity, so a negated exception on either
    /// registry package or manifest name wins over a positive match.
    pub fn matches_any(&self, left: &str, right: &str) -> bool {
        let mut positive = self.positive().peekable();
        (positive.peek().is_none() || positive.any(|p| p.hits(left) || p.hits(right)))
            && !self.negative().any(|p| p.hits(left) || p.hits(right))
    }
}

/// A regular expression from the config, compiled as it is read. It keeps
/// its text, which is how it serializes.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ConfigRegex(Regex);
impl TryFrom<String> for ConfigRegex {
    type Error = String;
    fn try_from(text: String) -> Result<Self, String> {
        Regex::new(&text)
            .map(Self)
            .map_err(|e| format!("invalid regex: {e}"))
    }
}
impl From<ConfigRegex> for String {
    fn from(regex: ConfigRegex) -> Self {
        regex.0.as_str().to_owned()
    }
}
impl std::ops::Deref for ConfigRegex {
    type Target = Regex;
    fn deref(&self) -> &Regex {
        &self.0
    }
}

/// A version filter from `select.current` or a `permitted` field: `!` negates,
/// `regex:` matches the text, and `semver:` matches a version in the
/// dependency's own versioning. Compiled as the config is read, it keeps
/// its text, which is how it serializes.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Filter {
    text: String,
    negated: bool,
    kind: FilterKind,
}
#[derive(Clone, Debug)]
enum FilterKind {
    Regex(Regex),
    /// The range text, evaluated per dependency because its syntax belongs to
    /// that dependency's ecosystem.
    Semver(String),
}
impl TryFrom<String> for Filter {
    type Error = String;
    fn try_from(text: String) -> Result<Self, String> {
        let (negated, value) = text
            .strip_prefix('!')
            .map_or((false, text.as_str()), |v| (true, v));
        let kind = if let Some(value) = value.strip_prefix("regex:") {
            FilterKind::Regex(Regex::new(value).map_err(|e| format!("invalid regex: {e}"))?)
        } else if let Some(value) = value.strip_prefix("semver:") {
            if value.is_empty() {
                return Err("semver: filter must not be empty".into());
            }
            FilterKind::Semver(value.to_owned())
        } else {
            return Err("expected regex:... or semver:... version filter".into());
        };
        Ok(Self {
            text,
            negated,
            kind,
        })
    }
}
impl From<Filter> for String {
    fn from(filter: Filter) -> Self {
        filter.text
    }
}
impl std::str::FromStr for Filter {
    type Err = String;
    fn from_str(text: &str) -> Result<Self, String> {
        Self::try_from(text.to_owned())
    }
}
impl Filter {
    /// The filter as written.
    pub fn as_str(&self) -> &str {
        &self.text
    }
    /// Whether `text` — a version in `scheme`'s syntax — satisfies the filter.
    /// A `semver:` filter on an ecosystem without range support is a
    /// configuration error, not a non-match.
    pub fn matches(&self, scheme: Ecosystem, text: &str) -> Result<bool, String> {
        let yes = match &self.kind {
            FilterKind::Regex(regex) => regex.is_match(text),
            FilterKind::Semver(range) => {
                if !scheme.supports_ranges() {
                    return Err(format!(
                        "a semver: filter needs an ecosystem with range support; {scheme:?} compares exact versions only"
                    ));
                }
                match scheme.parse(text) {
                    Ok(version) => scheme.matches(range, &version).map_err(|e| e.to_string())?,
                    // A value that is not a version (a range, a tag) does not
                    // match, but an invalid requirement is still an error.
                    Err(_) => {
                        scheme.check_range(range).map_err(|e| e.to_string())?;
                        false
                    }
                }
            }
        };
        Ok(yes != self.negated)
    }
    /// Whether the filter, which the prerelease `text` already satisfies,
    /// deliberately admits it; `release` gives `text` without its
    /// prerelease part, and is asked for only when a `regex:` filter needs
    /// it. A negated filter only excludes, so it never asks. A `regex:`
    /// filter asks when it does not match the release, so the match comes
    /// from the prerelease part (`regex:-rc\.`, `regex:nightly`). A
    /// `semver:` range asks when it names a prerelease version itself
    /// (`semver:>=3.0.0-rc.1`, `semver:[1.0-beta1,2)`).
    pub fn asks_for_prerelease(&self, scheme: Ecosystem, release: impl FnOnce() -> String) -> bool {
        if self.negated {
            return false;
        }
        match &self.kind {
            FilterKind::Regex(regex) => !regex.is_match(&release()),
            FilterKind::Semver(range) => names_prerelease(scheme, range),
        }
    }
}
/// Whether a range's text names a prerelease version in `scheme`'s syntax.
fn names_prerelease(scheme: Ecosystem, range: &str) -> bool {
    range
        .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+')))
        .filter(|token| !token.is_empty())
        .any(|token| {
            scheme
                .parse(token)
                .is_ok_and(|version| !version.0.pre.is_empty())
        })
}
