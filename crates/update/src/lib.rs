//! Deterministic manifest plans and guarded GitHub Git Data commits.
#![forbid(unsafe_code)]
mod cleanup;
pub use cleanup::{cleanup, delete_owned, owned};
mod plan;
mod write;
pub use plan::{Blob, Plan, Snapshot, blob_sha, branch_name, plan};
pub use write::{REF_UPDATE, apply, apply_all};
