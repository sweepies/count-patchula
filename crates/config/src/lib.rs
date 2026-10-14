//! Pkl configuration schema and validation for rustovate.
#![forbid(unsafe_code)]

/// The schema is part of the executable, independent of the current directory.
pub const SCHEMA: &str = include_str!("../../../Rustovate.pkl");
