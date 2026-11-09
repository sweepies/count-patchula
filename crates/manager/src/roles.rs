//! The file-role taxonomy: what role a repository file name plays for each
//! ecosystem, stated once here and next to the predicates each ecosystem
//! module already owns, and read by every classifier that used to keep its
//! own filename table. Everything fails closed: a name no ecosystem claims
//! has no role, so it is never materialized for a tool run, never public,
//! and credential-checked whenever it could hold one.

use crate::credentials::{CredentialCheck, CredentialScan};
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
    /// A settings file read whatever `files`/`exclude` select: the
    /// ecosystem's `registry_file` answer.
    pub registry: bool,
    /// Can say where packages come from.
    pub settings: bool,
    /// The credential check this name's text gets, `Some` exactly when the
    /// file may hold a committed credential: `credentials::keyed` runs it,
    /// so a credential-bearing file cannot lack a check.
    pub(crate) credential_scan: Option<CredentialScan>,
    /// This ecosystem's credential-check decision for the name.
    pub check: CredentialCheck,
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
    /// Whether its text is checked for committed credentials.
    pub check: CredentialCheck,
    /// A lockfile tool may modify it.
    pub writable: bool,
    /// A lockfile tool reads it but never modifies it.
    pub read: bool,
}

fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn named(names: &[&str], path: &str) -> bool {
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

const NPM_LOCKS: &[&str] = &[
    "package-lock.json",
    "npm-shrinkwrap.json",
    "aube-lock.yaml",
    "pnpm-lock.yaml",
    "yarn.lock",
];
const NODE_SETTINGS: &[&str] = &[".npmrc", ".yarnrc.yml"];
const NODE_MANIFEST: &[&str] = &["package.json"];
const PYTHON_LOCKS: &[&str] = &["uv.lock", "poetry.lock"];
const PNPM_WORKSPACE: &[&str] = &["pnpm-workspace.yaml"];
const BUN_LOCK: &[&str] = &["bun.lock"];
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
    (&["pip.conf", "pip.ini"], CredentialScan::Pip),
];
/// MSBuild project and solution suffixes NuGet reads.
const NUGET_SUFFIXES: &[&str] = &[".csproj", ".fsproj", ".vbproj", ".sln", ".slnx"];
/// `NUGET_SUFFIXES` plus the property and target files that can carry
/// restore sources.
const NUGET_STEERING_SUFFIXES: &[&str] = &[
    ".csproj", ".fsproj", ".vbproj", ".sln", ".slnx", ".props", ".targets",
];
/// The NuGet files the Count rewrites: its lockfile and the MSBuild files
/// that hold central package management and restore settings.
const NUGET_WRITABLE: &[&str] = &[
    "packages.lock.json",
    "Directory.Packages.props",
    "Directory.Build.props",
    "dotnet-tools.json",
    "global.json",
];
/// composer.json and composer.lock: the files Composer's tool rewrites.
const COMPOSER_FILES: &[&str] = &["composer.json", "composer.lock"];

pub fn eco_roles(manager: Manager, path: &str) -> EcoRoles {
    let mut roles = EcoRoles::default();
    match manager {
        Manager::Npm => {
            roles.registry = named(NODE_SETTINGS, path);
            roles.settings = roles.registry;
            roles.credential_scan = scanned(path, NPM_CREDENTIAL_SCANS);
            roles.check = if named(NODE_MANIFEST, path)
                || named(NPM_LOCKS, path)
                || named(BUN_LOCK, path)
                || named(NODE_SETTINGS, path)
                || named(PNPM_WORKSPACE, path)
            {
                CredentialCheck::Scan
            } else {
                CredentialCheck::Undecided
            };
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
            roles.check = if named(BUN_LOCK, path) || named(NODE_SETTINGS, path) {
                CredentialCheck::Scan
            } else {
                CredentialCheck::Undecided
            };
            roles.writable = named(BUN_LOCK, path);
            roles.read = named(NODE_SETTINGS, path);
            roles.steers = named(&["package.json", ".npmrc", ".yarnrc.yml", "bun.lock"], path);
        }
        Manager::Pyproject => {
            roles.registry = crate::python_settings(path) || named(PYTHON_LOCKS, path);
            roles.settings = crate::python_settings(path) || named(&["pip.conf", "pip.ini"], path);
            roles.credential_scan = scanned(path, PYTHON_CREDENTIAL_SCANS);
            roles.check = if named(
                &[
                    "pyproject.toml",
                    "uv.toml",
                    "poetry.toml",
                    "uv.lock",
                    "poetry.lock",
                ],
                path,
            ) {
                CredentialCheck::Scan
            } else if named(&[".python-version"], path) {
                CredentialCheck::Skip
            } else {
                CredentialCheck::Undecided
            };
            roles.writable = named(&["pyproject.toml"], path) || named(PYTHON_LOCKS, path);
            roles.read = named(&[".python-version", "uv.toml", "poetry.toml"], path);
            roles.steers = crate::python_settings(path)
                || named(&["poetry.toml"], path)
                || named(PYTHON_LOCKS, path);
        }
        Manager::Bundler => {
            roles.registry =
                crate::bundle_config(path) || named(lock_names(Manager::Bundler).supported, path);
            roles.settings = crate::bundle_config(path);
            roles.credential_scan =
                named(manifest_names(Manager::Bundler), path).then_some(CredentialScan::Gemfile);
            roles.check = if named(&["Gemfile", "Gemfile.lock"], path) || path.ends_with(".gemspec")
            {
                CredentialCheck::Scan
            } else {
                CredentialCheck::Undecided
            };
            roles.writable = named(&["Gemfile", "Gemfile.lock"], path);
            roles.read = path.ends_with(".gemspec");
            roles.steers = named(&["Gemfile", "Gemfile.lock"], path) || path.ends_with(".gemspec");
        }
        Manager::Composer => {
            roles.registry = named(lock_names(Manager::Composer).supported, path);
            roles.credential_scan =
                named(manifest_names(Manager::Composer), path).then_some(CredentialScan::Composer);
            roles.check = if named(COMPOSER_FILES, path) {
                CredentialCheck::Scan
            } else {
                CredentialCheck::Undecided
            };
            roles.writable = named(COMPOSER_FILES, path);
            roles.steers = named(COMPOSER_FILES, path);
        }
        Manager::Cargo => {
            roles.registry = crate::cargo::config_file(path);
            roles.settings = crate::cargo::config_file(path) || named(&["Cargo.toml"], path);
            roles.credential_scan =
                crate::cargo::config_file(path).then_some(CredentialScan::Cargo);
            roles.check = if named(&["Cargo.toml", "Cargo.lock"], path)
                || crate::cargo::config_file(path)
                || path.ends_with(".rs")
            {
                CredentialCheck::Scan
            } else {
                CredentialCheck::Undecided
            };
            roles.writable = named(&["Cargo.toml", "Cargo.lock"], path);
            roles.read = path.ends_with(".rs");
            roles.steers = named(&["Cargo.toml", "Cargo.lock"], path);
        }
        Manager::Go => {
            roles.registry = crate::go::work_file(path);
            roles.settings = roles.registry;
            roles.check = if named(&["go.mod", "go.work"], path) || path.ends_with(".go") {
                CredentialCheck::Scan
            } else if named(&["go.sum", "go.work.sum"], path) {
                CredentialCheck::Skip
            } else {
                CredentialCheck::Undecided
            };
            roles.writable = named(&["go.mod", "go.sum"], path);
            roles.read = named(&["go.work", "go.work.sum"], path) || path.ends_with(".go");
        }
        Manager::Terraform => {
            roles.check = if path.ends_with(".tf") {
                CredentialCheck::Scan
            } else if named(&[".terraform.lock.hcl"], path) {
                CredentialCheck::Skip
            } else {
                CredentialCheck::Undecided
            };
            roles.writable = path.ends_with(".tf") || named(&[".terraform.lock.hcl"], path);
        }
        Manager::Nuget => {
            roles.registry = crate::nuget::config_file(path) || crate::nuget::msbuild_file(path);
            roles.settings = roles.registry;
            roles.credential_scan =
                crate::nuget::config_file(path).then_some(CredentialScan::Nuget);
            roles.check = if crate::nuget::config_file(path)
                || named(manifest_names(Manager::Nuget), path)
                || suffixed(NUGET_SUFFIXES, path)
            {
                CredentialCheck::Scan
            } else if named(lock_names(Manager::Nuget).supported, path) {
                CredentialCheck::Skip
            } else {
                CredentialCheck::Undecided
            };
            roles.writable = named(NUGET_WRITABLE, path);
            roles.read = crate::nuget::config_file(path)
                || named(&["Directory.Build.targets"], path)
                || suffixed(NUGET_SUFFIXES, path);
            roles.steers = crate::nuget::config_file(path)
                || named(lock_names(Manager::Nuget).supported, path)
                || named(manifest_names(Manager::Nuget), path)
                || suffixed(NUGET_STEERING_SUFFIXES, path);
        }
        Manager::Maven => {
            roles.registry = crate::maven::repository_file(path);
            roles.settings = roles.registry;
        }
        Manager::Gradle => {
            roles.registry = crate::gradle::build_logic(path);
            roles.settings = roles.registry;
            roles.check = if named(&["gradle-wrapper.properties"], path) {
                CredentialCheck::Scan
            } else {
                CredentialCheck::Undecided
            };
            roles.writable = named(&["gradle-wrapper.properties"], path);
        }
        Manager::Mise => {
            roles.registry = crate::mise::config_file(path)
                || [
                    Manager::Npm,
                    Manager::Pyproject,
                    Manager::Cargo,
                    Manager::Nuget,
                ]
                .iter()
                .any(|m| crate::ecosystem(*m).backend_settings(path));
            roles.settings = crate::mise::config_file(path);
            roles.check = if named(&["mise.toml", ".mise.toml", "mise.lock"], path) {
                CredentialCheck::Scan
            } else if named(&[".tool-versions"], path) {
                CredentialCheck::Skip
            } else {
                CredentialCheck::Undecided
            };
            roles.writable = named(&["mise.lock"], path);
            roles.read = named(&["mise.toml", ".mise.toml", ".tool-versions"], path);
        }
        Manager::Nix => {
            roles.check = if named(&["flake.nix", "flake.lock"], path) {
                CredentialCheck::Scan
            } else {
                CredentialCheck::Undecided
            };
            roles.writable = named(&["flake.lock"], path);
            roles.read = named(&["flake.nix"], path);
        }
        Manager::Apm => {
            roles.check = if named(&["apm.yml", "apm.lock.yaml"], path) {
                CredentialCheck::Scan
            } else {
                CredentialCheck::Undecided
            };
            roles.writable = named(&["apm.lock.yaml"], path);
            roles.read = named(&["apm.yml"], path);
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
        let rank = |check: CredentialCheck| match check {
            CredentialCheck::Scan => 2,
            CredentialCheck::Skip => 1,
            CredentialCheck::Undecided => 0,
        };
        if rank(eco.check) > rank(roles.check) {
            roles.check = eco.check;
        }
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

pub fn credential_check(path: &str) -> CredentialCheck {
    roles(path).check
}

pub fn credential_scope(path: &str) -> bool {
    roles(path).credentials
}

/// A Cargo configuration file, read only for its `registries`.
pub fn cargo_config(path: &str) -> bool {
    crate::cargo::config_file(path)
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
            supported: &["bun.lock"],
            unsupported: &[],
            readable: &["bun.lock"],
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
            ".miserc",
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
