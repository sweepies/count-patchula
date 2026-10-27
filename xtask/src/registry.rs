//! The mise registry snapshot patchula-mise ships.

use crate::{Args, Result, output};
use serde_json::{Map, Value};
use std::process::Command;

/// Rewrites crates/mise/registry.json from the installed mise's registry.
pub fn update(args: Args) -> Result {
    args.positional(0, 0)?;
    let entries: Vec<Value> =
        serde_json::from_str(&output(Command::new("mise").args(["registry", "--json"]))?)?;
    let registry = entries
        .iter()
        .map(|entry| {
            let short = entry["short"]
                .as_str()
                .ok_or("a registry entry has no short name")?;
            Ok((short.to_owned(), entry["backends"].clone()))
        })
        .collect::<Result<Map<_, _>>>()?;
    let json = serde_json::to_string_pretty(&registry)? + "\n";
    std::fs::write("crates/mise/registry.json", json)?;
    Ok(())
}
