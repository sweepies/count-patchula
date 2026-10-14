use crate::{Client, Error, Result};
use reqwest::header;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::HashSet;

#[derive(Clone, Debug)]
pub struct GraphqlPage<T> {
    pub nodes: Vec<T>,
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
}

impl Client {
    /// Stop is called on each full page before following Link. Returned results
    /// include that page, so callers can merge it with known-history caches.
    pub async fn rest_pages<T: DeserializeOwned>(
        &self,
        path: &str,
        mut stop: impl FnMut(&[T]) -> bool,
    ) -> Result<Vec<T>> {
        let mut next = Some(self.url(path)?);
        let mut visited = HashSet::new();
        let mut result = Vec::new();
        for _ in 0..self.0.options.max_pages {
            let Some(url) = next.take() else {
                return Ok(result);
            };
            if !visited.insert(url.clone()) {
                return Err(Error::Pagination);
            }
            let response = self.get(url.as_str()).await?;
            let page: Vec<T> = serde_json::from_slice(&response.body).map_err(|_| Error::Json)?;
            let done = stop(&page);
            result.extend(page);
            if done {
                return Ok(result);
            }
            // Link may occur in several fields. Split only outside angle/quoted
            // strings so commas in URIs or extension parameters are preserved.
            for link in response.headers.get_all(header::LINK) {
                for part in split_links(link.to_str().map_err(|_| Error::Pagination)?)? {
                    let Some((target, params)) = part
                        .trim()
                        .strip_prefix('<')
                        .and_then(|part| part.split_once('>'))
                    else {
                        return Err(Error::Pagination);
                    };
                    let is_next = params
                        .split(';')
                        .filter_map(|param| param.trim().split_once('='))
                        .any(|(name, value)| {
                            name.eq_ignore_ascii_case("rel")
                                && value
                                    .trim()
                                    .trim_matches('"')
                                    .split_whitespace()
                                    .any(|rel| rel == "next")
                        });
                    if is_next {
                        if next.is_some() {
                            return Err(Error::Pagination);
                        }
                        next = Some(
                            self.url(url.join(target).map_err(|_| Error::Pagination)?.as_str())?,
                        );
                    }
                }
            }
            if next.is_none() {
                return Ok(result);
            }
        }
        Err(Error::Pagination)
    }

    /// The query must accept $cursor; caller supplies the connection extractor
    /// because GitHub connections can be nested under any data path.
    pub async fn graphql_pages<T>(
        &self,
        query: &str,
        mut variables: Value,
        mut extract: impl FnMut(Value) -> Result<GraphqlPage<T>>,
        mut stop: impl FnMut(&[T]) -> bool,
    ) -> Result<Vec<T>> {
        let variables = variables.as_object_mut().ok_or(Error::Configuration)?;
        variables.insert("cursor".into(), Value::Null);
        let mut visited = HashSet::new();
        let mut result = Vec::new();
        for _ in 0..self.0.options.max_pages {
            let page = extract(
                self.graphql(query, Value::Object(variables.clone()))
                    .await?,
            )?;
            let done = stop(&page.nodes);
            result.extend(page.nodes);
            if done || !page.has_next_page {
                return Ok(result);
            }
            let cursor = page
                .end_cursor
                .filter(|cursor| !cursor.is_empty())
                .ok_or(Error::Pagination)?;
            if !visited.insert(cursor.clone()) {
                return Err(Error::Pagination);
            }
            variables.insert("cursor".into(), Value::String(cursor));
        }
        Err(Error::Pagination)
    }
}

impl<T: DeserializeOwned> GraphqlPage<T> {
    /// Parse a `nodes` + `pageInfo` connection selected with a JSON pointer.
    pub fn from_connection(data: &Value, pointer: &str) -> Result<Self> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Info {
            has_next_page: bool,
            end_cursor: Option<String>,
        }
        let connection = data.pointer(pointer).ok_or(Error::Pagination)?;
        let nodes =
            serde_json::from_value(connection.get("nodes").ok_or(Error::Pagination)?.clone())
                .map_err(|_| Error::Json)?;
        let info: Info =
            serde_json::from_value(connection.get("pageInfo").ok_or(Error::Pagination)?.clone())
                .map_err(|_| Error::Pagination)?;
        Ok(Self {
            nodes,
            has_next_page: info.has_next_page,
            end_cursor: info.end_cursor,
        })
    }
}

fn split_links(value: &str) -> Result<Vec<&str>> {
    let (mut angle, mut quoted, mut escaped, mut start) = (false, false, false, 0);
    let mut parts = Vec::new();
    for (index, character) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' if quoted => escaped = true,
            '"' if !angle => quoted = !quoted,
            '<' if !quoted => angle = true,
            '>' if !quoted => angle = false,
            ',' if !angle && !quoted => {
                parts.push(&value[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    if angle || quoted || escaped {
        return Err(Error::Pagination);
    }
    parts.push(&value[start..]);
    Ok(parts)
}
