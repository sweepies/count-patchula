//! Deterministic manifest plans, signed GitHub branches and pull request lifecycle.
#![forbid(unsafe_code)]
pub mod automerge;
pub mod batch;
mod cleanup;
pub mod dashboard;
mod receipts;
pub use cleanup::{cleanup, delete_owned, owned};
pub use receipts::{
    INVENTORY_QUERY, Inventory, restore_inventory, restore_unchanged, save_receipts,
};
pub mod lockfiles;
mod plan;
mod write;
pub use plan::{Blob, Change, Plan, Snapshot, blob_sha, branch_name, plan};
pub use write::{REF_UPDATE, apply, apply_all, needs_requeue};

pub mod pull_requests;
pub use pull_requests::{BODY_LIMIT, Lifecycle, Recheck, update_key};
