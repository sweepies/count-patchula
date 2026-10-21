use crate::{Error, Result};
use rustovate_report::{
    Current, Dependency, PackageFile, Report, Repository, SCHEMA_VERSION, Status, Update,
};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Warning {
    pub line: usize,
    pub repository: Option<String>,
    pub field: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Exclusion {
    pub repository: String,
    pub manager: String,
    pub package_files: usize,
    pub dependencies: usize,
    pub updates: usize,
    pub rule: String,
    pub departure: u32,
    pub link: String,
}

#[derive(Debug)]
pub struct ParsedLogs {
    pub report: Report,
    pub warnings: Vec<Warning>,
    pub exclusions: Vec<Exclusion>,
}

struct Context<'a> {
    evaluated_at: Option<&'a str>,
    line: usize,
    repository: Option<&'a str>,
    field: String,
    warnings: &'a mut Vec<Warning>,
}

impl Context<'_> {
    fn warn(&mut self, field: &str, message: &str) {
        self.warnings.push(Warning {
            line: self.line,
            repository: self.repository.map(str::to_owned),
            field: format!("{}.{field}", self.field),
            message: message.into(),
        });
    }

    fn text(&mut self, value: &Value, field: &str) -> Option<String> {
        match value.get(field) {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            _ => {
                self.warn(
                    field,
                    "expected a nonempty string or null; treated as absent",
                );
                None
            }
        }
    }

    fn unknown_fields(&mut self, value: &Value, known: &[&str]) {
        if let Some(object) = value.as_object() {
            for key in object.keys().filter(|k| !known.contains(&k.as_str())) {
                self.warn(key, "unrecognized output field; ignored");
            }
        }
    }
}

fn required(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            Error::Input(format!(
                "Renovate comparison record needs a nonempty {field}"
            ))
        })
}

fn manager(name: &str) -> String {
    match name {
        "gomod" => "go",
        "pip_requirements" => "requirements",
        "pep621" | "poetry" | "pyenv" => "pyproject",
        "docker-compose" => "compose",
        "custom.regex" => "regex",
        other => other,
    }
    .into()
}

// Observed output metadata has no equivalent in the v1 comparison report.
// Recognize keys, never interpret templates or config/preset contents.
const DEP_METADATA: &[&str] = &[
    "depName",
    "packageName",
    "currentValue",
    "currentVersion",
    "currentDigest",
    "updates",
    "groupName",
    "datasource",
    "skipReason",
    "warnings",
    "depType",
    "versioning",
    "managerData",
    "currentVersionAgeInDays",
    "currentVersionTimestamp",
    "dependencyUrl",
    "fixedVersion",
    "homepage",
    "isAbandoned",
    "isSingleVersion",
    "lockedVersion",
    "mostRecentTimestamp",
    "registryUrl",
    "sourceDirectory",
    "sourceUrl",
    "target",
    "autoReplaceStringTemplate",
    "commitMessageTopic",
    "extractVersion",
    "replaceString",
    "skipStage",
    "prettyDepType",
    "deprecationMessage",
    "registryUrls",
    "lookupName",
];
const UPDATE_METADATA: &[&str] = &[
    "newValue",
    "newVersion",
    "newDigest",
    "updateType",
    "groupName",
    "branchName",
    "bucket",
    "isBreaking",
    "isLockfileUpdate",
    "isRange",
    "libYears",
    "newMajor",
    "newMinor",
    "newPatch",
    "newVersionAgeInDays",
    "pendingVersions",
    "releaseTimestamp",
    "downloadUrl",
    "hasAttestation",
    "isPin",
    "pendingChecks",
    "manager",
    "packageFile",
    "depName",
    "packageName",
    "currentValue",
    "currentVersion",
    "currentDigest",
    "datasource",
    "isPinDigest",
];
const FILE_METADATA: &[&str] = &[
    "packageFile",
    "deps",
    "lockFiles",
    "packageFileVersion",
    "extractedConstraints",
    "managerData",
    "skipInstalls",
    "extractVersionTemplate",
    "matchStrings",
];

fn update(value: &Value, inherited_group: Option<String>, ctx: &mut Context<'_>) -> Option<Update> {
    if !value.is_object() {
        ctx.warn("update", "expected an object; proposal ignored");
        return None;
    }
    ctx.unknown_fields(value, UPDATE_METADATA);
    let kind = ctx.text(value, "updateType").unwrap_or_else(|| {
        ctx.warn("updateType", "missing update type; retained as unknown");
        "unknown".into()
    });
    if !matches!(
        kind.as_str(),
        "major" | "minor" | "patch" | "digest" | "pin" | "pinDigest"
    ) {
        ctx.warn("updateType", "unrecognized update type; retained literally");
    }
    let proposed = Update {
        new_value: ctx.text(value, "newValue"),
        new_version: ctx.text(value, "newVersion"),
        new_digest: ctx.text(value, "newDigest"),
        update_type: if kind == "pinDigest" {
            "pin".into()
        } else {
            kind
        },
        group: ctx.text(value, "groupName").or(inherited_group),
        policy: None,
        release_age: None,
    };
    if proposed.new_value.is_none()
        && proposed.new_version.is_none()
        && proposed.new_digest.is_none()
    {
        ctx.warn(
            "target",
            "missing value/version/digest target; proposal ignored",
        );
        return None;
    }
    Some(proposed)
}

fn dependency(value: &Value, source_manager: &str, ctx: &mut Context<'_>) -> Result<Dependency> {
    ctx.unknown_fields(value, DEP_METADATA);
    let package = ctx.text(value, "packageName");
    let name = match ctx.text(value, "depName") {
        Some(name) => name,
        None => {
            ctx.warn("depName", "missing dependency name; using packageName");
            package
                .clone()
                .ok_or_else(|| Error::Input("dependency needs depName or packageName".into()))?
        }
    };
    let mut package = package.unwrap_or_else(|| {
        ctx.warn("packageName", "missing registry identity; using depName");
        name.clone()
    });
    // Datasource is metadata in report v1, but malformed values still diagnose.
    let datasource = ctx.text(value, "datasource");
    if datasource.as_deref() == Some("helm")
        && let Some(url) = value
            .get("registryUrls")
            .and_then(Value::as_array)
            .filter(|urls| urls.len() == 1)
            .and_then(|urls| urls[0].as_str())
    {
        package = format!("{url}|{package}");
    }
    let group = ctx.text(value, "groupName");
    let current = Current {
        value: ctx.text(value, "currentValue"),
        version: ctx.text(value, "currentVersion"),
        digest: ctx.text(value, "currentDigest"),
    };
    if current.version.is_none() {
        ctx.warn(
            "currentVersion",
            "lookup did not supply a resolved current version",
        );
    }
    // A digest-only dependency and an unspecified version are both real output.
    if current.value.is_none() && current.version.is_none() && current.digest.is_none() {
        ctx.warn("current", "no current value/version/digest available");
    }
    if ctx.text(value, "skipReason").is_some() {
        ctx.warn(
            "skipReason",
            "Renovate skipped lookup; dependency retained with available fields",
        );
    }
    if let Some(warnings) = value.get("warnings") {
        match warnings {
            Value::Array(items) if items.is_empty() => (),
            Value::Null => (),
            _ => ctx.warn(
                "warnings",
                "Renovate supplied dependency warnings; payload omitted",
            ),
        }
    }
    let mut updates = Vec::new();
    match value.get("updates") {
        Some(Value::Array(items)) => {
            let parent = ctx.field.clone();
            for (i, item) in items.iter().enumerate() {
                ctx.field = format!("{parent}.updates[{i}]");
                if let Some(proposed) = update(item, group.clone(), ctx) {
                    updates.push(proposed);
                }
            }
            ctx.field = parent;
        }
        _ => ctx.warn(
            "updates",
            "missing or non-array optional updates; treated as empty",
        ),
    }
    Ok(Dependency {
        manager: manager(source_manager),
        package,
        name,
        occurrence: None,
        current,
        updates,
        metadata: datasource.map(|source| rustovate_report::DependencyMetadata {
            tag_digests: Default::default(),
            stale: Default::default(),
            lookup: ctx
                .evaluated_at
                .map(|time| rustovate_report::LookupEvidence {
                    catalog: None,
                    evaluated_at: time.into(),
                    disabled_updates: Vec::new(),
                    rejected: Vec::new(),
                }),
            resolution: None,
            source,
            kinds: value
                .get("depType")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .into_iter()
                .collect(),
            locations: Vec::new(),
            held_back: Vec::new(),
        }),
    })
}

/// Compatibility helper for callers needing only the normalized report. Use
/// parse_logs_with_diagnostics when emitting triage so warnings aren't discarded.
pub fn parse_logs(logs: &str) -> Result<Report> {
    Ok(parse_logs_with_diagnostics(logs)?.report)
}

/// Read output only. Required snapshot structure/identity failures carry a line
/// number; diagnostics never include raw records or ignored field values.
pub fn parse_logs_with_diagnostics(logs: &str) -> Result<ParsedLogs> {
    let mut repositories = BTreeMap::<String, BTreeMap<String, Vec<Dependency>>>::new();
    let mut summaries = Vec::<(Option<String>, Value, Option<String>, usize)>::new();
    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let mut exclusions = Vec::new();
    let mut unavailable = BTreeMap::new();
    for (index, line) in logs.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let record: Value = serde_json::from_str(line)
            .map_err(|_| Error::Input(format!("invalid Renovate JSON on line {}", index + 1)))?;
        if record
            .get("level")
            .and_then(Value::as_u64)
            .is_some_and(|level| level >= 50)
        {
            failures.push(index + 1);
        }
        let repo = record.get("repository").and_then(Value::as_str);
        if record["msg"] == "Repository finished"
            && record["result"] == "config-validation"
            && let Some(repo) = repo
        {
            unavailable.insert(
                repo.to_owned(),
                "Renovate configuration validation failed; no lookup baseline".to_owned(),
            );
            repositories.remove(repo);
        }
        let mut ctx = Context {
            line: index + 1,
            evaluated_at: record.get("time").and_then(Value::as_str),
            repository: repo,
            field: "record".into(),
            warnings: &mut warnings,
        };
        if record.get("level").and_then(Value::as_u64) == Some(40) {
            ctx.warn("level", "Renovate logged a warning; payload omitted");
        }
        let result = (|| {
            if record["msg"] == "packageFiles with updates" {
                let repo = required(&record, "repository")?;
                unavailable.remove(&repo);
                let (field, files) = if let Some(files) =
                    record.get("config").and_then(Value::as_object)
                {
                    ("config", files)
                } else if let Some(files) = record.get("packageFiles").and_then(Value::as_object) {
                    ctx.warn(
                        "packageFiles",
                        "legacy snapshot shape; expected manager map under config",
                    );
                    ("packageFiles", files)
                } else {
                    return Err(Error::Input(
                        "config (or legacy packageFiles) must be a manager-keyed object".into(),
                    ));
                };
                let mut snapshot = BTreeMap::<String, Vec<Dependency>>::new();
                exclusions.retain(|e: &Exclusion| e.repository != repo);
                for (source_manager, files) in files {
                    ctx.field = field.into();
                    let files = files.as_array().ok_or_else(|| {
                        Error::Input("manager package files must be an array".into())
                    })?;
                    if source_manager == "renovate-config" {
                        exclusions.push(Exclusion {
                            repository: repo.clone(),
                            manager: source_manager.clone(),
                            package_files: files.len(),
                            dependencies: files
                                .iter()
                                .filter_map(|f| f["deps"].as_array())
                                .map(Vec::len)
                                .sum(),
                            updates: files
                                .iter()
                                .filter_map(|f| f["deps"].as_array())
                                .flatten()
                                .filter_map(|d| d["updates"].as_array())
                                .map(Vec::len)
                                .sum(),
                            rule: "renovate-config-manager".into(),
                            departure: 19,
                            link: "docs/departures.md#departure-19".into(),
                        });
                        continue;
                    }
                    if !matches!(
                        manager(source_manager).as_str(),
                        "cargo"
                            | "bun"
                            | "terraform"
                            | "kubernetes"
                            | "kustomize"
                            | "flux"
                            | "helm-values"
                            | "github-actions"
                            | "mise"
                            | "npm"
                            | "go"
                            | "requirements"
                            | "pyproject"
                            | "pre-commit"
                            | "compose"
                            | "regex"
                            | "dockerfile"
                    ) {
                        ctx.warn(source_manager, "unrecognized manager; retained literally");
                    }
                    for (i, file) in files.iter().enumerate() {
                        ctx.field = format!("{field}.{source_manager}[{i}]");
                        ctx.unknown_fields(file, FILE_METADATA);
                        let path = required(file, "packageFile")?;
                        let deps = file.get("deps").and_then(Value::as_array).ok_or_else(|| {
                            Error::Input("package file deps must be an array".into())
                        })?;
                        let target = snapshot.entry(path).or_default();
                        for (j, dep) in deps.iter().enumerate() {
                            ctx.field = format!("{field}.{source_manager}[{i}].deps[{j}]");
                            target.push(dependency(dep, source_manager, &mut ctx)?);
                        }
                    }
                }
                summaries.retain(|(name, _, _, _)| name.as_deref() != Some(repo.as_str()));
                repositories.insert(repo, snapshot);
            }
            // Only array branches/object branch with an upgrades array are
            // summaries. Diagnostic branches maps and branch names are ordinary logs.
            let mut branches = Vec::new();
            if let Some(items) = record.get("branches").and_then(Value::as_array) {
                branches.extend(items.iter());
            }
            if let Some(branch) = record.get("branch").filter(|b| b.is_object()) {
                branches.push(branch);
            }
            ctx.field = "branch".into();
            for branch in branches {
                if let Some(items) = branch.get("upgrades") {
                    match items.as_array() {
                        Some(upgrades) => {
                            for upgrade in upgrades {
                                summaries.push((
                                    repo.map(str::to_owned),
                                    upgrade.clone(),
                                    ctx.text(branch, "groupName"),
                                    index + 1,
                                ));
                            }
                        }
                        None => ctx.warn("upgrades", "non-array optional branch upgrades; ignored"),
                    }
                }
            }
            Ok(())
        })();
        result.map_err(|error: Error| {
            Error::Input(format!("Renovate line {}: {error}", index + 1))
        })?;
    }
    if !failures.is_empty() {
        return Err(Error::Input(format!(
            "Renovate logged errors on lines {failures:?}; use a successful lookup log"
        )));
    }
    if repositories.is_empty() && unavailable.is_empty() {
        return Err(Error::Input("no 'packageFiles with updates' records; use LOG_LEVEL=debug LOG_FORMAT=json and a successful lookup/full dry run".into()));
    }
    for (repo, value, group, line) in summaries {
        let mut ctx = Context {
            line,
            evaluated_at: None,
            repository: repo.as_deref(),
            field: "branch.upgrades".into(),
            warnings: &mut warnings,
        };
        let Some(files) = repo.as_ref().and_then(|repo| repositories.get_mut(repo)) else {
            ctx.warn(
                "repository",
                "branch summary has no matching repository snapshot; ignored",
            );
            continue;
        };
        if value["manager"] == "renovate-config" {
            continue;
        }
        let Some(name) = ctx
            .text(&value, "depName")
            .or_else(|| ctx.text(&value, "packageName"))
        else {
            ctx.warn(
                "depName",
                "branch summary has no dependency identity; ignored",
            );
            continue;
        };
        let path = ctx.text(&value, "packageFile");
        let source_manager = ctx.text(&value, "manager").map(|m| manager(&m));
        let current = ctx.text(&value, "currentValue");
        let mut candidates = files
            .iter_mut()
            .filter(|(file, _)| path.as_ref().is_none_or(|path| path == *file))
            .flat_map(|(_, deps)| deps.iter_mut())
            .filter(|dep| {
                dep.name == name
                    && source_manager.as_ref().is_none_or(|m| m == &dep.manager)
                    && current
                        .as_ref()
                        .is_none_or(|v| dep.current.value.as_ref() == Some(v))
            });
        let Some(dep) = candidates.next() else {
            ctx.warn(
                "identity",
                "branch summary does not match an extracted dependency; ignored",
            );
            continue;
        };
        // Duplicate occurrences with the same identity will be merged later.
        // Avoid guessing enrichment when a summary has multiple candidates.
        if candidates.next().is_some() {
            ctx.warn("identity", "ambiguous branch summary; enrichment ignored");
            continue;
        }
        let Some(proposed) = update(&value, group, &mut ctx) else {
            continue;
        };
        let matching: Vec<_> =
            dep.updates
                .iter_mut()
                .filter(|u| {
                    u.update_type == proposed.update_type
                        && proposed.new_value.as_ref().is_none_or(|v| {
                            u.new_value.as_ref().is_none_or(|existing| existing == v)
                        })
                        && proposed.new_version.as_ref().is_none_or(|v| {
                            u.new_version.as_ref().is_none_or(|existing| existing == v)
                        })
                        && proposed.new_digest.as_ref().is_none_or(|v| {
                            u.new_digest.as_ref().is_none_or(|existing| existing == v)
                        })
                })
                .collect();
        if matching.len() > 1 {
            ctx.warn(
                "target",
                "ambiguous branch update target; enrichment ignored",
            );
            continue;
        }
        if let Some(existing) = matching.into_iter().next() {
            existing.new_value = proposed.new_value.or(existing.new_value.take());
            existing.new_version = proposed.new_version.or(existing.new_version.take());
            existing.new_digest = proposed.new_digest.or(existing.new_digest.take());
            if proposed.group.is_some() {
                existing.group = proposed.group;
            }
        } else {
            dep.updates.push(proposed);
        }
    }
    let mut report = Report {
        actions: None,
        schema_version: SCHEMA_VERSION,
        repositories: Vec::new(),
        stats: None,
    };
    for (repo, files) in repositories {
        let mut package_files = Vec::new();
        for (path, dependencies) in files {
            let dependencies = rustovate_report::normalize_dependencies(dependencies);
            package_files.push(PackageFile { path, dependencies });
        }
        report.repositories.push(Repository {
            repository: repo,
            status: Status::Complete,
            reason: None,
            package_files,
            warnings: Vec::new(),
            default_branch: None,
            commit: None,
        });
    }
    for (repository, reason) in unavailable {
        report.repositories.push(Repository {
            repository,
            status: Status::Skipped,
            reason: Some(reason),
            package_files: Vec::new(),
            warnings: Vec::new(),
            default_branch: None,
            commit: None,
        });
    }
    report
        .repositories
        .sort_by(|a, b| a.repository.cmp(&b.repository));
    report.validate()?;
    Ok(ParsedLogs {
        report,
        warnings,
        exclusions,
    })
}
