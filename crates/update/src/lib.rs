//! Deterministic manifest plans, signed GitHub branches and pull request lifecycle.
#![forbid(unsafe_code)]
pub mod advertise;
pub mod automerge;
pub mod batch;
mod body;
mod cleanup;
pub mod dashboard;
pub mod markers;
mod receipts;
mod settle;
pub mod text;
mod trailers;
pub use cleanup::{cleanup, delete_owned, owned};
pub use receipts::{
    INVENTORY_QUERY, Inventory, RestoreOutcome, restore_inventory, restore_unchanged, save_receipts,
};
pub use settle::{Settlement, settle};
pub mod lockfiles;
mod plan;
mod write;
pub use plan::{Blob, Change, Hold, Plan, Snapshot, blob_sha, branch_name, plan, unified};
pub use write::{REF_UPDATE, apply_all, needs_requeue};

pub mod pull_requests;
pub mod tooling;
pub use pull_requests::{BODY_LIMIT, Lifecycle, update_key};
