use crate::{Body, failure};
use regex::Regex;
use reqwest::Url;
use rustovate_http::Error;
use serde_json::Value;

const MARKER: &str = "[REDACTED]";

#[derive(Clone)]
pub struct Redactor {
    patterns: Vec<Regex>,
    literals: Vec<String>,
}

impl Default for Redactor {
    fn default() -> Self {
        Self {
            patterns: vec![
                Regex::new(r"(?:gh[pso]_[A-Za-z0-9_]+|github_pat_[A-Za-z0-9_]+|v1\.[A-Za-z0-9_]+)")
                    .expect("static token regex"),
                Regex::new(r"(?i)(?:bearer|basic)\s+[A-Za-z0-9+/_.=-]+")
                    .expect("static authorization regex"),
            ],
            literals: Vec::new(),
        }
    }
}

impl Redactor {
    pub fn with_literals(mut self, values: impl IntoIterator<Item = String>) -> Self {
        self.literals
            .extend(values.into_iter().filter(|value| !value.is_empty()));
        self
    }

    pub fn with_pattern(mut self, pattern: &str) -> Result<Self, Error> {
        self.patterns
            .push(Regex::new(pattern).map_err(|_| failure("invalid redaction pattern"))?);
        Ok(self)
    }

    fn sensitive(name: &str) -> bool {
        let name = name.to_ascii_lowercase().replace('-', "_");
        [
            "authorization",
            "cookie",
            "token",
            "password",
            "secret",
            "credential",
            "api_key",
            "access_key",
            "signature",
        ]
        .iter()
        .any(|needle| name.contains(needle))
    }

    pub fn text(&self, text: &str) -> String {
        let urls = Regex::new(r#"https?://[^\s<>\"\\]+"#).expect("static URL regex");
        let text = urls.replace_all(text, |captures: &regex::Captures<'_>| {
            Url::parse(&captures[0])
                .map(|url| self.url(&url))
                .unwrap_or_else(|_| MARKER.into())
        });
        self.pattern_text(&text)
    }

    fn pattern_text(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for literal in &self.literals {
            text = text.replace(literal, MARKER);
        }
        for pattern in &self.patterns {
            text = pattern.replace_all(&text, MARKER).into_owned();
        }
        text
    }

    pub fn field(&self, name: &str, value: &str) -> String {
        if Self::sensitive(name) {
            MARKER.into()
        } else {
            self.text(value)
        }
    }

    pub fn url(&self, url: &Url) -> String {
        let mut result = url.clone();
        if !result.username().is_empty() {
            let _ = result.set_username("redacted");
        }
        if result.password().is_some() {
            let _ = result.set_password(Some("redacted"));
        }
        result.set_fragment(None);
        let pairs: Vec<_> = result
            .query_pairs()
            .map(|(name, value)| (name.to_string(), self.field(&name, &value)))
            .collect();
        if result.query().is_some() {
            result.query_pairs_mut().clear().extend_pairs(pairs);
        }
        self.pattern_text(result.as_str())
    }

    fn json(&self, value: &mut Value) {
        match value {
            Value::Object(fields) => {
                for (name, value) in fields {
                    if Self::sensitive(name) {
                        *value = Value::String(MARKER.into());
                    } else {
                        self.json(value);
                    }
                }
            }
            Value::Array(values) => {
                for value in values {
                    self.json(value);
                }
            }
            Value::String(text) => {
                *text = match Url::parse(text) {
                    Ok(url) if matches!(url.scheme(), "https" | "http") => self.url(&url),
                    _ => self.text(text),
                };
            }
            _ => {}
        }
    }

    pub(crate) fn body(&self, body: &mut Body) {
        match body {
            Body::Json(value) => self.json(value),
            Body::Text(text) => *text = self.text(text),
        }
    }

    /// Scan both raw bytes and parsed values, so JSON escaping cannot hide tokens.
    pub fn check(&self, bytes: &[u8]) -> Result<(), Error> {
        let text = String::from_utf8_lossy(bytes);
        if self.text(&text) != text {
            return Err(failure("possible secret in cassette"));
        }
        let mut json: Value =
            serde_json::from_slice(bytes).map_err(|_| failure("invalid cassette JSON"))?;
        let before = json.clone();
        // Header names occur in nested maps; replace entire sensitive values.
        self.json(&mut json);
        normalize_redacted_headers(&mut json);
        let mut before = before;
        normalize_redacted_headers(&mut before);
        if before != json {
            return Err(failure("possible secret in cassette fields or URL"));
        }
        Ok(())
    }
}

fn normalize_redacted_headers(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (name, value) in fields {
                if Redactor::sensitive(name)
                    && value
                        .as_array()
                        .is_some_and(|values| values.iter().all(|value| value == MARKER))
                {
                    *value = Value::String(MARKER.into());
                } else {
                    normalize_redacted_headers(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                normalize_redacted_headers(value);
            }
        }
        _ => {}
    }
}
