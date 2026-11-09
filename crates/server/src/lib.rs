//! API-only long-lived frontend over the shared batch engine.
#![forbid(unsafe_code)]
mod installations;
mod public;
mod reload;
pub mod remote;
mod runtime;
pub mod webhook;
pub use runtime::{Options, serve};
