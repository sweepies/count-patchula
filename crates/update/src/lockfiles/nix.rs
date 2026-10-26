//! `flake.lock` through the real nix: a pinned static build from the tool
//! store, run with a private chroot store (`--store local?root=…`), its own
//! HOME and configuration, no daemon and no root. Only the lock is written;
//! the inputs nix fetches land in the private store, which `prune` empties.
use super::*;
use serde_json::Value;
use std::{future::Future, pin::Pin};

/// The static nix the Count runs: Hydra's `buildStatic` binary of a release, for
/// each Linux architecture, pinned by build and checksum.
const VERSION: &str = "2.35.5";
const BUILDS: &[(&str, &str, &str)] = &[
    (
        "x86_64",
        "https://hydra.nixos.org/build/350797336/download/2/nix",
        "5d580d993636e3b987868881776214a7d4a169434715ec30bf9ead9345d78fb6",
    ),
    (
        "aarch64",
        "https://hydra.nixos.org/build/350556376/download/2/nix",
        "555348b1bf163414bfedc367633af3d85144c224b6be0084ec8a4e0555fbb0d1",
    ),
];
/// Flake references the Count lets nix fetch. Local paths, `file://`, ssh and
/// plain http are refused; so is anything a flag could be mistaken for.
const SCHEMES: &[&str] = &[
    "github:",
    "gitlab:",
    "sourcehut:",
    "flake:",
    "git+https://",
    "tarball+https://",
    "file+https://",
];

/// The mise spec for the static nix, or why this host has none.
pub(super) fn tools() -> Result<Vec<(String, String)>, String> {
    if std::env::consts::OS != "linux" {
        return Err(
            "flake.lock is regenerated only on Linux, where a static nix build exists".into(),
        );
    }
    let (_, url, sha256) = BUILDS
        .iter()
        .find(|(arch, ..)| *arch == std::env::consts::ARCH)
        .ok_or_else(|| format!("no static nix build for {}", std::env::consts::ARCH))?;
    Ok(vec![(
        format!("http:nix[url={url},checksum=sha256:{sha256},bin=nix]"),
        VERSION.into(),
    )])
}

/// The direct inputs of a lock: input name to its node.
/// Why an input `flake.nix` declares, or any input override nested in it, is
/// refused.
fn declared_refusal(input: &Value, directory: &str) -> Option<String> {
    let dir = Some(directory);
    match input {
        Value::String(reference) => patchula_manager::flake_reference_refusal(reference, dir),
        Value::Object(attrs) => attrs
            .get("url")
            .and_then(Value::as_str)
            .and_then(|url| patchula_manager::flake_reference_refusal(url, dir))
            .or_else(|| {
                attrs
                    .contains_key("type")
                    .then(|| patchula_manager::flake_input_refusal(attrs, dir))
                    .flatten()
            })
            .or_else(|| {
                attrs
                    .get("inputs")?
                    .as_object()?
                    .iter()
                    .find_map(|(name, nested)| {
                        declared_refusal(nested, directory)
                            .map(|reason| format!("{name}: {reason}"))
                    })
            }),
        _ => None,
    }
}

fn inputs(lock: &Value) -> BTreeMap<String, &Value> {
    let nodes = &lock["nodes"];
    let root = lock["root"].as_str().unwrap_or("root");
    nodes[root]["inputs"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(name, node)| Some((name.clone(), &nodes[node.as_str()?])))
        .collect()
}

impl Runner {
    /// The private store and fetcher cache, kept across runs in the tool store.
    fn nix_dir(&self) -> PathBuf {
        self.mise_data.join("nix")
    }
    /// Run the pinned nix against the private store. Nothing is read from the
    /// operator: no system or user nix.conf, no daemon socket, no flake
    /// `nixConfig`. A GitHub token goes only into nix's per-host `access-tokens`.
    async fn nix(
        &self,
        driver: &Path,
        cwd: Option<PathBuf>,
        args: Vec<String>,
        token: Option<&str>,
    ) -> Result<Vec<u8>, String> {
        let conf = driver.join("nix-conf");
        std::fs::create_dir_all(&conf).map_err(|e| e.to_string())?;
        std::fs::write(conf.join("nix.conf"), "").map_err(|e| e.to_string())?;
        let path = |p: &Path| p.to_string_lossy().into_owned();
        let mut config = String::from(
            "experimental-features = nix-command flakes\naccept-flake-config = false\nmax-jobs = 0\nwarn-dirty = false\n",
        );
        if let Some(token) = token.filter(|t| !t.is_empty()) {
            config.push_str(&format!("access-tokens = github.com={token}\n"));
        }
        let store = self.nix_dir();
        std::fs::create_dir_all(store.join("root")).map_err(|e| e.to_string())?;
        let mut call = vec![
            "--store".to_string(),
            format!("local?root={}", path(&store.join("root"))),
        ];
        call.extend(args);
        let tools = tools()?;
        let _installing = if self.installed(&tools) {
            None
        } else {
            Some(self.nix_install.lock().await)
        };
        self.tool(
            driver,
            Invocation {
                tools,
                bin: "nix".into(),
                cwd,
                env: vec![
                    ("NIX_CONF_DIR".into(), path(&conf)),
                    ("NIX_USER_CONF_FILES".into(), path(&conf.join("nix.conf"))),
                    ("XDG_CONFIG_DIRS".into(), path(&conf)),
                    ("XDG_CACHE_HOME".into(), path(&store.join("cache"))),
                    ("NIX_CONFIG".into(), config),
                ],
                args: call,
            },
        )
        .await
    }
    async fn nix_token(&self) -> Result<String, String> {
        match &self.credentials {
            Some(credentials) => credentials
                .token()
                .await
                .map_err(|_| "GitHub authentication failed".to_owned()),
            None => Ok(self.token.clone()),
        }
    }
    /// `nix flake update <inputs>` for updates, or bare for maintenance, in the
    /// flake's directory. The lock is diffed afterwards: each requested input
    /// must be locked at its approved target, and no node may point where the
    /// Count refuses to fetch.
    pub(super) async fn flake_lock(
        &self,
        driver: &Path,
        root: &Path,
        baseline: &BTreeMap<String, Blob>,
        directory: &str,
        jobs: &[&Job],
        maintenance: bool,
    ) -> Result<(), String> {
        let lock = node::join(directory, "flake.lock");
        let before: Value = serde_json::from_str(
            &baseline
                .get(&lock)
                .ok_or_else(|| format!("missing {lock}"))?
                .content,
        )
        .map_err(|e| format!("{lock}: {e}"))?;
        let before_inputs = inputs(&before);
        let refused: BTreeSet<&String> = before_inputs
            .iter()
            .filter(|(_, node)| {
                ["original", "locked"].iter().any(|key| {
                    node[key].as_object().is_some_and(|attrs| {
                        patchula_manager::flake_input_refusal(attrs, Some(directory)).is_some()
                    })
                })
            })
            .map(|(name, _)| name)
            .collect();
        let requested: BTreeSet<&String> = if maintenance {
            before_inputs
                .keys()
                .filter(|name| !refused.contains(name))
                .collect()
        } else {
            jobs.iter().filter_map(|j| j.package.as_ref()).collect()
        };
        if let Some(name) = requested
            .iter()
            .find(|name| !before_inputs.contains_key(**name) || refused.contains(*name))
        {
            return Err(format!("flake input {name} cannot be updated by the Count"));
        }
        // nix fetches what flake.nix declares, not what the lock holds, so the
        // declared inputs are checked before it runs. Restricted evaluation
        // reads only the flake's directory and fetches nothing.
        let flake = root.join(directory);
        let declared = self
            .nix(
                driver,
                None,
                vec![
                    "eval".into(),
                    "--json".into(),
                    "--impure".into(),
                    "--option".into(),
                    "restrict-eval".into(),
                    "true".into(),
                    "-I".into(),
                    format!("flake={}", flake.display()),
                    "--expr".into(),
                    "(import <flake/flake.nix>).inputs or { }".into(),
                ],
                None,
            )
            .await?;
        let declared: Value = serde_json::from_slice(&declared)
            .map_err(|e| format!("{}: {e}", node::join(directory, "flake.nix")))?;
        for (name, input) in declared.as_object().into_iter().flatten() {
            // An input the lock already refuses is left alone above, as long
            // as flake.nix still points it at the same place.
            let unchanged = refused.contains(name)
                && before_inputs
                    .get(name)
                    .and_then(|node| node["original"].as_object())
                    .and_then(patchula_manager::flake_input_reference)
                    .is_some_and(|reference| {
                        input.as_str().or_else(|| input["url"].as_str()) == Some(reference.as_str())
                    });
            if !unchanged && let Some(reason) = declared_refusal(input, directory) {
                return Err(format!(
                    "{}: input {name}: {reason}",
                    node::join(directory, "flake.nix")
                ));
            }
        }
        let mut args = vec![
            "flake".to_string(),
            "update".into(),
            "--flake".into(),
            format!("path:{}", flake.display()),
        ];
        // Maintenance updates every input, naming them only when some are
        // refused, so nix never fetches those.
        if !maintenance || !refused.is_empty() {
            if requested.is_empty() {
                return Ok(());
            }
            args.push("--".into());
            args.extend(requested.iter().map(|name| (*name).clone()));
        }
        let token = self.nix_token().await?;
        self.nix(driver, Some(flake.clone()), args, Some(&token))
            .await?;
        let after: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join(&lock)).map_err(|e| format!("{lock}: {e}"))?,
        )
        .map_err(|e| format!("{lock}: {e}"))?;
        let after_inputs = inputs(&after);
        let root_node = after["root"].as_str().unwrap_or("root");
        let direct: BTreeSet<&str> = after["nodes"][root_node]["inputs"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(_, node)| node.as_str())
            .collect();
        // Only what nix wrote is checked; nodes the repository already had are
        // its own business, and inputs refused above were never fetched.
        for (name, node) in after["nodes"].as_object().into_iter().flatten() {
            if before["nodes"].get(name) == Some(node) {
                continue;
            }
            for key in ["original", "locked"] {
                let Some(attrs) = node[key].as_object() else {
                    continue;
                };
                let dir = direct.contains(name.as_str()).then_some(directory);
                if let Some(reason) = patchula_manager::flake_input_refusal(attrs, dir) {
                    return Err(format!("{lock}: node {name}: {reason}"));
                }
            }
        }
        // An update must lock the revision that passed lookup and its
        // release-age wait; a branch that moved since is not published.
        for job in jobs {
            let (Some(name), Some(target)) = (&job.package, &job.target) else {
                continue;
            };
            let locked = after_inputs.get(name).map(|n| &n["locked"]);
            let got = locked.and_then(|l| l["rev"].as_str().or_else(|| l["narHash"].as_str()));
            if got != Some(target.as_str()) {
                return Err(format!(
                    "nix locked flake input {name} at {}, not the approved {target}: it moved after the lookup",
                    got.unwrap_or("nothing")
                ));
            }
        }
        Ok(())
    }
    /// Empty the private nix store: nothing in it is a GC root, since it only
    /// caches fetched inputs, so `nix store gc` removes every path no running nix
    /// holds. The fetcher cache goes too. Returns whether there was a store.
    pub(super) async fn prune_nix(&self) -> Result<bool, String> {
        let dir = self.nix_dir();
        if !dir.exists() {
            return Ok(false);
        }
        if let Ok(tools) = tools()
            && self.installed(&tools)
        {
            let driver = tempfile::tempdir().map_err(|e| e.to_string())?;
            self.nix(driver.path(), None, vec!["store".into(), "gc".into()], None)
                .await?;
        }
        match std::fs::remove_dir_all(dir.join("cache")) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(true),
        }
    }
}

impl patchula_datasource::Flakes for Runner {
    fn prefetch<'a>(
        &'a self,
        reference: &'a str,
        token: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + 'a>> {
        Box::pin(async move {
            if !SCHEMES.iter().any(|s| reference.starts_with(s))
                || reference
                    .chars()
                    .any(|c| c.is_whitespace() || c.is_control())
            {
                return Err(format!("flake reference {reference:?} is not fetched"));
            }
            let driver = tempfile::tempdir().map_err(|e| e.to_string())?;
            self.nix(
                driver.path(),
                None,
                vec![
                    "flake".into(),
                    "prefetch".into(),
                    "--json".into(),
                    "--refresh".into(),
                    "--".into(),
                    reference.into(),
                ],
                token,
            )
            .await
        })
    }
}
