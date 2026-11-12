//! Deterministic manifest plans, signed GitHub branches and pull request lifecycle.
#![forbid(unsafe_code)]
pub mod advertise;
pub mod automerge;
pub mod batch;
mod body;
mod cleanup;
pub mod dashboard;
mod error;
pub mod identity;
pub mod maintenance;
pub mod markers;
mod receipts;
mod settle;
pub mod text;
mod trailers;
pub use cleanup::{cleanup, delete_owned, owned};
pub use error::{Error, PlanError, Result};
pub use receipts::{
    DefaultBranch, DefaultCommit, INVENTORY_QUERY, Inventory, Issue, Page, RestoreOutcome,
    restore_inventory, save_receipts,
};
pub use settle::{Settlement, settle};
mod plan;
mod pr;
#[cfg(feature = "testing")]
pub use plan::testing;
mod write;
pub use plan::{
    Blob, Change, Hold, Plan, Snapshot, blob_sha, branch_name, lock_all, plan, unified,
};
pub use write::{REF_UPDATE, apply_all, needs_requeue};

pub mod pull_requests;
pub mod tooling;
pub use pull_requests::{BODY_LIMIT, Lifecycle};
