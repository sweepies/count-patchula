//! The file-role taxonomy: what role a repository file name plays for each
//! ecosystem, stated once here and next to the predicates each ecosystem
//! module owns, and read by every classifier that needs a file's role.
//! Everything fails closed: a name no ecosystem claims
//! has no role, so it is never materialized for a tool run and never public.
//! A file that may hold a committed credential gets a `credential_scan`.

use crate::credentials::CredentialScan;
use patchula_words::Manager;

/// The lockfile names one manager's ecosystem recognizes: `supported` are
/// regenerated in place by the ecosystem's tool, `unsupported` are names
/// the tool writes in a format the runner does not regenerate.
#[derive(Clone, Copy, Debug, Default)]
pub struct LockNames {
    pub supported: &'static [&'static str],
    pub unsupported: &'static [&'static str],
    /// Supported locks whose text the Count parses back, where that
    /// differs from `supported`.
    pub readable: &'static [&'static str],
}

/// The roles one file name can have for one manager's ecosystem.
#[derive(Clone, Copy, Debug, Default)]
pub struct EcoRoles {
    /// A settings file read whatever `files`/`exclude` select.
    pub registry: bool,
    /// Can say where packages come from.
    pub settings: bool,
    /// The credential check this name's text gets, `Some` exactly when the
    /// file may hold a committed credential: `committed_credentials` runs it,
    /// so a credential-bearing file cannot lack a check.
    pub(crate) credential_scan: Option<CredentialScan>,
    /// A lockfile tool may modify it.
    pub writable: bool,
    /// A lockfile tool reads it but never modifies it.
    pub read: bool,
    /// It can steer what this ecosystem's tools fetch.
    pub steers: bool,
}

/// The roles a file name has across every ecosystem: the union, since a
/// path does not say which manager reads it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Roles {
    /// Can say where packages come from.
    pub settings: bool,
    /// May hold a committed credential.
    pub credentials: bool,
    /// A lockfile tool may modify it.
    pub writable: bool,
    /// A lockfile tool reads it but never modifies it.
    pub read: bool,
}

/// The last segment of `path`: its file name.
pub fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub(crate) fn named(names: &[&str], path: &str) -> bool {
    names.contains(&leaf(path))
}

fn suffixed(extensions: &[&str], path: &str) -> bool {
    extensions.iter().any(|e| path.ends_with(e))
}

/// The credential scan `path` gets among `choices`' `(names, scan)` pairs.
fn scanned(path: &str, choices: &[(&[&str], CredentialScan)]) -> Option<CredentialScan> {
    choices
        .iter()
        .find(|(names, _)| named(names, path))
        .map(|(_, scan)| *scan)
}

pub(crate) const NPM_LOCKS: &[&str] = &[
    "package-lock.json",
    "npm-shrinkwrap.json",
    "aube-lock.yaml",
    "pnpm-lock.yaml",
    "yarn.lock",
];
pub(crate) const NODE_SETTINGS: &[&str] = &[".npmrc", ".yarnrc.yml"];
pub(crate) const NODE_MANIFEST: &[&str] = &["package.json"];
pub(crate) const PYTHON_LOCKS: &[&str] = &["uv.lock", "poetry.lock"];
pub(crate) const PNPM_WORKSPACE: &[&str] = &["pnpm-workspace.yaml"];
pub(crate) const BUN_LOCK: &[&str] = &["bun.lock"];
/// Python's settings files: `pyproject.toml`'s tool tables and the tool's
/// own config, including `poetry.toml`, which `poetry lock` reads for its
/// repositories. `pdm.toml`, `pixi.toml` and `hatch.toml` are only checked
/// for the index keys they may name.
pub(crate) const PYTHON_SETTINGS: &[&str] = &[
    "pyproject.toml",
    "uv.toml",
    "poetry.toml",
    "pdm.toml",
    "pixi.toml",
    "hatch.toml",
];
/// pip's own configuration, which `settings` covers but no index lookup
/// reads.
const PIP_SETTINGS: &[&str] = &["pip.conf", "pip.ini"];
/// The Python files a tool run reads but never modifies.
const PYTHON_READ: &[&str] = &[".python-version", "uv.toml", "poetry.toml"];
/// The NuGet JSON manifests that are not MSBuild files.
pub(crate) const NUGET_JSON: &[&str] = &["global.json", "dotnet-tools.json"];
/// The MSBuild targets file NuGet reads settings from.
const NUGET_BUILD_TARGETS: &[&str] = &["Directory.Build.targets"];
/// The wrapper properties file a Gradle run reads.
const GRADLE_WRAPPER: &[&str] = &["gradle-wrapper.properties"];
/// The credential-bearing files and the check each gets. `.npmrc` and
/// `.yarnrc.yml` are npm's settings, which bun reads too;
/// `pnpm-workspace.yaml` is npm's alone.
const NPM_CREDENTIAL_SCANS: &[(&[&str], CredentialScan)] = &[
    (&[".npmrc"], CredentialScan::Npmrc),
    (&[".yarnrc.yml"], CredentialScan::Yarnrc),
    (PNPM_WORKSPACE, CredentialScan::PnpmWorkspace),
];
const BUN_CREDENTIAL_SCANS: &[(&[&str], CredentialScan)] = &[
    (&[".npmrc"], CredentialScan::Npmrc),
    (&[".yarnrc.yml"], CredentialScan::Yarnrc),
];
/// Python's credential-bearing files and the check each gets.
const PYTHON_CREDENTIAL_SCANS: &[(&[&str], CredentialScan)] = &[
    (&["poetry.toml"], CredentialScan::Poetry),
    (&["pyproject.toml", "uv.toml"], CredentialScan::Python),
    (PIP_SETTINGS, CredentialScan::Pip),
];
/// MSBuild project and solution suffixes NuGet reads.
const NUGET_SUFFIXES: &[&str] = &[".csproj", ".fsproj", ".vbproj", ".sln", ".slnx"];
/// The property and target files that, beside `NUGET_SUFFIXES`, can carry
/// restore sources.
const NUGET_PROPERTY_SUFFIXES: &[&str] = &[".props", ".targets"];
/// The NuGet files the Count rewrites: its lockfile and the MSBuild files
/// that hold central package management and restore settings.
const NUGET_WRITABLE: &[&str] = &[
    "packages.lock.json",
    "Directory.Packages.props",
    "Directory.Build.props",
    "dotnet-tools.json",
    "global.json",
];

/// The mise files the Count parses for tools, which a tool run reads but
/// never modifies. `mise::config_file` also covers the other names mise
/// reads configuration by (`mise/config.toml`, `.config/mise/conf.d/*.toml`,
/// environment variants); those get `settings` and `registry` only.
pub(crate) const MISE_SELECTED: &[&str] = &["mise.toml", ".mise.toml", ".tool-versions"];

pub fn eco_roles(manager: Manager, path: &str) -> EcoRoles {
    let mut roles = EcoRoles::default();
    match manager {
        Manager::Npm => {
            roles.registry = named(NODE_SETTINGS, path);
            roles.settings = roles.registry;
            roles.credential_scan = scanned(path, NPM_CREDENTIAL_SCANS);
            roles.writable = named(NPM_LOCKS, path) || named(NODE_MANIFEST, path);
            roles.read = named(NODE_SETTINGS, path) || named(PNPM_WORKSPACE, path);
            roles.steers = named(NODE_MANIFEST, path)
                || named(NODE_SETTINGS, path)
                || named(PNPM_WORKSPACE, path)
                || named(NPM_LOCKS, path);
        }
        Manager::Bun => {
            roles.registry = named(NODE_SETTINGS, path);
            roles.settings = roles.registry;
            roles.credential_scan = scanned(path, BUN_CREDENTIAL_SCANS);
            roles.writable = named(BUN_LOCK, path);
            roles.read = named(NODE_SETTINGS, path);
            roles.steers =
                named(NODE_MANIFEST, path) || named(NODE_SETTINGS, path) || named(BUN_LOCK, path);
        }
        Manager::Pyproject => {
            roles.registry = named(PYTHON_SETTINGS, path) || named(PYTHON_LOCKS, path);
            roles.settings = named(PYTHON_SETTINGS, path) || named(PIP_SETTINGS, path);
            roles.credential_scan = scanned(path, PYTHON_CREDENTIAL_SCANS);
            roles.writable =
                named(manifest_names(Manager::Pyproject), path) || named(PYTHON_LOCKS, path);
            roles.read = named(PYTHON_READ, path);
            roles.steers = named(PYTHON_SETTINGS, path) || named(PYTHON_LOCKS, path);
        }
        Manager::Bundler => {
            roles.registry =
                crate::bundle_config(path) || named(lock_names(Manager::Bundler).supported, path);
            roles.settings = crate::bundle_config(path);
            roles.credential_scan =
                named(manifest_names(Manager::Bundler), path).then_some(CredentialScan::Gemfile);
            roles.writable = named(manifest_names(Manager::Bundler), path)
                || named(lock_names(Manager::Bundler).supported, path);
            roles.read = path.ends_with(".gemspec");
            roles.steers = roles.writable || path.ends_with(".gemspec");
        }
        Manager::Composer => {
            roles.registry = named(lock_names(Manager::Composer).supported, path);
            roles.credential_scan =
                named(manifest_names(Manager::Composer), path).then_some(CredentialScan::Composer);
            roles.writable = named(manifest_names(Manager::Composer), path)
                || named(lock_names(Manager::Composer).supported, path);
            roles.steers = roles.writable;
        }
        Manager::Cargo => {
            roles.registry = crate::cargo::config_file(path);
            roles.settings =
                crate::cargo::config_file(path) || named(manifest_names(Manager::Cargo), path);
            roles.credential_scan =
                crate::cargo::config_file(path).then_some(CredentialScan::Cargo);
            roles.writable = named(manifest_names(Manager::Cargo), path)
                || named(lock_names(Manager::Cargo).supported, path);
            roles.read = path.ends_with(".rs");
            roles.steers = roles.writable;
        }
        Manager::Go => {
            roles.registry = crate::go::work_file(path);
            roles.settings = roles.registry;
            roles.writable = named(manifest_names(Manager::Go), path) || named(&["go.sum"], path);
            roles.read = named(&["go.work", "go.work.sum"], path) || path.ends_with(".go");
        }
        Manager::Terraform => {
            roles.writable =
                path.ends_with(".tf") || named(lock_names(Manager::Terraform).supported, path);
        }
        Manager::Nuget => {
            roles.registry = crate::nuget::config_file(path) || crate::nuget::msbuild_file(path);
            roles.settings = roles.registry;
            roles.credential_scan =
                crate::nuget::config_file(path).then_some(CredentialScan::Nuget);
            roles.writable = named(NUGET_WRITABLE, path);
            roles.read = crate::nuget::config_file(path)
                || named(NUGET_BUILD_TARGETS, path)
                || suffixed(NUGET_SUFFIXES, path);
            roles.steers = crate::nuget::config_file(path)
                || named(lock_names(Manager::Nuget).supported, path)
                || named(manifest_names(Manager::Nuget), path)
                || suffixed(NUGET_SUFFIXES, path)
                || suffixed(NUGET_PROPERTY_SUFFIXES, path);
        }
        Manager::Maven => {
            roles.registry = crate::maven::repository_file(path);
            roles.settings = roles.registry;
        }
        Manager::Gradle => {
            roles.registry = crate::gradle::build_logic(path);
            roles.settings = roles.registry;
            roles.writable = named(GRADLE_WRAPPER, path);
        }
        Manager::Mise => {
            // A package backend mise runs in the project reads its settings.
            roles.registry = crate::mise::config_file(path) || crate::mise::backend_settings(path);
            roles.settings = crate::mise::config_file(path);
            roles.read = named(MISE_SELECTED, path);
            roles.writable = named(lock_names(Manager::Mise).supported, path);
        }
        Manager::Nix => {
            roles.writable = named(lock_names(Manager::Nix).supported, path);
            roles.read = named(manifest_names(Manager::Nix), path);
        }
        Manager::Apm => {
            roles.writable = named(lock_names(Manager::Apm).supported, path);
            roles.read = named(manifest_names(Manager::Apm), path);
        }
        Manager::GithubActions => {
            // The Actions run writes the lock; the workflows and action
            // files it is written from are checked out only while the
            // work holds an Actions job (checkout.rs), so nothing here
            // is `read` and `.github/dependabot.yml` stays out.
            roles.writable = path == ACTIONS_LOCK;
            // The lockfile is read into the snapshot without being
            // extracted from: which workflows it lists is the pinning
            // fact, and its `uses:` entries are the tool's.
            roles.registry = roles.writable;
            // A workflow or action file can steer the run's fetches: it
            // names the repositories the tool resolves.
            roles.steers = github_workflow(path) || action_file(path) || roles.writable;
        }
        _ => {}
    }
    roles
}

pub fn roles(path: &str) -> Roles {
    let mut roles = Roles::default();
    for manager in Manager::ALL {
        let eco = eco_roles(*manager, path);
        roles.settings |= eco.settings;
        roles.credentials |= eco.credential_scan.is_some();
        roles.writable |= eco.writable;
        roles.read |= eco.read;
    }
    roles
}

/// A file that can say where packages come from: Python settings,
/// Bundler's settings, a NuGet config, an MSBuild file that can set
/// restore sources, or Maven or Gradle build files that can add
/// repositories. One the Count cannot read, such as a symlink or a file
/// that is not text, must reach the scanner as unreadable, since leaving it
/// out would make its packages the public registry's.
pub fn registry_settings(path: &str) -> bool {
    roles(path).settings
}

pub fn credential_scope(path: &str) -> bool {
    roles(path).credentials
}

/// A Cargo configuration file, read only for its `registries`.
pub fn cargo_config(path: &str) -> bool {
    crate::cargo::config_file(path)
}

/// The GitHub Actions workflow lockfile's one path: not a leaf name
/// matched anywhere in the tree.
pub const ACTIONS_LOCK: &str = ".github/workflows/actions.lock";

/// A workflow file directly under `.github/workflows/` — never nested,
/// and never the lockfile itself. `.forgejo` and `.gitea` workflows are
/// not GitHub Actions' and match nothing here.
pub fn github_workflow(path: &str) -> bool {
    let Some(rest) = path.strip_prefix(".github/workflows/") else {
        return false;
    };
    path != ACTIONS_LOCK
        && !rest.contains('/')
        && matches!(rest.rsplit('.').next(), Some("yml" | "yaml"))
}

/// An in-repository action definition, `action.yml` or `action.yaml` at
/// any depth.
pub fn action_file(path: &str) -> bool {
    matches!(leaf(path), "action.yml" | "action.yaml")
}

/// Whether `path` is a NuGet configuration file, in any letter case.
pub fn nuget_config(path: &str) -> bool {
    crate::nuget::config_file(path)
}

pub fn lock_names(manager: Manager) -> LockNames {
    match manager {
        Manager::Npm => LockNames {
            supported: NPM_LOCKS,
            unsupported: &["bun.lockb"],
            readable: &["package-lock.json", "npm-shrinkwrap.json", "aube-lock.yaml"],
        },
        Manager::Bun => LockNames {
            supported: BUN_LOCK,
            unsupported: &[],
            readable: BUN_LOCK,
        },
        Manager::Pyproject => LockNames {
            supported: PYTHON_LOCKS,
            unsupported: &["pdm.lock", "pylock.toml", "Pipfile.lock"],
            readable: &[],
        },
        Manager::Bundler => LockNames {
            supported: &["Gemfile.lock"],
            unsupported: &[],
            readable: &[],
        },
        Manager::Composer => LockNames {
            supported: &["composer.lock"],
            unsupported: &[],
            readable: &[],
        },
        Manager::Cargo => LockNames {
            supported: &["Cargo.lock"],
            unsupported: &[],
            readable: &[],
        },
        Manager::Nuget => LockNames {
            supported: &["packages.lock.json"],
            unsupported: &[],
            readable: &[],
        },
        Manager::Mise => LockNames {
            supported: &["mise.lock"],
            unsupported: &[],
            readable: &[],
        },
        Manager::Gradle => LockNames {
            supported: &[],
            unsupported: &["gradle.lockfile"],
            readable: &[],
        },
        Manager::Nix => LockNames {
            supported: &["flake.lock"],
            unsupported: &[],
            readable: &[],
        },
        Manager::Terraform => LockNames {
            supported: &[".terraform.lock.hcl"],
            unsupported: &[],
            readable: &[],
        },
        Manager::Apm => LockNames {
            supported: &["apm.lock.yaml"],
            unsupported: &[],
            readable: &[],
        },
        _ => LockNames::default(),
    }
}

/// The manifest leaf names `manager`'s ecosystem reads. Names matched by
/// extension (`.csproj`, `.gemspec`, `*.tf`) cannot be listed here; callers
/// that must cover them check the path's suffix themselves.
pub fn manifest_names(manager: Manager) -> &'static [&'static str] {
    match manager {
        Manager::Npm | Manager::Bun => NODE_MANIFEST,
        Manager::Pyproject => &["pyproject.toml"],
        Manager::Bundler => &["Gemfile"],
        Manager::Composer => &["composer.json"],
        Manager::Cargo => &["Cargo.toml"],
        Manager::Go => &["go.mod"],
        Manager::Nuget => &[
            "global.json",
            "dotnet-tools.json",
            "Directory.Packages.props",
            "Directory.Build.props",
            "Directory.Build.targets",
        ],
        Manager::Mise => &[
            "mise.toml",
            ".mise.toml",
            "mise.local.toml",
            ".miserc.toml",
            ".miserc.local.toml",
            ".tool-versions",
        ],
        Manager::Nix => &["flake.nix"],
        Manager::Apm => &["apm.yml"],
        _ => &[],
    }
}

/// The settings file a symlink at `path` could stand for: the path itself
/// when it is a settings file, or one inside a linked folder the tools
/// follow: `.bundle` for Bundler, `.mvn` for Maven, `.cargo` for Cargo, and
/// Gradle's build-logic folders, whose contents the Count then cannot read.
pub fn linked_settings(path: &str) -> Option<String> {
    if roles(path).settings {
        return Some(path.to_owned());
    }
    let leaf = leaf(path);
    if leaf == ".bundle" {
        return Some(format!("{path}/config"));
    }
    if leaf == ".mvn" {
        return Some(format!("{path}/maven.config"));
    }
    if leaf == ".cargo" {
        return Some(format!("{path}/config.toml"));
    }
    if crate::gradle::logic_folder(path) {
        return Some(format!("{path}/build.gradle.kts"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every file the tools read credentials from is scanned for committed
    /// ones, and every other file a tool run materializes or that steers
    /// what it fetches names here why it is not. The probes are every name
    /// the taxonomy lists plus a path for each predicate, at the root and
    /// nested, so a file that gains a role without a `credential_scan`, or
    /// loses its scan, fails here until it gets one or a reason.
    #[test]
    fn credential_bearing_settings_are_scanned() {
        /// Files whose tools read credentials, or credentialed registry
        /// URLs, from them.
        const SCANNED: &[&str] = &[
            ".npmrc",
            ".yarnrc.yml",
            "pnpm-workspace.yaml",
            "composer.json",
            "pyproject.toml",
            "uv.toml",
            "poetry.toml",
            "pip.conf",
            "pip.ini",
            "Gemfile",
            "nuget.config",
            "NuGet.Config",
            ".cargo/config.toml",
            ".cargo/config",
        ];
        /// Materialized or steering files with no scan, and why. Those
        /// "not scanned yet" can hold credentials their tools read: gaps
        /// in the hygiene check, not in the `public` partition's boundary.
        const UNSCANNED: &[(&[&str], &str)] = &[
            (&["pdm.toml"], "not scanned yet: PDM repository passwords"),
            (
                &[".bundle/config"],
                "not scanned yet: BUNDLE_<HOST> credentials",
            ),
            (&["settings.xml"], "not scanned yet: Maven server passwords"),
            (&["pixi.toml"], "names channels and indexes only"),
            (
                &["hatch.toml"],
                "Hatch reads publish credentials from user config",
            ),
            (
                &[
                    "uv.lock",
                    "poetry.lock",
                    "Gemfile.lock",
                    "composer.lock",
                    "Cargo.lock",
                    "yarn.lock",
                    "package-lock.json",
                    "npm-shrinkwrap.json",
                    "pnpm-lock.yaml",
                    "aube-lock.yaml",
                    "bun.lock",
                    "packages.lock.json",
                    "flake.lock",
                    ".terraform.lock.hcl",
                    "apm.lock.yaml",
                    "mise.lock",
                    ACTIONS_LOCK,
                ],
                "a lockfile: names, versions, hashes and sources",
            ),
            (
                &["Cargo.toml"],
                "Cargo reads registry tokens only from its config",
            ),
            (
                &["go.work", "go.mod", "main.go"],
                "Go reads credentials from .netrc or GOAUTH",
            ),
            (
                &[
                    "package.json",
                    "flake.nix",
                    "apm.yml",
                    "app.gemspec",
                    ".tool-versions",
                    ".python-version",
                ],
                "names packages and versions; its tool reads no credential here",
            ),
            (&["src/main.rs"], "Rust source, out of the check's scope"),
            (
                &["main.tf"],
                "Terraform reads registry credentials from its CLI config",
            ),
            (
                &["gradle-wrapper.properties"],
                "not scanned yet: a user and password in distributionUrl",
            ),
            (
                &[".github/workflows/ci.yml", "action.yml"],
                "CI files, out of the check's scope",
            ),
            (
                &["pom.xml", ".mvn/maven.config"],
                "Maven reads server credentials from settings.xml",
            ),
            (
                &[
                    "build.gradle",
                    "settings.gradle.kts",
                    "buildSrc/src/main/kotlin/Conventions.kt",
                ],
                "build logic, out of the check's scope",
            ),
            (
                &[
                    "App.csproj",
                    "Build.targets",
                    "Directory.Packages.props",
                    "Directory.Build.props",
                    "Directory.Build.targets",
                    "App.fsproj",
                    "App.sln",
                    "Build.props",
                    "global.json",
                    "dotnet-tools.json",
                ],
                "NuGet reads source credentials only from nuget.config",
            ),
            (
                &[
                    "mise.toml",
                    ".mise.toml",
                    "mise.local.toml",
                    ".miserc.toml",
                    ".miserc.local.toml",
                    "mise.production.toml",
                    "mise/config.toml",
                    ".config/mise/config.toml",
                    ".config/mise/conf.d/tools.toml",
                ],
                "environment values, out of the check's scope",
            ),
        ];
        let unscanned = |probe: &str| UNSCANNED.iter().any(|(names, _)| names.contains(&probe));
        // Every file a tool run materializes or that steers its fetches.
        let guarded = |path: &str| {
            let union = roles(path);
            union.settings
                || union.read
                || union.writable
                || Manager::ALL.iter().any(|manager| {
                    let eco = eco_roles(*manager, path);
                    eco.registry || eco.steers
                })
        };
        let mut probes: Vec<&str> = SCANNED.to_vec();
        probes.extend(UNSCANNED.iter().flat_map(|(names, _)| names.iter()));
        probes.extend(NODE_SETTINGS.iter().chain(PNPM_WORKSPACE));
        probes.extend(
            PYTHON_SETTINGS
                .iter()
                .chain(PIP_SETTINGS)
                .chain(PYTHON_READ),
        );
        probes.extend(GRADLE_WRAPPER.iter().chain(NUGET_WRITABLE));
        probes.extend(MISE_SELECTED);
        probes.extend([
            "src/main.rs",
            "main.go",
            "main.tf",
            "app.gemspec",
            "App.sln",
            "App.fsproj",
            "Build.props",
            ".github/workflows/ci.yml",
            "action.yml",
            ACTIONS_LOCK,
        ]);
        for manager in Manager::ALL {
            let locks = lock_names(*manager);
            probes.extend(manifest_names(*manager));
            probes.extend(locks.supported.iter().chain(locks.unsupported));
        }
        for (names, why) in UNSCANNED {
            for name in *names {
                assert!(
                    guarded(name),
                    "{name} is listed unscanned ({why}) but plays no role"
                );
            }
        }
        let mut unlisted = Vec::new();
        for probe in probes {
            for path in [probe.to_owned(), format!("sub/dir/{probe}")] {
                if credential_scope(&path) {
                    assert!(SCANNED.contains(&probe), "{path} is scanned but not listed");
                } else if SCANNED.contains(&probe) {
                    panic!("{path} is not scanned");
                } else if guarded(&path) && !unscanned(probe) {
                    unlisted.push(path);
                }
            }
        }
        unlisted.sort();
        unlisted.dedup();
        assert!(
            unlisted.is_empty(),
            "materialized or steering but neither scanned nor listed unscanned: {unlisted:?}"
        );
    }

    /// Managers the Count only reads through other files name no lockfile
    /// or manifest of their own; Gradle's one lockfile is unsupported.
    #[test]
    fn managers_without_lockfiles_or_manifests_report_none() {
        let go = lock_names(Manager::Go);
        assert!(go.supported.is_empty() && go.unsupported.is_empty() && go.readable.is_empty());
        assert!(
            lock_names(Manager::Gradle)
                .unsupported
                .contains(&"gradle.lockfile")
        );
        assert!(manifest_names(Manager::Gradle).is_empty());
        assert!(manifest_names(Manager::Terraform).is_empty());
        assert!(manifest_names(Manager::GithubActions).is_empty());
    }
}
