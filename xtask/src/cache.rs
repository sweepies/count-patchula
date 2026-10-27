//! GitHub Actions cache pruning, through the gh CLI. Needs GH_TOKEN with
//! actions: write, and GH_REPO.

use crate::{Args, Result, output, run};
use serde_json::Value;
use std::process::Command;

struct Entry {
    id: u64,
    size: u64,
    key: String,
}

/// Keeps the newest entry of each key prefix on a ref, or deletes every entry
/// on it with --all.
pub fn prune(mut args: Args) -> Result {
    let reference = args
        .value("--ref")?
        .unwrap_or_else(|| "refs/heads/main".into());
    let all = args.flag("--all");
    let drop_others = args.flag("--drop-others");
    let prefixes = args.positional(0, usize::MAX)?;
    if all {
        return delete(list(&reference, None)?, 0);
    }
    if prefixes.is_empty() {
        return Err("name at least one key prefix, or pass --all".into());
    }
    for prefix in &prefixes {
        delete(list(&reference, Some(prefix))?, 1)?;
    }
    if drop_others {
        println!("caches matching no prefix:");
        let others = list(&reference, None)?
            .into_iter()
            .filter(|entry| !prefixes.iter().any(|p| entry.key.starts_with(p.as_str())))
            .collect();
        delete(others, 0)?;
    }
    Ok(())
}

/// The ref's caches, newest first, optionally only those whose key starts
/// with a prefix.
fn list(reference: &str, prefix: Option<&str>) -> Result<Vec<Entry>> {
    let mut gh = Command::new("gh");
    gh.args(["cache", "list", "--ref", reference, "--limit", "1000"])
        .args(["--sort", "created_at", "--order", "desc"])
        .args(["--json", "id,key,sizeInBytes"]);
    if let Some(prefix) = prefix {
        gh.args(["--key", prefix]);
    }
    let entries: Vec<Value> = serde_json::from_str(&output(&mut gh)?)?;
    entries
        .iter()
        .map(|entry| {
            Ok(Entry {
                id: entry["id"].as_u64().ok_or("a cache entry has no id")?,
                size: entry["sizeInBytes"].as_u64().unwrap_or(0),
                key: entry["key"].as_str().unwrap_or_default().to_owned(),
            })
        })
        .collect()
}

/// Deletes all but the first `keep` entries. A failed delete is a warning,
/// since the next run tries again.
fn delete(entries: Vec<Entry>, keep: usize) -> Result {
    let (mut count, mut freed) = (0, 0);
    for (index, entry) in entries.iter().enumerate() {
        if index < keep {
            println!("keep   {}", entry.key);
            continue;
        }
        println!("delete {}", entry.key);
        let id = entry.id.to_string();
        if run(Command::new("gh").args(["cache", "delete", &id])).is_err() {
            println!("::warning::could not delete cache {}", entry.key);
        }
        count += 1;
        freed += entry.size;
    }
    println!("deleted {count} entries, {} MB", freed / 1_048_576);
    Ok(())
}
