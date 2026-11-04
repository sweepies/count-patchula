//! Persistent, partitioned caches for the package data lockfile tools
//! download, under `tool-caches/` in the runner's cache directory.
//!
//! Each tool's cache lives in a partition: `public` when its run holds no
//! registry credentials, shared by every installation and the CLI, or
//! `private-<hash>` when it does, keyed by the sorted `Credential::scope()`s
//! of the entries that run receives, so packages fetched with one
//! installation's credentials never reach another's cache. Where a tool has a
//! documented read-only lower layer (Go's module proxy layout, NuGet's
//! fallback folders) a private run reads the public partition's content
//! without writing to it; it holds a shared lock on both partitions.
//!
//! A run holds its partition's lock file, `tool-caches/<name>.lock`, shared,
//! so eviction never removes a partition in use. Tools whose caches document
//! concurrent use (Cargo's package cache, the Go module cache, npm's cacache,
//! uv's cache) share a partition freely; for the rest an exclusive
//! `<partition>/<tool>.lock` serialises them.
use super::*;
use std::time::{Duration, SystemTime};

/// The partitions' directory inside the runner's cache.
pub(super) const ROOT: &str = "tool-caches";
const PUBLIC: &str = "public";
/// The file inside a partition whose mtime records its last use.
const USED: &str = ".used";
/// A partition unused for this long is pruned.
const AGE: Duration = Duration::from_secs(14 * 86_400);
/// How often one process prunes, at most.
pub(super) const EVERY: Duration = Duration::from_secs(3600);

/// A tool whose package data persists: whether its own cache is documented as
/// safe for concurrent use, and the public directories a private run may use
/// as a read-only lower layer.
pub(super) struct Tool {
    /// Take the partition's `<tool>.lock` exclusively while it runs.
    serial: bool,
    /// Directories inside `public` the tool reads through a lower layer.
    lower: &'static [&'static str],
}

/// How a tool's cache is wired, or `None` for one with no cache to keep.
pub(super) fn tool(bin: &str) -> Option<Tool> {
    Some(match bin {
        // Documented concurrent-safe caches, shared freely within a partition:
        // Cargo locks its package cache itself, the Go module cache is
        // designed for concurrent use, and so are npm's cacache and uv's cache.
        "cargo" | "npm" | "uv" => Tool {
            serial: false,
            lower: &[],
        },
        "go" => Tool {
            serial: false,
            // A module cache's `cache/download` tree is a module proxy: a
            // private run lists it first in GOPROXY as a read-only file://
            // proxy, so public modules never download twice.
            lower: &["gomod/cache/download"],
        },
        "dotnet" => Tool {
            serial: true,
            // Fallback package folders are read-only by contract.
            lower: &["nuget/packages"],
        },
        // No documented concurrency guarantee: one run at a time per partition.
        "aube" | "bun" | "yarn" | "composer" | "poetry" | "bundle" => Tool {
            serial: true,
            lower: &[],
        },
        // Terraform's `providers lock` keeps no package cache, and nix and apm
        // keep their own.
        _ => return None,
    })
}

/// The partition `scopes` shares: `public` without credentials, else a name
/// nobody outside a run with the same entries can predict, keyed by the
/// entries' scopes alone, never their secrets.
pub(super) fn partition(scopes: &[String]) -> String {
    if scopes.is_empty() {
        PUBLIC.into()
    } else {
        format!("private-{}", hash(scopes.join("\n").as_bytes()))
    }
}

/// The locks a run holds on its partition, dropped when it finishes.
pub(super) struct Hold {
    _shared: std::fs::File,
    _lower: Option<std::fs::File>,
    _serial: Option<std::fs::File>,
}

/// Open (creating) `path` and lock it; a run's shared hold keeps eviction out,
/// a serial tool's exclusive one serialises the partition between its runs.
fn lock(path: &Path, exclusive: bool) -> Result<std::fs::File, String> {
    let file = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    if exclusive {
        file.lock()
    } else {
        file.lock_shared()
    }
    .map(|()| file)
    .map_err(|e| format!("cannot lock tool cache {}: {e}", path.display()))
}

/// Mark the partition used now.
fn touch(partition: &Path) -> Result<(), String> {
    std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(partition.join(USED))
        .and_then(|file| file.set_modified(SystemTime::now()))
        .map_err(|e| e.to_string())
}

/// The environment placing `bin`'s cache inside `partition`, with `lower` the
/// public partition a private run reads through. `goproxy` is the proxy list
/// the run would use anyway, after the file proxy.
fn env(bin: &str, partition: &Path, lower: Option<&Path>, goproxy: &str) -> Vec<(String, String)> {
    let path = |path: &str| partition.join(path).to_string_lossy().into_owned();
    match bin {
        // Cargo keeps credentials out of CARGO_HOME; tokens are env vars.
        "cargo" => vec![("CARGO_HOME".into(), path("cargo"))],
        "go" => {
            let mut env = vec![("GOMODCACHE".into(), path("gomod"))];
            if let Some(lower) = lower {
                env.push((
                    "GOPROXY".into(),
                    format!(
                        "file://{},{goproxy}",
                        lower.join("gomod/cache/download").display()
                    ),
                ));
            }
            env
        }
        "npm" => vec![("NPM_CONFIG_CACHE".into(), path("npm"))],
        "aube" => vec![
            ("AUBE_CACHE_DIR".into(), path("aube/cache")),
            ("AUBE_STORE_DIR".into(), path("aube/store")),
        ],
        "bun" => vec![("BUN_INSTALL_CACHE_DIR".into(), path("bun"))],
        "yarn" => vec![("YARN_GLOBAL_FOLDER".into(), path("yarn"))],
        "composer" => vec![("COMPOSER_CACHE_DIR".into(), path("composer"))],
        "uv" => vec![("UV_CACHE_DIR".into(), path("uv"))],
        "poetry" => vec![("POETRY_CACHE_DIR".into(), path("poetry"))],
        "dotnet" => {
            let mut env = vec![
                ("NUGET_PACKAGES".into(), path("nuget/packages")),
                ("NUGET_HTTP_CACHE_PATH".into(), path("nuget/http-cache")),
            ];
            if let Some(lower) = lower {
                env.push((
                    "NUGET_FALLBACK_PACKAGES".into(),
                    lower.join("nuget/packages").to_string_lossy().into_owned(),
                ));
            }
            env
        }
        "bundle" => vec![("BUNDLE_USER_CACHE".into(), path("bundler"))],
        _ => Vec::new(),
    }
}

/// Hold `name`'s partition (and `public`'s for a private run with a lower
/// layer) for one `bin` run, and return its environment. Blocking, so callers
/// run it off the async runtime.
pub(super) fn acquire(
    root: &Path,
    name: &str,
    tool: &Tool,
    bin: &str,
    goproxy: &str,
) -> Result<(Vec<(String, String)>, Hold), String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let shared = lock(&root.join(format!("{name}.lock")), false)?;
    let partition = root.join(name);
    std::fs::create_dir_all(&partition).map_err(|e| e.to_string())?;
    touch(&partition)?;
    let (lower_dir, lower) = if name != PUBLIC && !tool.lower.is_empty() {
        let file = lock(&root.join(format!("{PUBLIC}.lock")), false)?;
        let public = root.join(PUBLIC);
        for dir in tool.lower {
            std::fs::create_dir_all(public.join(dir)).map_err(|e| e.to_string())?;
        }
        touch(&public)?;
        (Some(public), Some(file))
    } else {
        (None, None)
    };
    let env = env(bin, &partition, lower_dir.as_deref(), goproxy);
    for (key, value) in &env {
        if key != "GOPROXY" {
            std::fs::create_dir_all(value).map_err(|e| e.to_string())?;
        }
    }
    let serial = tool
        .serial
        .then(|| lock(&partition.join(format!("{bin}.lock")), true))
        .transpose()?;
    Ok((
        env,
        Hold {
            _shared: shared,
            _lower: lower,
            _serial: serial,
        },
    ))
}

/// A partition's last use: its marker's mtime, else its directory's.
fn used_at(partition: &Path) -> SystemTime {
    std::fs::metadata(partition.join(USED))
        .or_else(|_| std::fs::metadata(partition))
        .and_then(|m| m.modified())
        .unwrap_or_else(|_| SystemTime::now())
}

fn size(dir: &Path) -> u64 {
    fn walk(dir: &Path, total: &mut u64) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                walk(&entry.path(), total);
            } else if meta.is_file() {
                *total += meta.len();
            }
        }
    }
    let mut total = 0;
    walk(dir, &mut total);
    total
}

/// Remove `name`'s partition when nothing holds it: the lock is only tried
/// exclusively, so a run's shared hold keeps it.
fn evict(root: &Path, name: &str) -> bool {
    let Ok(file) = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join(format!("{name}.lock")))
    else {
        return false;
    };
    if file.try_lock().is_err() {
        return false;
    }
    match std::fs::remove_dir_all(root.join(name)) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, partition = %name, "tool cache partition was not removed");
            false
        }
    }
}

/// Drop partitions unused for `AGE`, then least-recently-used private ones,
/// then `public`, until `root` is within `cap` bytes. A partition a run holds
/// is never removed.
pub(super) fn prune(root: &Path, cap: u64, now: SystemTime) -> Result<(), String> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    let mut partitions = Vec::new();
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name != PUBLIC && !name.starts_with("private-") {
            continue;
        }
        partitions.push((name, entry.path(), used_at(&entry.path())));
    }
    let mut kept = Vec::new();
    for (name, path, used) in partitions {
        let stale = now
            .duration_since(used)
            .map(|age| age > AGE)
            .unwrap_or(false);
        if !stale || !evict(root, &name) {
            kept.push((name, used, size(&path)));
        }
    }
    let mut total: u64 = kept.iter().map(|(_, _, size)| *size).sum();
    if total <= cap {
        return Ok(());
    }
    // The least recently used private partition goes first, `public` last.
    kept.sort_by_key(|(_, used, _)| *used);
    let mut rest = Vec::new();
    for (name, used, size) in kept.into_iter() {
        if total > cap && name != PUBLIC && evict(root, &name) {
            total -= size;
        } else {
            rest.push((name, used, size));
        }
    }
    if total > cap {
        for (name, _, _) in rest {
            if name == PUBLIC {
                evict(root, PUBLIC);
            }
        }
    }
    Ok(())
}
