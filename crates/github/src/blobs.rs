//! Git blobs by SHA: from the cache when it holds them, else a large blob as
//! REST's raw bytes and the rest through GraphQL in batches bounded by count
//! and size, else through REST's JSON. A blob read raw or through GraphQL
//! counts only when its bytes hash to the SHA asked for, so a binary,
//! truncated or re-encoded answer is never taken for the blob. Either way the
//! blob is kept in the cache as its REST representation, so a later read
//! needs no request. A cached blob counts only when it hashes to its SHA too:
//! the cache is a file a run restores, and its scope names a reach, not a
//! secret, so an entry planted under it is dropped and the blob read from
//! GitHub again.
use crate::{Client, Error, MAX_REDIRECTS, Read, Result, SENT};
use base64::Engine;
use futures_util::{StreamExt, stream};
use patchula_http::{Response, conditional};
use reqwest::{
    Method, StatusCode,
    header::{self, HeaderMap, HeaderValue},
};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    future::Future,
    pin::Pin,
};

/// Blobs one GraphQL query reads.
const BATCH: usize = 50;
/// The known bytes one GraphQL query reads. GraphQL escapes text into JSON,
/// so one big answer costs more than several smaller ones read side by side.
const MAX_GRAPHQL_BATCH_BYTES: u64 = 512 * 1024;
/// Blobs at least this large are read as REST's raw bytes, never through
/// GraphQL.
const LARGE_BLOB_BYTES: u64 = 256 * 1024;
/// The media type REST answers a blob's bytes in.
const RAW: &str = "application/vnd.github.raw+json";

fn path(repo: &str, sha: &str) -> String {
    format!("repos/{repo}/git/blobs/{sha}")
}

/// Whether `sha` is a Git object ID as GraphQL takes it: forty lowercase hex
/// digits.
fn object_id(sha: &str) -> bool {
    sha.len() == 40 && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The Git blob SHA of `content`.
pub fn blob_sha(content: &[u8]) -> String {
    let mut digest = Sha1::new();
    digest.update(format!("blob {}\0", content.len()));
    digest.update(content);
    crate::hex(&digest.finalize())
}

/// The content of a REST blob representation.
fn decode(body: &[u8]) -> Result<Vec<u8>> {
    let value: Value = serde_json::from_slice(body).map_err(|_| Error::Json)?;
    if value["encoding"] != "base64" {
        return Err(Error::Json);
    }
    let content = value["content"].as_str().ok_or(Error::Json)?;
    base64::engine::general_purpose::STANDARD
        .decode(content.replace(['\n', '\r'], ""))
        .map_err(|_| Error::Json)
}

/// GraphQL batches of `blobs` (sorted, each with its known size): at most
/// [`BATCH`] blobs and [`MAX_GRAPHQL_BATCH_BYTES`] known bytes each, and at
/// least one blob, so none is left out.
fn graphql_chunks(blobs: &[(String, Option<u64>)]) -> Vec<Vec<String>> {
    let mut chunks: Vec<Vec<String>> = Vec::new();
    let mut bytes: u64 = 0;
    for (sha, size) in blobs {
        let size = size.unwrap_or(0);
        match chunks.last_mut() {
            Some(chunk)
                if chunk.len() < BATCH && bytes.saturating_add(size) <= MAX_GRAPHQL_BATCH_BYTES =>
            {
                chunk.push(sha.clone());
                bytes += size;
            }
            _ => {
                chunks.push(vec![sha.clone()]);
                bytes = size;
            }
        }
    }
    chunks
}

impl Client {
    /// The content of each blob in `shas` from `repo` (`owner/name`).
    pub async fn blobs(&self, repo: &str, shas: &[String]) -> Result<BTreeMap<String, Vec<u8>>> {
        let items: Vec<_> = shas.iter().map(|sha| (sha.clone(), None)).collect();
        self.blobs_sized(repo, &items).await
    }

    /// The content of each blob in `items` from `repo` (`owner/name`), each
    /// with its size when the tree listing gave it. A blob of at least
    /// [`LARGE_BLOB_BYTES`] is read as raw bytes; the rest go through
    /// GraphQL in batches its size bounds, side by side with the raw reads.
    pub async fn blobs_sized(
        &self,
        repo: &str,
        items: &[(String, Option<u64>)],
    ) -> Result<BTreeMap<String, Vec<u8>>> {
        let mut sizes = BTreeMap::<String, Option<u64>>::new();
        for (sha, size) in items {
            let known = sizes.entry(sha.clone()).or_default();
            *known = known.or(*size);
        }
        let mut out = BTreeMap::new();
        let cached: Vec<Result<(String, Option<Response>)>> =
            stream::iter(sizes.keys().cloned().collect::<BTreeSet<String>>())
                .map(|sha| -> Pin<Box<dyn Future<Output = _> + Send + '_>> {
                    Box::pin(async move {
                        let response = self
                            .get_representation(&path(repo, &sha), None, Read::Cached)
                            .await?;
                        Ok((sha, response))
                    })
                })
                .buffer_unordered(self.concurrency())
                .collect()
                .await;
        let mut missing = Vec::new();
        for item in cached {
            match item? {
                (sha, Some(response)) => match decode(&response.body) {
                    Ok(content) if blob_sha(&content) == sha => {
                        out.insert(sha, content);
                    }
                    _ => {
                        self.0
                            .cache
                            .http_remove(&self.blob_key(repo, &sha)?)
                            .await
                            .map_err(|_| Error::Cache)?;
                        missing.push(sha);
                    }
                },
                (sha, None) => missing.push(sha),
            }
        }
        // `buffer_unordered` finishes in any order; the batch query's
        // variables must name its SHAs the same way every run.
        missing.sort_unstable();
        let (large, small): (Vec<_>, Vec<_>) = missing
            .iter()
            .map(|sha| (sha.clone(), sizes.get(sha).copied().flatten()))
            .partition(|(sha, size)| {
                object_id(sha) && size.is_some_and(|size| size >= LARGE_BLOB_BYTES)
            });
        let raw = async {
            stream::iter(large)
                .map(|(sha, _)| -> Pin<Box<dyn Future<Output = _> + Send + '_>> {
                    Box::pin(async move {
                        Ok::<_, Error>(self.raw_blob(repo, &sha).await?.map(|blob| (sha, blob)))
                    })
                })
                .buffer_unordered(self.concurrency())
                .collect::<Vec<_>>()
                .await
        };
        let graphql = async {
            let Some((owner, name)) = repo.split_once('/') else {
                return Vec::new();
            };
            // GraphQL refuses a whole query for one malformed object ID.
            let small: Vec<_> = small
                .into_iter()
                .filter(|(sha, _)| object_id(sha))
                .collect();
            stream::iter(graphql_chunks(&small))
                .map(|chunk| -> Pin<Box<dyn Future<Output = _> + Send + '_>> {
                    Box::pin(async move { self.graphql_blobs(owner, name, &chunk).await })
                })
                .buffer_unordered(self.concurrency())
                .collect::<Vec<_>>()
                .await
        };
        let (raw, batches) = tokio::join!(raw, graphql);
        let raw: Vec<_> = raw
            .into_iter()
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect();
        // The raw blobs, and each GraphQL batch, go to the cache in one write.
        for batch in std::iter::once(Ok(raw)).chain(batches) {
            let batch = batch?;
            self.store_blobs(repo, &batch).await?;
            out.extend(batch);
        }
        let rest: Vec<_> = missing
            .into_iter()
            .filter(|sha| !out.contains_key(sha))
            .collect();
        // The cache pass above already took every cached blob that hashes
        // right, so these always ask GitHub. The REST JSON answer is decoded
        // directly (offline test fixtures use non-SHA names); cached re-reads
        // verify `blob_sha` on load.
        let fetched: Vec<Result<(String, Vec<u8>)>> = stream::iter(rest)
            .map(|sha| async move {
                let response = self
                    .get_representation(&path(repo, &sha), None, Read::Revalidate)
                    .await?
                    .ok_or(Error::MissingBody)?;
                Ok((sha, decode(&response.body)?))
            })
            .buffer_unordered(self.concurrency())
            .collect()
            .await;
        for item in fetched {
            let (sha, content) = item?;
            out.insert(sha, content);
        }
        Ok(out)
    }

    /// The bytes of blob `sha` as REST's raw media type gives them, when they
    /// hash to `sha`. An answer GitHub refuses, limits or cannot give, or
    /// bytes that hash to anything else, give none, so the JSON read takes
    /// over, but a refused credential stops the read.
    async fn raw_blob(&self, repo: &str, sha: &str) -> Result<Option<Vec<u8>>> {
        let mut url = self.url(&path(repo, sha))?;
        let mut visited = HashSet::new();
        for _ in 0..MAX_REDIRECTS {
            if !visited.insert(url.clone()) {
                return Ok(None);
            }
            let mut request = self.request(Method::GET, url.clone());
            request
                .headers_mut()
                .insert(header::ACCEPT, HeaderValue::from_static(RAW));
            let response = match self.exchange(&request, false, false).await {
                Ok(response) => response,
                Err(Error::RateLimit | Error::Transport(_)) => return Ok(None),
                Err(error) => return Err(error),
            };
            match response.status.as_u16() {
                301 | 302 | 303 | 307 | 308 => {
                    let Some(target) = response
                        .headers
                        .get(header::LOCATION)
                        .and_then(|header| header.to_str().ok())
                        .and_then(|location| url.join(location).ok())
                    else {
                        return Ok(None);
                    };
                    // Same origin only, as every REST read.
                    match self.url(target.as_str()) {
                        Ok(target) => url = target,
                        Err(_) => return Ok(None),
                    }
                }
                status @ (401 | 403) => return Err(Error::Status(status)),
                _ if response.status.is_success() && blob_sha(&response.body) == sha => {
                    return Ok(Some(response.body));
                }
                _ => return Ok(None),
            }
        }
        Ok(None)
    }

    /// The blobs of `shas` GraphQL gives exactly. A query GitHub refuses,
    /// limits or cannot answer gives none, so REST reads them instead, but a
    /// refused credential stops the read.
    async fn graphql_blobs(
        &self,
        owner: &str,
        name: &str,
        shas: &[String],
    ) -> Result<Vec<(String, Vec<u8>)>> {
        let parameters: String = (0..shas.len())
            .map(|i| format!(", $o{i}: GitObjectID!"))
            .collect();
        let fields: Vec<String> = (0..shas.len())
            .map(|i| {
                format!(
                    "b{i}: object(oid: $o{i}) {{ ... on Blob {{ text isBinary isTruncated }} }}"
                )
            })
            .collect();
        let query = format!(
            "query($owner: String!, $name: String!{parameters}) {{ repository(owner: $owner, name: $name) {{ {} }} }}",
            fields.join(" ")
        );
        let mut variables = json!({ "owner": owner, "name": name });
        for (i, sha) in shas.iter().enumerate() {
            variables[format!("o{i}")] = sha.clone().into();
        }
        let data = match self.graphql::<Value>(&query, variables).await {
            Ok(data) => data,
            Err(Error::Status(status @ (401 | 403))) => return Err(Error::Status(status)),
            Err(Error::Graphql(_) | Error::Status(_) | Error::RateLimit | Error::Transport(_)) => {
                return Ok(Vec::new());
            }
            Err(error) => return Err(error),
        };
        Ok(shas
            .iter()
            .enumerate()
            .filter_map(|(i, sha)| {
                let blob = &data["repository"][format!("b{i}")];
                if blob["isBinary"] != false || blob["isTruncated"] != false {
                    return None;
                }
                let text = blob["text"].as_str()?.as_bytes();
                (blob_sha(text) == *sha).then(|| (sha.clone(), text.to_vec()))
            })
            .collect())
    }

    /// Keep each blob's content as its REST representation.
    async fn store_blobs(&self, repo: &str, blobs: &[(String, Vec<u8>)]) -> Result<()> {
        if blobs.is_empty() {
            return Ok(());
        }
        let now = conditional::unix_now();
        let entries = blobs
            .iter()
            .map(|(sha, content)| {
                let body = serde_json::to_vec(&json!({
                    "sha": sha,
                    "encoding": "base64",
                    "content": base64::engine::general_purpose::STANDARD.encode(content),
                }))
                .map_err(|_| Error::Json)?;
                let response = Response {
                    status: StatusCode::OK,
                    headers: HeaderMap::new(),
                    body,
                    stale: None,
                };
                Ok((
                    self.blob_key(repo, sha)?,
                    conditional::entry(&response, now),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        self.0
            .cache
            .http_put_many(&entries)
            .await
            .map_err(|_| Error::Cache)
    }

    /// The cache key a blob's REST representation is kept under.
    fn blob_key(&self, repo: &str, sha: &str) -> Result<patchula_cache::HttpKey> {
        let url = self.url(&path(repo, sha))?;
        conditional::key(
            "GET",
            url.as_str(),
            SENT,
            None,
            Some(self.credential_scope()),
        )
        .map_err(|_| Error::Cache)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_shas_match_git() {
        // `printf 'hi\n' | git hash-object --stdin`
        assert_eq!(
            blob_sha(b"hi\n"),
            "45b983be36b73c0788dc9cbcb76cbb80fc7bb057"
        );
    }
}
