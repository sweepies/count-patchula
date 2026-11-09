//! Reading a repository's files as one engine. A push of the tree would
//! carry `entries`; the scan reads the ones the config selects, follows
//! what they name for a bounded number of rounds, marks what it cannot
//! read unreadable rather than absent, refuses committed credentials, and
//! hands extraction the files, the path inventory, the unsupported list,
//! the snapshot blobs and the warnings. GitHub and a local checkout differ
//! only in where the listing and the blob contents come from: `view` is
//! handed both.
use crate::{Error, Result};
use patchula_config::Config;
use patchula_manager::{
    Files, MORE_ROUNDS, Scanner, Skipped, Text, Unread, committed_credentials, credential_reason,
    linked_settings, registry_settings,
};
use patchula_update::{Blob, tooling};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// One file's bytes, or why they could not be read.
pub type Content = std::result::Result<Vec<u8>, String>;
/// A read's answers: one per entry asked for, in order.
pub type Contents = Result<Vec<Content>>;

/// One file a repository's tree carries.
#[derive(Clone)]
pub struct Entry {
    pub path: String,
    /// The Git blob id when the listing knows it without reading: the
    /// tree's SHA on GitHub, none locally, where it is the content's own.
    pub sha: Option<String>,
    /// The mode a push would record: `100644`, `100755`, or `120000` for a
    /// symlink, which is never read.
    pub mode: String,
}

/// A repository's file listing.
pub struct Listing {
    /// Every file the tree carries, symlinks included.
    pub entries: Vec<Entry>,
    /// Paths the listing saw that are not files the tree carries: below a
    /// symlinked directory, or not a regular file at all. Always empty for
    /// GitHub, whose trees hold no such paths.
    pub hidden: Vec<String>,
}

/// What reading a repository's files gives extraction and the snapshot.
pub struct View {
    /// Every path the tree carries, selected or not.
    pub paths: BTreeSet<String>,
    /// The files read, and the ones that could not be, as unreadable.
    pub files: Files,
    /// The blobs of the files read, for the snapshot's write-back.
    pub blobs: BTreeMap<String, Blob>,
    /// The files among `paths` the Count does not maintain.
    pub unsupported: Vec<Skipped>,
    /// The warnings the read itself gives.
    pub warnings: Vec<String>,
    /// Why the repository is refused for committed credentials, if it is.
    pub refusal: Option<String>,
}

/// Read `listing` for `scanner` and `config`. `read` returns the content
/// of each entry it is given, one result per entry in order: an outer
/// error fails the whole read, an inner one is that entry's own failure.
/// It is only ever given non-symlink entries; a `120000` entry is
/// unreadable without a read, as is one behind a symlink.
pub fn view(
    read: &mut dyn FnMut(&[Entry]) -> Contents,
    listing: Listing,
    scanner: &Scanner,
    config: &Config,
) -> Result<View> {
    let Listing { entries, hidden } = listing;
    let by_path: BTreeMap<&str, &Entry> = entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    let paths: BTreeSet<String> = entries.iter().map(|entry| entry.path.clone()).collect();
    let hidden: BTreeSet<String> = hidden.into_iter().collect();
    // Flux and Argo CD manifests are YAML anywhere in the tree, so the
    // delivery warning reads every YAML file, unless the tree has more
    // than it will take, when delivery stays unknown. Only a config
    // that can merge on its own and hasn't silenced the warning pays
    // for those reads.
    let merges = config.defaults.merge
        || config.security.apply.merge == Some(true)
        || config.rules.iter().any(|r| r.apply.merge == Some(true));
    let delivery = merges
        && !config
            .warnings
            .quiet
            .contains_key(patchula_words::ToolingRule::SecurityMergeWithoutDelivery.as_str());
    let manifests = entries
        .iter()
        .filter(|entry| tooling::manifest(&entry.path) && !scanner.needs(&entry.path))
        .count();
    let read_manifests = delivery && manifests <= tooling::MANIFEST_LIMIT;
    // A settings file a symlink stands for, or one inside a linked
    // `.bundle`, is unreadable rather than absent or read as the link's
    // target: without it, a scoped package would be looked up on the
    // public registry, where its name may be someone else's.
    let linked: BTreeSet<String> = entries
        .iter()
        .filter(|entry| entry.mode == "120000")
        .map(|entry| entry.path.as_str())
        .chain(hidden.iter().map(String::as_str))
        .filter_map(linked_settings)
        .filter(|settings| scanner.needs(settings))
        .collect();
    let mut files = Files::new();
    let mut blobs = BTreeMap::new();
    let mut wanted = Vec::new();
    for entry in &entries {
        if !(scanner.needs(&entry.path)
            || tooling::reads(&entry.path)
            || (read_manifests && tooling::manifest(&entry.path)))
        {
            continue;
        }
        if entry.mode == "120000" {
            files.insert(entry.path.clone(), Text::Unread(Unread::Failed));
        } else {
            wanted.push(entry.clone());
        }
    }
    let contents = read(&wanted)?;
    for (entry, content) in wanted.iter().zip(contents) {
        place(entry, content, true, &mut files, &mut blobs)?;
    }
    for settings in &linked {
        files.insert(settings.clone(), Text::Unread(Unread::Failed));
    }
    // What the files read so far name, such as the sources of a Gradle
    // build the settings include, until they name nothing new.
    for _ in 0..MORE_ROUNDS {
        let more = scanner.more(&files, paths.iter().chain(&hidden).map(String::as_str));
        if more.is_empty() {
            break;
        }
        let mut wanted = Vec::new();
        for path in more {
            match by_path.get(path.as_str()) {
                Some(entry) if entry.mode != "120000" => wanted.push((*entry).clone()),
                _ => {
                    files.insert(path, Text::Unread(Unread::Failed));
                }
            }
        }
        let contents = read(&wanted)?;
        for (entry, content) in wanted.iter().zip(contents) {
            place(entry, content, false, &mut files, &mut blobs)?;
        }
    }
    // What the chain still names when the rounds run out is unreadable the
    // same way, so its lookups fail saying why.
    let deep = scanner.more(&files, paths.iter().chain(&hidden).map(String::as_str));
    for path in &deep {
        files.insert(path.clone(), Text::Unread(Unread::TooDeep));
    }
    // A repository that commits its own registry credentials is refused:
    // its lockfile tools would fetch what they buy into the shared public
    // cache partition.
    let refusal = files
        .iter()
        .filter(|(path, _)| scanner.credential_checked(path))
        .find_map(|(path, text)| {
            committed_credentials(path, text.read().ok()?)
                .map(|kind| credential_reason(path, &kind))
        });
    let unsupported = scanner.unsupported(paths.iter().map(String::as_str));
    let mut warnings = Vec::new();
    for path in &deep {
        warnings.push(Unread::TooDeep.reason(path));
    }
    if delivery && manifests > tooling::MANIFEST_LIMIT {
        warnings.push(format!(
            "delivery unknown: more than {} YAML files",
            tooling::MANIFEST_LIMIT
        ));
    }
    Ok(View {
        paths,
        files,
        blobs,
        unsupported,
        warnings,
        refusal,
    })
}

/// One entry's content into `files` and `blobs`. `primary` is the first
/// selection: a file that cannot be read there refuses the repository,
/// while one named by a later round is unreadable instead. A settings
/// file that is not text is unreadable in either.
fn place(
    entry: &Entry,
    content: Content,
    primary: bool,
    files: &mut Files,
    blobs: &mut BTreeMap<String, Blob>,
) -> Result<()> {
    let bytes = match content {
        Ok(bytes) => bytes,
        Err(reason) if primary => {
            return Err(Error::Invalid(format!(
                "cannot read {}: {reason}",
                entry.path
            )));
        }
        Err(_) => {
            files.insert(entry.path.clone(), Text::Unread(Unread::Failed));
            return Ok(());
        }
    };
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) if !primary || registry_settings(&entry.path) => {
            files.insert(entry.path.clone(), Text::Unread(Unread::Failed));
            return Ok(());
        }
        Err(_) => return Err(Error::Invalid("selected file is not UTF-8".into())),
    };
    let sha = entry
        .sha
        .clone()
        .unwrap_or_else(|| patchula_update::blob_sha(&text));
    files.insert(entry.path.clone(), Text::Read(text.clone()));
    blobs.insert(
        entry.path.clone(),
        Blob {
            sha,
            mode: entry.mode.clone(),
            content: text,
        },
    );
    Ok(())
}

/// The walk's result once its thread finishes, `None` when it died.
pub(crate) type Walked<T, E> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Option<std::result::Result<T, E>>> + Send>>;

/// A repository-reading walk running on its own thread — `spawn_blocking`
/// would keep a paused test clock from firing the timers the answering
/// side awaits — with its blob reads requested over `requests` and
/// answered through `answers`; `done` resolves with the walk's result.
pub(crate) struct Walk<Req, Ans, T, E> {
    /// Each batch of entries the walk wants contents for.
    pub requests: tokio::sync::mpsc::UnboundedReceiver<Req>,
    /// Where the walk's read waits for each batch's answer.
    pub answers: std::sync::mpsc::Sender<std::result::Result<Ans, String>>,
    /// The walk's result, or `None` when its thread died.
    pub done: Walked<T, E>,
}

/// Start `walk` on its own thread; see [`Walk`]. `read` sends each request
/// and waits for its answer, failing only when the answering side is gone.
pub(crate) fn blocking_walk<Req, Ans, T, E>(
    walk: impl FnOnce(
        &mut dyn FnMut(Req) -> std::result::Result<Ans, String>,
    ) -> std::result::Result<T, E>
    + Send
    + 'static,
) -> Walk<Req, Ans, T, E>
where
    Req: Send + 'static,
    Ans: Send + 'static,
    T: Send + 'static,
    E: Send + 'static,
{
    let (requests, incoming) = tokio::sync::mpsc::unbounded_channel::<Req>();
    let (answers, replies) = std::sync::mpsc::channel::<std::result::Result<Ans, String>>();
    let (done, walked) = tokio::sync::oneshot::channel::<std::result::Result<T, E>>();
    std::thread::spawn(move || {
        let mut read = move |request: Req| -> std::result::Result<Ans, String> {
            requests
                .send(request)
                .map_err(|_| "repository reader closed".to_owned())?;
            replies
                .recv()
                .map_err(|_| "repository reader closed".to_owned())?
        };
        let _ = done.send(walk(&mut read));
    });
    Walk {
        requests: incoming,
        answers,
        done: Box::pin(async move { walked.await.ok() }),
    }
}

/// `dir`'s listing as a push would carry it: what `git ls-files` lists as
/// tracked or untracked but not ignored, so `.gitignore`,
/// `.git/info/exclude` and the global excludes apply, or the whole tree
/// walked when it is not in a Git work tree or Git is not installed,
/// skipping `.git`. A file below a symlinked directory, a symlink, and
/// anything that is not a regular file are not entries: the first is
/// `hidden`, the second an entry of mode `120000`, the rest `hidden`.
pub fn list_local(dir: &Path) -> Result<Listing> {
    let listed = match git_files(dir).map_err(|e| Error::Invalid(e.to_string()))? {
        Some(listed) => listed,
        None => walk(dir).map_err(|e| Error::Invalid(e.to_string()))?,
    };
    let mut entries = Vec::new();
    let mut hidden = Vec::new();
    for relative in listed {
        // Deleted tracked files, files below a symlinked directory, and
        // anything that is not a file here are not files the tree carries:
        // a file below a symlink would be read, and written, outside the
        // tree.
        let below_symlink = Path::new(&relative)
            .ancestors()
            .skip(1)
            .filter(|parent| !parent.as_os_str().is_empty())
            .any(|parent| {
                std::fs::symlink_metadata(dir.join(parent)).is_ok_and(|meta| meta.is_symlink())
            });
        if below_symlink {
            hidden.push(relative);
            continue;
        }
        match std::fs::symlink_metadata(dir.join(&relative)) {
            Ok(meta) if meta.is_symlink() => entries.push(Entry {
                path: relative,
                sha: None,
                mode: "120000".into(),
            }),
            Ok(meta) if meta.is_file() => entries.push(Entry {
                path: relative,
                sha: None,
                mode: local_mode(&meta),
            }),
            _ => hidden.push(relative),
        }
    }
    Ok(Listing { entries, hidden })
}

/// The mode Git would record for a regular file: `100755` when the owner
/// execute bit is set, as `git add` decides it.
#[cfg(unix)]
fn local_mode(meta: &std::fs::Metadata) -> String {
    use std::os::unix::fs::PermissionsExt;
    if meta.permissions().mode() & 0o100 != 0 {
        "100755".into()
    } else {
        "100644".into()
    }
}
#[cfg(not(unix))]
fn local_mode(_: &std::fs::Metadata) -> String {
    "100644".into()
}

/// The local directory's view: its listing, with blob reads from disk.
pub fn local_view(dir: &Path, scanner: &Scanner, config: &Config) -> Result<View> {
    let listing = list_local(dir)?;
    view(
        &mut |entries: &[Entry]| -> Contents {
            Ok(entries
                .iter()
                .map(|entry| {
                    std::fs::read(dir.join(&entry.path)).map_err(|error| error.to_string())
                })
                .collect())
        },
        listing,
        scanner,
        config,
    )
}

/// The files Git would carry from this directory, relative to it, or none
/// when it is not in a Git work tree or Git is not installed.
fn git_files(dir: &Path) -> std::io::Result<Option<Vec<String>>> {
    let output = match std::process::Command::new("git")
        .env("LC_ALL", "C")
        .arg("-C")
        .arg(dir)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("not a git repository") {
            return Ok(None);
        }
        return Err(std::io::Error::other(format!(
            "git ls-files failed: {}",
            stderr.trim()
        )));
    }
    Ok(Some(
        output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| String::from_utf8_lossy(path).into_owned())
            .collect(),
    ))
}

/// Every file below a directory outside Git, skipping `.git`.
fn walk(dir: &Path) -> std::io::Result<Vec<String>> {
    let mut listed = Vec::new();
    let mut pending = vec![dir.to_owned()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                if entry.file_name() != ".git" {
                    pending.push(path);
                }
                continue;
            }
            listed.push(
                path.strip_prefix(dir)
                    .expect("walked below the root")
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    Ok(listed)
}
