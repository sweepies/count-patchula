//! Reading a repository's files as one engine. A push of the tree would
//! carry `entries`; the scan reads the ones the config selects, follows
//! what they name for a bounded number of rounds, marks what it cannot
//! read unreadable rather than absent, refuses committed credentials, and
//! hands extraction the files, the path inventory, the unsupported list,
//! the snapshot blobs and the warnings. GitHub and a local checkout differ
//! only in where the listing and the blob contents come from: `view` is
//! handed a [`Listing`] and a [`Source`].
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

/// A path the listing saw that is not a file the tree carries.
pub struct Hidden {
    /// The path, relative to the repository root.
    pub path: String,
    /// Whether it resolves on disk, following whatever links stand in its
    /// way: false for a tracked file deleted from the working tree, which
    /// is absent rather than unreadable.
    pub resolves: bool,
}

/// A repository's file listing.
pub struct Listing {
    /// Every file the tree carries, symlinks included.
    pub entries: Vec<Entry>,
    /// Paths the listing saw that are not files the tree carries: below a
    /// symlinked directory, or not a regular file at all. Always empty for
    /// GitHub, whose trees hold no such paths.
    pub hidden: Vec<Hidden>,
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
    pub warnings: Vec<String>,
    /// Why the repository is refused for committed credentials, if it is.
    pub refusal: Option<String>,
}

/// Where a view's blob contents come from.
pub trait Source {
    /// The contents of `entries`, one result per entry in order: an outer
    /// error fails the whole read, an inner one is that entry's own
    /// failure. Only ever given non-symlink entries.
    fn read<'a>(
        &'a mut self,
        entries: &'a [Entry],
    ) -> impl std::future::Future<Output = Contents> + Send + 'a;
}

/// Read `listing` for `scanner` and `config` through `source`, awaiting it
/// once per selection and once per round of the include chain.
pub async fn view<S: Source>(
    source: &mut S,
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
    let hidden_paths: BTreeSet<String> = hidden.iter().map(|h| h.path.clone()).collect();
    // Flux and Argo CD manifests are YAML anywhere in the tree, so the
    // delivery warning reads every YAML file, unless the tree has more
    // than it will take, when delivery stays unknown. Only a config
    // that can merge on its own and hasn't silenced the warning pays
    // for those reads.
    let delivery = config.may_merge()
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
        .chain(
            hidden
                .iter()
                .filter(|h| h.resolves)
                .map(|h| h.path.as_str()),
        )
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
    let contents = source.read(&wanted).await?;
    for (entry, content) in wanted.iter().zip(contents) {
        place(entry, content, true, &mut files, &mut blobs)?;
    }
    for settings in &linked {
        files.insert(settings.clone(), Text::Unread(Unread::Failed));
    }
    // What the files read so far name, such as the sources of a Gradle
    // build the settings include, until they name nothing new.
    for _ in 0..MORE_ROUNDS {
        let more = scanner.more(
            &files,
            paths.iter().chain(&hidden_paths).map(String::as_str),
        );
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
        let contents = source.read(&wanted).await?;
        for (entry, content) in wanted.iter().zip(contents) {
            place(entry, content, false, &mut files, &mut blobs)?;
        }
    }
    // What the chain still names when the rounds run out is unreadable the
    // same way, so its lookups fail saying why.
    let deep = scanner.more(
        &files,
        paths.iter().chain(&hidden_paths).map(String::as_str),
    );
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
        // A registry-settings file that cannot be read is unreadable, so
        // its packages' lookups fail rather than go to a public registry.
        Err(reason) if primary && !registry_settings(&entry.path) => {
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
            let resolves = std::fs::metadata(dir.join(&relative)).is_ok();
            hidden.push(Hidden {
                path: relative,
                resolves,
            });
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
            _ => {
                let resolves = std::fs::metadata(dir.join(&relative)).is_ok();
                hidden.push(Hidden {
                    path: relative,
                    resolves,
                });
            }
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

/// The local working tree's blob source: files are read synchronously
/// inside the async read, which only the CLI's local path uses.
struct Local<'a> {
    dir: &'a Path,
}

impl Source for Local<'_> {
    async fn read(&mut self, entries: &[Entry]) -> Contents {
        Ok(entries
            .iter()
            .map(|entry| {
                std::fs::read(self.dir.join(&entry.path)).map_err(|error| error.to_string())
            })
            .collect())
    }
}

/// The local directory's view: its listing, with blob reads from disk.
pub async fn local_view(dir: &Path, scanner: &Scanner, config: &Config) -> Result<View> {
    let listing = list_local(dir)?;
    let mut source = Local { dir };
    view(&mut source, listing, scanner, config).await
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
            .map(|path| {
                std::str::from_utf8(path).map(str::to_owned).map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "repository path is not UTF-8",
                    )
                })
            })
            .collect::<std::io::Result<Vec<_>>>()?,
    ))
}

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
                    .to_str()
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("repository path is not UTF-8: {}", path.display()),
                        )
                    })?
                    .replace('\\', "/"),
            );
        }
    }
    Ok(listed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A source that answers every asked entry with fixed bytes.
    struct Fixed(BTreeMap<String, Vec<u8>>);
    impl Source for Fixed {
        async fn read(&mut self, entries: &[Entry]) -> Contents {
            Ok(entries
                .iter()
                .map(|entry| {
                    self.0
                        .get(&entry.path)
                        .cloned()
                        .ok_or_else(|| format!("no {}", entry.path))
                })
                .collect())
        }
    }
    fn entry(path: &str) -> Entry {
        Entry {
            path: path.into(),
            sha: Some(path.into()),
            mode: "100644".into(),
        }
    }
    fn config(pkl: &str) -> Config {
        patchula_config::load_files(
            "/repo/patchula.pkl",
            BTreeMap::from([("/repo/patchula.pkl".into(), pkl.into())]),
        )
        .unwrap()
    }
    /// The delivery warning reads YAML manifests only when the config has a
    /// merge path; the batch PR's merge counts. Emptying `rules` drops the
    /// built-in merging rule and `managers` the YAML-selecting ones, leaving
    /// `batchMerge` as the only merge path.
    #[tokio::test]
    async fn batch_merge_reads_delivery_manifests() {
        let mut source = Fixed(BTreeMap::from([(
            "deploy/app.yaml".into(),
            b"apiVersion: v1\n".to_vec(),
        )]));
        for (pkl, read) in [
            (
                "amends \"PatchulaConfig.pkl\"\nrules = new Listing {}\nmanagers = new Listing { \"bundler\" }\nprs { batchMerge = true }\n",
                true,
            ),
            (
                "amends \"PatchulaConfig.pkl\"\nrules = new Listing {}\nmanagers = new Listing { \"bundler\" }\n",
                false,
            ),
        ] {
            let config = config(pkl);
            assert_eq!(config.may_merge(), read, "may_merge {pkl}");
            let scanner = Scanner::new(&config).unwrap();
            let listing = Listing {
                entries: vec![entry("deploy/app.yaml"), entry("src/main.rs")],
                hidden: Vec::new(),
            };
            let view = view(&mut source, listing, &scanner, &config).await.unwrap();
            assert_eq!(view.blobs.contains_key("deploy/app.yaml"), read, "{pkl}");
            assert!(!view.blobs.contains_key("src/main.rs"));
        }
    }
}
