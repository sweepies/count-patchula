//! Git blobs by SHA: from the cache when it holds them, else through GraphQL
//! in batches, else through REST. A blob read through GraphQL counts only when
//! its text hashes to the SHA asked for, so a binary, truncated or re-encoded
//! answer is never taken for the blob. Either way the blob is kept in the
//! cache as its REST representation, so a later read needs no request.
use crate::{Client, Error, Read, Result, cache_entry};
use base64::Engine;
use futures_util::{StreamExt, stream};
use patchula_http::Response;
use reqwest::{Method, StatusCode, header::HeaderMap};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
};

/// Blobs one GraphQL query reads.
const BATCH: usize = 50;

fn path(repo: &str, sha: &str) -> String {
    format!("repos/{repo}/git/blobs/{sha}")
}

/// The Git blob SHA of `content`.
fn blob_sha(content: &[u8]) -> String {
    let mut digest = Sha1::new();
    digest.update(format!("blob {}\0", content.len()));
    digest.update(content);
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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

impl Client {
    /// The content of each blob in `shas` from `repo` (`owner/name`).
    pub async fn blobs(&self, repo: &str, shas: &[String]) -> Result<BTreeMap<String, Vec<u8>>> {
        let mut out = BTreeMap::new();
        let mut missing = Vec::new();
        for sha in shas.iter().collect::<BTreeSet<_>>() {
            match self
                .get_representation(&path(repo, sha), None, Read::Cached)
                .await?
            {
                Some(response) => {
                    out.insert(sha.clone(), decode(&response.body)?);
                }
                None => missing.push(sha.clone()),
            }
        }
        if let Some((owner, name)) = repo.split_once('/') {
            let chunks: Vec<Vec<String>> = missing.chunks(BATCH).map(<[String]>::to_vec).collect();
            let batches: Vec<_> = stream::iter(chunks)
                .map(|chunk| -> Pin<Box<dyn Future<Output = _> + Send + '_>> {
                    Box::pin(async move { self.graphql_blobs(owner, name, &chunk).await })
                })
                .buffer_unordered(self.concurrency())
                .collect()
                .await;
            for (sha, content) in batches.into_iter().flatten() {
                self.store_blob(repo, &sha, &content).await?;
                out.insert(sha, content);
            }
        }
        let rest: Vec<_> = missing
            .into_iter()
            .filter(|sha| !out.contains_key(sha))
            .collect();
        let fetched: Vec<Result<(String, Vec<u8>)>> = stream::iter(rest)
            .map(|sha| async move {
                let response = self.get_immutable(&path(repo, &sha)).await?;
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

    /// The blobs of `shas` GraphQL gives exactly. A failed query gives none,
    /// so REST reads them instead.
    async fn graphql_blobs(
        &self,
        owner: &str,
        name: &str,
        shas: &[String],
    ) -> Vec<(String, Vec<u8>)> {
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
        let Ok(data) = self.graphql(&query, variables).await else {
            return Vec::new();
        };
        shas.iter()
            .enumerate()
            .filter_map(|(i, sha)| {
                let blob = &data["repository"][format!("b{i}")];
                if blob["isBinary"] != false || blob["isTruncated"] != false {
                    return None;
                }
                let text = blob["text"].as_str()?.as_bytes();
                (blob_sha(text) == *sha).then(|| (sha.clone(), text.to_vec()))
            })
            .collect()
    }

    /// Keep `content` as the REST representation of the blob.
    async fn store_blob(&self, repo: &str, sha: &str, content: &[u8]) -> Result<()> {
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
        };
        let request = self.request(Method::GET, self.url(&path(repo, sha))?);
        let key = self.key(&request, [])?;
        self.0
            .cache
            .http_put(&key, &cache_entry(&response))
            .await
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
