//! Validated pattern lists, compiled once outside dependency/release loops.
use globset::{GlobBuilder, GlobMatcher};
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
            let pattern = if let Some(v) = value.strip_prefix("regex:") {
                Pattern::Regex(Regex::new(v).map_err(|e| e.to_string())?)
            } else {
                Pattern::Glob(
                    GlobBuilder::new(value.strip_prefix("glob:").unwrap_or(value))
                        .literal_separator(true)
                        .build()
                        .map_err(|e| e.to_string())?
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
}
