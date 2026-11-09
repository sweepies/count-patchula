//! Validated pattern lists, compiled once outside dependency/release loops.
use globset::{GlobBuilder, GlobMatcher};
use patchula_versioning::Ecosystem;
use regex::Regex;

enum Pattern {
    Glob(GlobMatcher),
    Regex(Regex),
}
impl Pattern {
    fn matches(&self, text: &str) -> bool {
        match self {
            Self::Glob(p) => p.is_match(text),
            Self::Regex(p) => p.is_match(text),
        }
    }
}
pub struct Patterns {
    positive: Vec<Pattern>,
    negative: Vec<Pattern>,
}
impl Patterns {
    pub fn compile(values: &[String]) -> Result<Self, String> {
        let mut result = Self {
            positive: Vec::new(),
            negative: Vec::new(),
        };
        for value in values {
            let (negative, value) = value
                .strip_prefix('!')
                .map_or((false, value.as_str()), |v| (true, v));
            if value.is_empty() {
                return Err("pattern must not be empty".into());
            }
            let pattern = if let Some(v) = value.strip_prefix("regex:") {
                Pattern::Regex(Regex::new(v).map_err(|e| format!("invalid regex: {e}"))?)
            } else {
                let glob = value.strip_prefix("glob:").unwrap_or(value);
                if glob.is_empty() {
                    return Err("glob must not be empty".into());
                }
                Pattern::Glob(
                    GlobBuilder::new(glob)
                        .literal_separator(true)
                        .build()
                        .map_err(|e| format!("invalid glob: {e}"))?
                        .compile_matcher(),
                )
            };
            if negative {
                result.negative.push(pattern);
            } else {
                result.positive.push(pattern);
            }
        }
        Ok(result)
    }
    /// Empty positive list means any; exclusions always win.
    pub fn matches(&self, text: &str) -> bool {
        (self.positive.is_empty() || self.positive.iter().any(|p| p.matches(text)))
            && !self.negative.iter().any(|p| p.matches(text))
    }
    /// Evaluate aliases as one identity, so a negated exception on either
    /// registry package or manifest name wins over a positive match.
    pub fn matches_any(&self, left: &str, right: &str) -> bool {
        (self.positive.is_empty()
            || self
                .positive
                .iter()
                .any(|p| p.matches(left) || p.matches(right)))
            && !self
                .negative
                .iter()
                .any(|p| p.matches(left) || p.matches(right))
    }
}

/// A version filter from `select.current` or a `permitted` field: `!` negates,
/// `regex:` matches the text, and `semver:` matches a version in the
/// dependency's own versioning.
pub struct Filter {
    negated: bool,
    kind: FilterKind,
}
enum FilterKind {
    Regex(Regex),
    /// The range text, evaluated per dependency because its syntax belongs to
    /// that dependency's ecosystem.
    Semver(String),
}
impl Filter {
    pub fn compile(text: &str) -> Result<Self, String> {
        let (negated, text) = text.strip_prefix('!').map_or((false, text), |v| (true, v));
        let kind = if let Some(value) = text.strip_prefix("regex:") {
            FilterKind::Regex(Regex::new(value).map_err(|e| format!("invalid regex: {e}"))?)
        } else if let Some(value) = text.strip_prefix("semver:") {
            if value.is_empty() {
                return Err("semver: filter must not be empty".into());
            }
            FilterKind::Semver(value.to_owned())
        } else {
            return Err("expected regex:... or semver:... version filter".into());
        };
        Ok(Self { negated, kind })
    }
    /// Whether `text` — a version in `scheme`'s syntax — satisfies the filter.
    /// A `semver:` filter on an ecosystem without range support is a
    /// configuration error, not a non-match.
    pub fn matches(&self, scheme: Ecosystem, text: &str) -> Result<bool, String> {
        use patchula_versioning::Versioning;
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
                    Err(_) => false,
                }
            }
        };
        Ok(yes != self.negated)
    }
}
