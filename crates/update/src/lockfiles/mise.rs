//! Targeted mise.lock updates using a literal, tools-only configuration.
use super::*;
use serde_json::{Value, json};

fn value(value: &toml_edit::Value) -> Value {
    if let Some(v) = value.as_str() {
        json!(v)
    } else if let Some(v) = value.as_integer() {
        json!(v)
    } else if let Some(v) = value.as_float() {
        json!(v)
    } else if let Some(v) = value.as_bool() {
        json!(v)
    } else if let Some(v) = value.as_array() {
        Value::Array(v.iter().map(self::value).collect())
    } else if let Some(v) = value.as_inline_table() {
        Value::Object(v.iter().map(|(k, v)| (k.into(), self::value(v))).collect())
    } else {
        json!(value.to_string())
    }
}
fn item(item: &toml_edit::Item) -> Value {
    if let Some(v) = item.as_value() {
        value(v)
    } else if let Some(v) = item.as_array_of_tables() {
        Value::Array(
            v.iter()
                .map(|table| {
                    Value::Object(
                        table
                            .iter()
                            .map(|(k, v)| (k.into(), self::item(v)))
                            .collect(),
                    )
                })
                .collect(),
        )
    } else if let Some(v) = item.as_table_like() {
        Value::Object(v.iter().map(|(k, v)| (k.into(), self::item(v))).collect())
    } else {
        Value::Null
    }
}
fn parsed(text: &str) -> Result<Value, String> {
    let doc = toml_edit::Document::parse(text).map_err(|e| format!("invalid mise.lock: {e}"))?;
    Ok(Value::Object(
        doc.iter().map(|(k, v)| (k.into(), item(v))).collect(),
    ))
}
fn platforms(entries: &Value) -> BTreeSet<String> {
    entries
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|entry| {
            entry
                .as_object()
                .into_iter()
                .flatten()
                .flat_map(|(key, value)| {
                    if let Some(platform) = key.strip_prefix("platforms.") {
                        vec![platform.into()]
                    } else if key == "platforms" {
                        value
                            .as_object()
                            .into_iter()
                            .flatten()
                            .map(|(name, _)| name.clone())
                            .collect()
                    } else {
                        Vec::new()
                    }
                })
        })
        .collect()
}

impl Runner {
    pub(super) async fn mise_lock(
        &self,
        driver: &Path,
        root: &Path,
        baseline: &BTreeMap<String, Blob>,
        jobs: &[&Job],
        maintenance: bool,
        config: &Config,
    ) -> Result<(), String> {
        let directory = &jobs.first().ok_or("missing mise lock job")?.directory;
        let lock_path = if directory.is_empty() {
            "mise.lock".into()
        } else {
            format!("{directory}/mise.lock")
        };
        let original = &baseline
            .get(&lock_path)
            .ok_or("missing committed mise.lock")?
            .content;
        let before = parsed(original)?;
        let old_tools = before["tools"]
            .as_object()
            .ok_or("mise.lock has no tool entries")?;
        let selected: BTreeSet<_> = if maintenance {
            old_tools.keys().cloned().collect()
        } else {
            jobs.iter().filter_map(|job| job.package.clone()).collect()
        };
        let work = driver.join(format!("mise-lock-{}", hash(directory.as_bytes())));
        std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        let mut tools = toml_edit::Table::new();
        let mut paths: Vec<_> = baseline
            .keys()
            .filter(|path| {
                matches!(
                    path.rsplit('/').next(),
                    Some("mise.toml" | ".mise.toml" | ".tool-versions")
                ) && (under(path, directory) || under(directory, super::directory(path)))
            })
            .collect();
        paths.sort_by_key(|path| path.len());
        for path in paths {
            let text = std::fs::read_to_string(root.join(path)).map_err(|e| e.to_string())?;
            if path.ends_with(".tool-versions") {
                for line in text.lines() {
                    let mut words = line.split('#').next().unwrap_or("").split_whitespace();
                    if let Some(name) = words.next()
                        && selected.contains(name)
                    {
                        let versions: Vec<_> = words.collect();
                        if versions.len() == 1 {
                            tools[name] = toml_edit::value(versions[0]);
                        } else {
                            tools[name] = toml_edit::value(
                                versions.into_iter().collect::<toml_edit::Array>(),
                            );
                        }
                    }
                }
            } else {
                let doc = toml_edit::Document::parse(text)
                    .map_err(|e| e.to_string())?
                    .into_mut();
                if let Some(declarations) =
                    doc.get("tools").and_then(toml_edit::Item::as_table_like)
                {
                    for (name, spec) in declarations
                        .iter()
                        .filter(|(name, _)| selected.contains(*name))
                    {
                        tools.insert(name, spec.clone());
                    }
                }
            }
        }
        if selected.is_empty() || selected.iter().any(|name| !tools.contains_key(name)) {
            return Err("mise.lock tool has no literal project declaration".into());
        }
        let mut doc = toml_edit::DocumentMut::new();
        doc["tools"] = toml_edit::Item::Table(tools);
        let sanitized = doc.to_string();
        if sanitized.contains("{{") {
            return Err("mise lock tool configuration contains a template".into());
        }
        std::fs::write(work.join("mise.toml"), &sanitized).map_err(|e| e.to_string())?;
        std::fs::write(work.join("mise.lock"), original).map_err(|e| e.to_string())?;
        let existing_platforms: BTreeSet<_> = selected
            .iter()
            .flat_map(|name| platforms(&before["tools"][name]))
            .collect();
        if existing_platforms.is_empty() {
            return Err("mise.lock has no existing platforms to preserve".into());
        }
        let defaults =
            toml_edit::Document::parse(include_str!("defaults.toml")).map_err(|e| e.to_string())?;
        let version = defaults["tools"]["mise"]
            .as_str()
            .ok_or("missing pinned mise")?;
        let mut command = self.command(driver);
        command.args.extend([
            "exec".into(),
            format!("{}@{version}", patchula_mise::PACKAGE),
            "--".into(),
            "env".into(),
            "-u".into(),
            "MISE_NO_CONFIG".into(),
            "mise".into(),
            "--cd".into(),
            work.to_string_lossy().into_owned(),
            "lock".into(),
            "--platform".into(),
            existing_platforms.into_iter().collect::<Vec<_>>().join(","),
        ]);
        if maintenance {
            command.args.extend([
                "--bump".into(),
                "--minimum-release-age".into(),
                format!("{}h", config.defaults.wait_hours),
            ]);
        }
        command.args.extend(selected.iter().cloned());
        self.executor
            .run(command)
            .await
            .map_err(|e| format!("mise lock: {e}"))?;
        if std::fs::read_to_string(work.join("mise.toml")).map_err(|e| e.to_string())? != sanitized
        {
            return Err("mise lock modified the requested tool declarations".into());
        }
        let generated =
            std::fs::read_to_string(work.join("mise.lock")).map_err(|e| e.to_string())?;
        let after = parsed(&generated)?;
        let mut untouched_before = before.clone();
        let mut untouched_after = after.clone();
        for name in &selected {
            if let Some(tools) = untouched_before["tools"].as_object_mut() {
                tools.remove(name);
            }
            if let Some(tools) = untouched_after["tools"].as_object_mut() {
                tools.remove(name);
            }
            let entries = after["tools"][name]
                .as_array()
                .filter(|entries| !entries.is_empty())
                .ok_or("mise lock removed a selected tool")?;
            if !platforms(&before["tools"][name]).is_subset(&platforms(&after["tools"][name])) {
                return Err(format!("mise lock dropped an existing platform for {name}"));
            }
            for job in jobs
                .iter()
                .filter(|job| job.package.as_deref() == Some(name))
            {
                if let Some(target) = &job.target
                    && !entries.iter().any(|entry| {
                        entry["version"].as_str().is_some_and(|v| {
                            v.trim_start_matches('v') == target.trim_start_matches('v')
                        })
                    })
                {
                    return Err(format!("mise lock did not resolve {name} to {target}"));
                }
            }
        }
        if untouched_before != untouched_after {
            return Err("mise lock changed unrelated entries or lockfile metadata".into());
        }
        if collect(&work)?
            .keys()
            .any(|path| !matches!(path.as_str(), "mise.toml" | "mise.lock"))
        {
            return Err("mise lock produced unsupported sidecar files".into());
        }
        write(root, &lock_path, &generated)
    }
}
