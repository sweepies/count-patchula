//! The cache scope of a fixed-token client. By default a token's scope is a
//! hash of the token itself. An installation token (`ghs_`, which includes an
//! Action's `GITHUB_TOKEN`) is minted fresh for every run, so that scope
//! would never carry anything from one run to the next. Such a token is
//! scoped by the reach GitHub reports for it instead: the App's bot user and
//! the exact set of repository ids the token lists. `repository_selection`
//! never shapes the scope, so a token narrowed to some repositories can
//! never share the full installation's entries, whatever GitHub calls its
//! selection. Every other token keeps the token hash, since its bytes are
//! already stable and GitHub does not report its reach.
//!
//! The reach is read live on every run and never cached, and anything
//! unexpected keeps the token hash: an error answer, a missing or unknown
//! `repository_selection` or one that changes between pages, a listing
//! longer than `PAGES` pages, empty, naming a repository twice or whose
//! count disagrees with `total_count`, a viewer without an id, or a token
//! that cannot read the contents of a repository in its set.
//!
//! Permissions are not part of the scope. Every endpoint the scoped cache
//! answers without asking GitHub (`crate::unrevalidated`) needs at most
//! `contents: read` on a repository the token reaches, and an installation
//! token's permissions are the same on every repository it reaches, so the
//! scope is used only after the token has read a repository's contents in
//! this run.
use crate::{Client, Error, Response, decode, hex, pagination::walk};
use reqwest::{Method, Url};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, sync::Arc};

/// Pages of `installation/repositories` a listing may take. A token that
/// reaches more repositories keeps the token hash.
const PAGES: usize = 10;
const PER_PAGE: usize = 100;

#[derive(Deserialize)]
struct Listing {
    total_count: usize,
    repository_selection: Option<String>,
    repositories: Vec<Repository>,
}

#[derive(Deserialize)]
struct Repository {
    id: u64,
    full_name: String,
    /// A missing flag counts as private.
    private: Option<bool>,
}

#[derive(Deserialize)]
struct Viewer {
    viewer: ViewerId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ViewerId {
    database_id: Option<u64>,
}

/// What `installation/repositories` says a token reaches.
struct Reach {
    /// `repos:{sha256 of the sorted ids}`.
    scope: String,
    /// A repository in the set whose contents the token must read: a
    /// private one when the set has any, since any token reads a public
    /// repository's contents.
    probe: String,
}

impl Client {
    /// This client with its token's reach as its credential scope, when the
    /// token is an installation token and GitHub reports a reach the rules
    /// above accept; otherwise this client unchanged. Call it before the
    /// client is cloned: a shared client is returned unchanged.
    pub async fn identify(self) -> Self {
        let Some(scope) = self.reach_scope().await else {
            return self;
        };
        match Arc::try_unwrap(self.0) {
            Ok(mut inner) => {
                inner.credential_scope = hex(&Sha256::digest(scope.as_bytes()));
                Self(Arc::new(inner))
            }
            Err(shared) => Self(shared),
        }
    }

    /// The scope string for this client's reach, before hashing; `None`
    /// keeps the token hash.
    async fn reach_scope(&self) -> Option<String> {
        // Refreshable credentials have no fixed token to probe.
        let token = self.0.auth.token()?;
        if !token.starts_with("ghs_") {
            return None;
        }
        let reach = self.installation_reach().await?;
        let bot = self
            .graphql::<Viewer>("query { viewer { databaseId } }", serde_json::json!({}))
            .await
            .ok()?
            .viewer
            .database_id?;
        if !self.reads_contents(&reach.probe).await {
            return None;
        }
        Some(format!("{}|app:{bot}/{}", self.api_url(), reach.scope))
    }

    /// The token's reach as `installation/repositories` reports it.
    async fn installation_reach(&self) -> Option<Reach> {
        let mut selection: Option<String> = None;
        let mut total = None;
        let mut repositories = Vec::new();
        // Any failure, or pages that disagree on the selection or total,
        // leaves the reach unknown; the error itself is not kept.
        walk(
            self.url(&format!("installation/repositories?per_page={PER_PAGE}"))
                .ok()?,
            PAGES,
            |target| self.url(target),
            |url| async move { self.get_live(url).await.ok_or(Error::Pagination) },
            |response| {
                let page: Listing = decode(&response.body, "GET installation/repositories".into())?;
                let page_selection = page.repository_selection.ok_or(Error::Json)?;
                if selection.get_or_insert_with(|| page_selection.clone()) != &page_selection
                    || *total.get_or_insert(page.total_count) != page.total_count
                {
                    return Err(Error::Pagination);
                }
                repositories.extend(page.repositories);
                Ok(false)
            },
        )
        .await
        .ok()?;
        reach(&selection?, total?, repositories)
    }

    /// Whether the token reads `repository`'s contents: listing one commit
    /// needs `contents: read`.
    async fn reads_contents(&self, repository: &str) -> bool {
        match self.url(&format!("repos/{repository}/commits?per_page=1")) {
            Ok(url) => self.get_live(url).await.is_some(),
            Err(_) => false,
        }
    }

    /// A GET that never reads or writes the cache and follows no redirect;
    /// `None` unless GitHub answers 2xx.
    async fn get_live(&self, url: Url) -> Option<Response> {
        let request = self.request(Method::GET, url);
        let response = self.exchange(&request, false, false).await.ok()?;
        response.status.is_success().then_some(response)
    }
}

/// The reach a complete listing names, or `None` when it names none the
/// rules accept. Either selection is keyed by the ids the token lists.
fn reach(selection: &str, total: usize, repositories: Vec<Repository>) -> Option<Reach> {
    if !matches!(selection, "all" | "selected") {
        return None;
    }
    let probe = repositories
        .iter()
        .min_by_key(|repository| (repository.private == Some(false), repository.id))?
        .full_name
        .clone();
    let ids: BTreeSet<u64> = repositories.iter().map(|r| r.id).collect();
    if ids.len() != total || repositories.len() != total {
        return None;
    }
    let joined = ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    Some(Reach {
        scope: format!("repos:{}", hex(&Sha256::digest(joined.as_bytes()))),
        probe,
    })
}
