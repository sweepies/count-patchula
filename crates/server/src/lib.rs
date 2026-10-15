//! API-only long-lived frontend over the shared batch engine.
#![forbid(unsafe_code)]
mod installations;
mod runtime;
pub mod settings;
pub mod webhook;
pub use runtime::{Options, serve};
