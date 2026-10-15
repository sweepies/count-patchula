//! Deterministic manifest plans, signed GitHub branches and pull request lifecycle.
#![forbid(unsafe_code)]
mod cleanup;
pub use cleanup::{cleanup, delete_owned, owned};
mod plan;
mod write;
pub use plan::{Blob, Change, Plan, Snapshot, blob_sha, branch_name, plan};
pub use write::{REF_UPDATE, apply, apply_all};

mod pull_requests;
pub use pull_requests::{BODY_LIMIT, Lifecycle, Recheck, update_key};
