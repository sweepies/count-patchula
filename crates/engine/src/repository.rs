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

/// The Git tree modes of a symlink, an executable file and a plain file.
pub(crate) const SYMLINK_MODE: &str = "120000";
pub(crate) const EXECUTABLE_MODE: &str = "100755";
pub(crate) const FILE_MODE: &str = "100644";

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
        .filter(|entry| entry.mode == SYMLINK_MODE)
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
        if entry.mode == SYMLINK_MODE {
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
                Some(entry) if entry.mode != SYMLINK_MODE => wanted.push((*entry).clone()),
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
                mode: SYMLINK_MODE.into(),
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
        EXECUTABLE_MODE.into()
    } else {
        FILE_MODE.into()
    }
}
#[cfg(not(unix))]
fn local_mode(_: &std::fs::Metadata) -> String {
    FILE_MODE.into()
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
            mode: FILE_MODE.into(),
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

    /// A selected file the source cannot read refuses the repository: it is
    /// failed, not dropped, so its dependencies are not silently absent.
    #[tokio::test]
    async fn a_primary_read_failure_refuses_the_view() {
        let config = config("amends \"PatchulaConfig.pkl\"\n");
        let scanner = Scanner::new(&config).unwrap();
        let mut source = Fixed(BTreeMap::new());
        let listing = Listing {
            entries: vec![entry("patchula.pkl"), entry("package.json")],
            hidden: Vec::new(),
        };
        let Err(error) = view(&mut source, listing, &scanner, &config).await else {
            panic!("the view succeeded")
        };
        assert_eq!(
            error.to_string(),
            "cannot read package.json: no package.json"
        );
    }

    /// A registry-settings file the source cannot read stays unreadable
    /// rather than failing the repository: its packages' lookups fail later
    /// instead of reaching the public registry. The same holds when its
    /// bytes are not text.
    #[tokio::test]
    async fn an_unreadable_or_binary_settings_file_is_unreadable_not_refused() {
        let config = config("amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"npm\" }\n");
        let scanner = Scanner::new(&config).unwrap();
        // A read error and non-UTF-8 bytes land the same way.
        for content in [None, Some(b"registry=\xff".to_vec())] {
            let mut map = BTreeMap::new();
            map.insert(
                "package.json".to_owned(),
                b"{\"dependencies\":{\"left-pad\":\"1.0.0\"}}".to_vec(),
            );
            if let Some(bytes) = &content {
                map.insert(".npmrc".into(), bytes.clone());
            }
            let mut source = Fixed(map);
            let listing = Listing {
                entries: vec![entry("package.json"), entry(".npmrc")],
                hidden: Vec::new(),
            };
            let view = view(&mut source, listing, &scanner, &config).await.unwrap();
            assert_eq!(
                view.files.get(".npmrc"),
                Some(&Text::Unread(Unread::Failed)),
                "{content:?}"
            );
        }
    }

    /// A path the include chain names that the listing only saw as hidden —
    /// below a symlinked directory — is unreadable rather than absent, so its
    /// lookups fail saying why.
    #[tokio::test]
    async fn a_named_path_without_a_tree_entry_is_unreadable() {
        let config = config("amends \"PatchulaConfig.pkl\"\n");
        let scanner = Scanner::new(&config).unwrap();
        let mut map = BTreeMap::new();
        map.insert(
            "settings.gradle.kts".to_owned(),
            b"includeBuild(\"tools/logic\")\n".to_vec(),
        );
        let mut source = Fixed(map);
        let listing = Listing {
            entries: vec![entry("settings.gradle.kts")],
            hidden: vec![Hidden {
                path: "tools/logic/src/main/kotlin/Repos.kt".into(),
                resolves: false,
            }],
        };
        let view = view(&mut source, listing, &scanner, &config).await.unwrap();
        assert_eq!(
            view.files.get("tools/logic/src/main/kotlin/Repos.kt"),
            Some(&Text::Unread(Unread::Failed)),
            "{:?}",
            view.files
        );
    }

    /// When the include chain still names files after the round limit, they
    /// are marked too deep and warned about, so their lookups say why.
    #[tokio::test]
    async fn a_chain_past_the_round_limit_is_marked_too_deep() {
        let config = config("amends \"PatchulaConfig.pkl\"\n");
        let scanner = Scanner::new(&config).unwrap();
        let mut map = BTreeMap::new();
        let mut entries = Vec::new();
        // An `apply from:` chain of Groovy scripts longer than MORE_ROUNDS:
        // .groovy is not selected outright, so each script names the next
        // only once read, and the last rounds' names are never read.
        let depth = MORE_ROUNDS + 2;
        map.insert(
            "settings.gradle.kts".into(),
            b"apply from: \"a1.groovy\"\n".to_vec(),
        );
        entries.push(entry("settings.gradle.kts"));
        for i in 1..=depth {
            let path = format!("a{i}.groovy");
            let next = format!("apply from: \"a{}.groovy\"\n", i + 1);
            map.insert(path.clone(), next.into_bytes());
            entries.push(entry(&path));
        }
        let mut source = Fixed(map);
        let listing = Listing {
            entries,
            hidden: Vec::new(),
        };
        let view = view(&mut source, listing, &scanner, &config).await.unwrap();
        let deep: Vec<_> = view
            .files
            .iter()
            .filter(|(_, text)| matches!(text, Text::Unread(Unread::TooDeep)))
            .map(|(path, _)| path.clone())
            .collect();
        assert!(!deep.is_empty(), "{:?}", view.files);
        for path in &deep {
            assert!(
                view.warnings
                    .iter()
                    .any(|w| w.contains(path) && w.contains("include chain")),
                "{path}: {:?}",
                view.warnings
            );
        }
    }

    /// More YAML manifests than the delivery scan takes leaves delivery
    /// unknown: a warning, and no manifest read attempted.
    #[tokio::test]
    async fn more_yaml_than_the_manifest_limit_leaves_delivery_unknown() {
        let config = config(
            "amends \"PatchulaConfig.pkl\"\nrules = new Listing {}\nmanagers = new Listing { \"bundler\" }\nprs { batchMerge = true }\n",
        );
        let scanner = Scanner::new(&config).unwrap();
        let entries: Vec<_> = (0..=tooling::MANIFEST_LIMIT)
            .map(|i| entry(&format!("deploy/{i}.yaml")))
            .collect();
        let mut source = Fixed(BTreeMap::new());
        let listing = Listing {
            entries,
            hidden: Vec::new(),
        };
        let view = view(&mut source, listing, &scanner, &config).await.unwrap();
        assert!(
            view.warnings.iter().any(|w| w
                == &format!(
                    "delivery unknown: more than {} YAML files",
                    tooling::MANIFEST_LIMIT
                )),
            "{:?}",
            view.warnings
        );
    }
}
