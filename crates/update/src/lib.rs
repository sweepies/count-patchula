//! Deterministic manifest plans and guarded GitHub Git Data commits.
#![forbid(unsafe_code)]
mod plan;
mod write;
pub use plan::{Blob, Plan, Snapshot, blob_sha, branch_name, plan};
pub use write::{REF_UPDATE, apply};
