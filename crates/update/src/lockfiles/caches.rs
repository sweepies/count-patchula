//! Persistent, partitioned caches for the package data lockfile tools
//! download, under `tool-caches/` in the runner's cache directory.
//!
//! Each tool's cache lives in a partition: `public` when its run holds no
//! registry credentials, shared by every installation and the CLI, or
//! `private-<hash>` when it does, keyed by the sorted `Credential::scope()`s
//! of the entries that run receives, so packages fetched with one
//! installation's credentials never reach another's cache. There is no
//! read-only lower layer: a private run does not touch `public`, since a
//! public package of the same name could then satisfy it without the
//! private feed (dependency confusion), and a stale proxy listing could
//! resolve `latest` wrong. A private partition warms after its first run.
//! Repositories that commit their own credentials are refused upstream, so
//! nothing credentialed enters `public`.
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
pub(super) const PUBLIC: &str = "public";
/// The file inside a partition whose mtime records its last use.
const USED: &str = ".used";
/// A partition unused for this long is pruned.
const AGE: Duration = Duration::from_secs(14 * 86_400);
/// How often one process prunes each root, at most.
pub(super) const EVERY: Duration = Duration::from_secs(3600);

/// A tool whose package data persists: whether its own cache is documented
/// as safe for concurrent use.
pub(super) struct Tool {
    /// Take the partition's `<tool>.lock` exclusively while it runs.
    serial: bool,
}

/// How a tool's cache is wired, or `None` for one with no cache to keep.
pub(super) fn tool(bin: &str) -> Option<Tool> {
    Some(match bin {
        // Documented concurrent-safe caches, shared freely within a partition:
        // Cargo locks its package cache itself, the Go module cache is
        // designed for concurrent use, and so are npm's cacache and uv's cache.
        "cargo" | "go" | "npm" | "uv" => Tool { serial: false },
        // No documented concurrency guarantee: one run at a time per partition.
        "aube" | "bun" | "yarn" | "composer" | "poetry" | "bundle" | "dotnet" => {
            Tool { serial: true }
        }
        // Terraform's `providers lock` keeps no package cache, and nix and apm
        // keep their own.
        _ => return None,
    })
}

/// The `private-<hash>` partition credentialed runs share: a name nobody
/// outside a run with the same entries can predict, keyed by the entries'
/// scopes alone, never their secrets. Runs without credentials choose
/// between `public` and `installation-<hash>` by what they can fetch, in
/// `Runner::use_caches`.
pub(super) fn partition(scopes: &[String]) -> String {
    format!("private-{}", hash(scopes.join("\n").as_bytes()))
}

/// The locks a run holds on its partition, dropped when it finishes.
pub(super) struct Hold {
    _shared: std::fs::File,
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

/// The environment placing `bin`'s cache inside `partition`.
fn env(bin: &str, partition: &Path) -> Vec<(String, String)> {
    let path = |path: &str| partition.join(path).to_string_lossy().into_owned();
    match bin {
        // Cargo keeps credentials out of CARGO_HOME; tokens are env vars.
        "cargo" => vec![("CARGO_HOME".into(), path("cargo"))],
        "go" => vec![
            ("GOMODCACHE".into(), path("gomod")),
            // The module cache is read-only by default, which eviction could
            // not remove; `-modcacherw` keeps it writable.
            ("GOFLAGS".into(), "-modcacherw".into()),
        ],
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
        "dotnet" => vec![
            ("NUGET_PACKAGES".into(), path("nuget/packages")),
            ("NUGET_HTTP_CACHE_PATH".into(), path("nuget/http-cache")),
        ],
        "bundle" => vec![("BUNDLE_USER_CACHE".into(), path("bundler"))],
        _ => Vec::new(),
    }
}

/// Hold `name`'s partition for one `bin` run, and return its environment.
/// Blocking, so callers run it off the async runtime.
pub(super) fn acquire(
    root: &Path,
    name: &str,
    tool: &Tool,
    bin: &str,
) -> Result<(Vec<(String, String)>, Hold), String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let shared = lock(&root.join(format!("{name}.lock")), false)?;
    let partition = root.join(name);
    std::fs::create_dir_all(&partition).map_err(|e| e.to_string())?;
    touch(&partition)?;
    let env = env(bin, &partition);
    for (key, value) in &env {
        if key != "GOFLAGS" {
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

/// Restore the owner's write permission on `path`, where a tool wrote it
/// read-only, as a Go module cache does without `-modcacherw`. A symlink
/// is never followed: its mode is its target's concern, not the
/// partition's, and removing the link is enough.
fn unreadonly(path: &Path) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if meta.file_type().is_symlink() {
        return;
    }
    let permissions = meta.permissions();
    if !permissions.readonly() {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            path,
            std::fs::Permissions::from_mode(permissions.mode() | 0o200),
        );
    }
    #[cfg(not(unix))]
    {
        let mut permissions = permissions;
        permissions.set_readonly(false);
        let _ = std::fs::set_permissions(path, permissions);
    }
}

/// Restore write permission everywhere under `dir`, so a partition written
/// by a tool whose cache is read-only (an older Go module cache) can still
/// be removed.
fn writable(dir: &Path) {
    unreadonly(dir);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        unreadonly(&path);
        // `file_type` does not follow a link, so a symlinked directory
        // under the partition is removed, never descended into.
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            writable(&path);
        }
    }
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
    let partition = root.join(name);
    writable(&partition);
    match std::fs::remove_dir_all(&partition) {
        Ok(()) => {
            // The lock file goes with its partition while the exclusive
            // hold lasts, so a new partition of the name cannot race a
            // leftover lock.
            let _ = std::fs::remove_file(root.join(format!("{name}.lock")));
            true
        }
        Err(error) => {
            tracing::warn!(%error, partition = %name, "tool cache partition was not removed");
            false
        }
    }
}

/// Whether it is time to prune `root` again: at most once per `EVERY` per
/// root per process. The server builds one Runner per installation, so the
/// record is process-wide, keyed by root, not per Runner. A prune records
/// itself only when it actually ran — when `prune.lock` is held elsewhere,
/// a skipped call must stay eligible.
pub(super) fn due(root: &Path) -> bool {
    PRUNED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(root)
        .is_none_or(|ran| ran.elapsed() >= EVERY)
}

/// When the last prune of `root` ran.
static PRUNED: std::sync::Mutex<BTreeMap<PathBuf, std::time::Instant>> =
    std::sync::Mutex::new(BTreeMap::new());

/// Mark `root` pruned: called only by a prune that ran.
fn record(root: &Path) {
    PRUNED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(root.to_path_buf(), std::time::Instant::now());
}

/// Drop partitions unused for `AGE`, then least-recently-used private ones,
/// then `public`, until `root` is within `cap` bytes. A partition a run holds
/// is never removed. One prune runs per root at a time, across processes too:
/// a concurrent prune could over-evict, so a held `prune.lock` skips this one.
pub(super) fn prune(root: &Path, cap: u64, now: SystemTime) -> Result<(), String> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    let guard = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join("prune.lock"))
        .map_err(|e| e.to_string())?;
    if guard.try_lock().is_err() {
        return Ok(());
    }
    let _guard = guard;
    record(root);
    let mut partitions = Vec::new();
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name != PUBLIC && !name.starts_with("private-") && !name.starts_with("installation-") {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Restoring write permission never follows a symlink: the link is
    /// skipped, so its target outside the partition keeps its mode.
    #[test]
    fn unreadonly_never_follows_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside");
        std::fs::write(&outside, "x").unwrap();
        let mut permissions = std::fs::metadata(&outside).unwrap().permissions();
        permissions.set_mode(0o444);
        std::fs::set_permissions(&outside, permissions).unwrap();
        let partition = temp.path().join("public");
        std::fs::create_dir_all(&partition).unwrap();
        std::os::unix::fs::symlink(&outside, partition.join("link")).unwrap();
        writable(&partition);
        let mode = std::fs::metadata(&outside).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o444, "the symlink's target was followed");
    }

    /// Removing a partition removes its lock file too, while the
    /// eviction still holds the lock exclusively.
    #[test]
    fn an_evicted_partitions_lock_file_is_removed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("private-x")).unwrap();
        std::fs::File::create(root.join("private-x.lock")).unwrap();
        assert!(evict(root, "private-x"));
        assert!(!root.join("private-x").exists());
        assert!(!root.join("private-x.lock").exists());
    }

    /// A partition a run still holds is not evicted, and its lock file
    /// stays put.
    #[test]
    fn a_held_partition_keeps_its_lock_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("private-x")).unwrap();
        let file = std::fs::File::create(root.join("private-x.lock")).unwrap();
        file.lock_shared().unwrap();
        assert!(!evict(root, "private-x"));
        assert!(root.join("private-x").exists());
        assert!(root.join("private-x.lock").exists());
    }

    /// `due` stays true while `prune.lock` is held — the skipped prune
    /// never ran, so it records nothing — and goes false after a prune
    /// that ran.
    #[test]
    fn a_prune_marks_due_only_when_it_ran() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("tool-caches-due-test");
        std::fs::create_dir_all(&root).unwrap();
        assert!(due(&root));
        let guard = std::fs::File::create(root.join("prune.lock")).unwrap();
        guard.lock().unwrap();
        prune(&root, 0, SystemTime::now()).unwrap();
        assert!(due(&root), "a skipped prune still counts as due");
        drop(guard);
        prune(&root, 0, SystemTime::now()).unwrap();
        assert!(!due(&root), "a real prune marks itself done");
    }
}
