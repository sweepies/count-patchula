use crate::{Client, Error, Response, Result};
use reqwest::{Url, header};
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
        self.rest_pages_with(path, |response| {
            let page: Vec<T> = serde_json::from_slice(&response.body).map_err(|_| Error::Json)?;
            let done = stop(&page);
            Ok((page, done))
        })
        .await
    }

    /// `rest_pages` for an endpoint whose page wraps its array in an object,
    /// such as `installation/repositories`' `{"repositories": [...]}`:
    /// `entries` pulls one page's items out of the decoded body.
    pub async fn rest_pages_envelope<T>(
        &self,
        path: &str,
        mut entries: impl FnMut(&Value) -> Result<Vec<T>>,
        mut stop: impl FnMut(&[T]) -> bool,
    ) -> Result<Vec<T>> {
        self.rest_pages_with(path, |response| {
            let body: Value = serde_json::from_slice(&response.body).map_err(|_| Error::Json)?;
            let page = entries(&body)?;
            let done = stop(&page);
            Ok((page, done))
        })
        .await
    }

    /// The page loop `rest_pages` and `rest_pages_envelope` share: `page`
    /// decodes one response into its items and says whether to stop.
    async fn rest_pages_with<T>(
        &self,
        path: &str,
        mut page: impl FnMut(&Response) -> Result<(Vec<T>, bool)>,
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
            let (entries, done) = page(&response)?;
            result.extend(entries);
            if done {
                return Ok(result);
            }
            if let Some(target) = next_link(&response, &url)? {
                next = Some(self.url(target.as_str())?);
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

/// The `rel="next"` target of a REST page's Link headers, resolved against
/// the page URL. Errors on malformed links or more than one next.
pub fn next_link(response: &Response, url: &Url) -> Result<Option<Url>> {
    let mut next = None;
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
                next = Some(url.join(target).map_err(|_| Error::Pagination)?);
            }
        }
    }
    Ok(next)
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

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::{StatusCode, header::HeaderValue};

    fn response(links: &[&'static str]) -> Response {
        let mut headers = header::HeaderMap::new();
        for link in links {
            headers.append(header::LINK, HeaderValue::from_static(link));
        }
        Response {
            status: StatusCode::OK,
            headers,
            body: Vec::new(),
            stale: None,
        }
    }

    #[test]
    fn two_next_links_are_a_pagination_error() {
        let url = Url::parse("https://api.github.com/app/installations?per_page=100").unwrap();
        let response = response(&[
            "<https://api.github.com/app/installations?per_page=100&page=2>; rel=\"next\"",
            "<https://api.github.com/app/installations?per_page=100&page=3>; rel=\"next\"",
        ]);
        assert!(matches!(next_link(&response, &url), Err(Error::Pagination)));
    }
}
