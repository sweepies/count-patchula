use crate::{Error, Result};
use rustovate_report::{
    Current, Dependency, PackageFile, Report, Repository, SCHEMA_VERSION, Status, Update,
};
use serde_json::Value;
use std::collections::BTreeMap;

fn text(value: &Value, field: &str) -> Result<Option<String>> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        _ => Err(Error::Input(format!(
            "Renovate field {field} must be a string or null"
        ))),
    }
}

fn required(value: &Value, field: &str) -> Result<String> {
    text(value, field)?
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::Input(format!("Renovate comparison record is missing {field}")))
}

fn manager(name: &str) -> String {
    match name {
        "gomod" => "go",
        "pip_requirements" => "requirements",
        "docker-compose" => "compose",
        "custom.regex" => "regex",
        other => other,
    }
    .into()
}

fn update(value: &Value, inherited_group: Option<String>) -> Result<Update> {
    let kind = required(value, "updateType")?;
    Ok(Update {
        new_value: text(value, "newValue")?,
        new_version: text(value, "newVersion")?,
        new_digest: text(value, "newDigest")?,
        update_type: if kind == "pinDigest" {
            "pin".into()
        } else {
            kind
        },
        group: text(value, "groupName")?.or(inherited_group),
    })
}

fn dependency(value: &Value, source_manager: &str) -> Result<Dependency> {
    let name = required(value, "depName")?;
    let group = text(value, "groupName")?;
    let updates = match value.get("updates") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(updates)) => updates
            .iter()
            .map(|u| update(u, group.clone()))
            .collect::<Result<_>>()?,
        _ => return Err(Error::Input("Renovate updates must be an array".into())),
    };
    Ok(Dependency {
        manager: manager(source_manager),
        package: text(value, "packageName")?.unwrap_or_else(|| name.clone()),
        name,
        current: Current {
            value: text(value, "currentValue")?,
            version: text(value, "currentVersion")?,
            digest: text(value, "currentDigest")?,
        },
        updates,
    })
}

/// Non-JSON runtime banners are ignored. Broken JSON-looking lines and comparison
/// records fail with a line number, without echoing potentially sensitive log text.
pub fn parse_logs(logs: &str) -> Result<Report> {
    let mut repositories = BTreeMap::<String, BTreeMap<String, Vec<Dependency>>>::new();
    let mut summaries = Vec::new();
    let mut failures = Vec::new();
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
        let result = (|| {
            if record["msg"] == "packageFiles with updates" {
                let repo = required(&record, "repository")?;
                let files = record
                    .get("packageFiles")
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        Error::Input("packageFiles must be a manager-keyed object".into())
                    })?;
                // Latest snapshot wins, including a valid empty extraction.
                let mut snapshot = BTreeMap::<String, Vec<Dependency>>::new();
                for (manager, files) in files {
                    let files = files.as_array().ok_or_else(|| {
                        Error::Input("manager packageFiles must be an array".into())
                    })?;
                    for file in files {
                        let path = required(file, "packageFile")?;
                        let deps = file.get("deps").and_then(Value::as_array).ok_or_else(|| {
                            Error::Input("package file deps must be an array".into())
                        })?;
                        let target = snapshot.entry(path).or_default();
                        for dep in deps {
                            target.push(dependency(dep, manager)?);
                        }
                    }
                }
                summaries.retain(|(name, _, _)| name != &repo);
                repositories.insert(repo, snapshot);
            }
            // Branch metadata is output too; use it only to enrich extracted deps.
            if let Some(branches) = record.get("branches").and_then(Value::as_array) {
                let repo = required(&record, "repository")?;
                for branch in branches {
                    if let Some(upgrades) = branch.get("upgrades").and_then(Value::as_array) {
                        for upgrade in upgrades {
                            summaries.push((
                                repo.clone(),
                                upgrade.clone(),
                                text(branch, "groupName")?,
                            ));
                        }
                    }
                }
            }
            if let Some(branch) = record.get("branch").filter(|b| b.is_object())
                && let Some(upgrades) = branch.get("upgrades").and_then(Value::as_array)
            {
                let repo = required(&record, "repository")?;
                for upgrade in upgrades {
                    summaries.push((repo.clone(), upgrade.clone(), text(branch, "groupName")?));
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
    if repositories.is_empty() {
        return Err(Error::Input("no 'packageFiles with updates' records; use LOG_LEVEL=debug LOG_FORMAT=json and a successful lookup/full dry run".into()));
    }
    for (repo, value, group) in summaries {
        let Some(files) = repositories.get_mut(&repo) else {
            continue;
        };
        let name = required(&value, "depName")?;
        let path = text(&value, "packageFile")?;
        let source_manager = text(&value, "manager")?.map(|m| manager(&m));
        let current = text(&value, "currentValue")?;
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
            return Err(Error::Input(
                "branch upgrade does not match an extracted dependency".into(),
            ));
        };
        if candidates.next().is_some() {
            return Err(Error::Input("ambiguous branch upgrade; include packageFile/manager/currentValue in the captured output".into()));
        }
        let proposed = update(&value, group)?;
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
            return Err(Error::Input("ambiguous branch update target".into()));
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
        schema_version: SCHEMA_VERSION,
        repositories: Vec::new(),
    };
    for (repo, files) in repositories {
        let mut package_files = Vec::new();
        for (path, dependencies) in files {
            let mut merged = BTreeMap::<(String, String), Dependency>::new();
            for mut dep in dependencies {
                let key = (dep.manager.clone(), dep.name.clone());
                if let Some(existing) = merged.get_mut(&key) {
                    if existing.current != dep.current || existing.package != dep.package {
                        return Err(Error::Input(format!(
                            "ambiguous dependency identity in {repo}/{path}; duplicate manager/name has different package or current fields"
                        )));
                    }
                    existing.updates.append(&mut dep.updates);
                } else {
                    merged.insert(key, dep);
                }
            }
            let dependencies = merged
                .into_values()
                .map(|mut dep| {
                    dep.updates.sort();
                    dep.updates.dedup();
                    dep
                })
                .collect();
            package_files.push(PackageFile { path, dependencies });
        }
        report.repositories.push(Repository {
            repository: repo,
            status: Status::Complete,
            reason: None,
            package_files,
        });
    }
    report.validate()?;
    Ok(report)
}
