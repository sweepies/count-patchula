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

/// The version requests a tool declaration makes, which mise records as the
/// `specifiers` of the lock entries it resolves.
fn requests(spec: &toml_edit::Item) -> BTreeSet<String> {
    fn value(spec: &toml_edit::Value) -> Vec<String> {
        if let Some(request) = spec.as_str() {
            vec![request.into()]
        } else if let Some(requests) = spec.as_array() {
            requests.iter().flat_map(value).collect()
        } else {
            spec.as_inline_table()
                .and_then(|table| table.get("version"))
                .map(value)
                .unwrap_or_default()
        }
    }
    match spec.as_value() {
        Some(spec) => value(spec).into_iter().collect(),
        None => spec.get("version").map(requests).unwrap_or_default(),
    }
}

/// One version a declaration requests, with the options it asks mise for.
struct Request {
    version: String,
    options: BTreeMap<String, toml_edit::Value>,
}

/// What a declaration requests, or `None` for a form that is not a version
/// string, a table with a string `version`, or an array of those.
fn request_list(spec: &toml_edit::Item) -> Option<Vec<Request>> {
    fn table(table: &dyn toml_edit::TableLike, out: &mut Vec<Request>) -> Option<()> {
        out.push(Request {
            version: table.get("version")?.as_str()?.to_owned(),
            options: table
                .iter()
                .filter(|(key, _)| *key != "version")
                .map(|(key, value)| Some((key.to_owned(), value.as_value()?.clone())))
                .collect::<Option<_>>()?,
        });
        Some(())
    }
    fn each(value: &toml_edit::Value, out: &mut Vec<Request>) -> Option<()> {
        if let Some(version) = value.as_str() {
            out.push(Request {
                version: version.to_owned(),
                options: BTreeMap::new(),
            });
            Some(())
        } else if let Some(values) = value.as_array() {
            values.iter().try_for_each(|value| each(value, out))
        } else {
            table(value.as_inline_table()?, out)
        }
    }
    let mut out = Vec::new();
    match spec.as_value() {
        Some(value) => each(value, &mut out)?,
        None => table(spec.as_table_like()?, &mut out)?,
    }
    Some(out)
}

/// The options every request in `requests` shares, compared as parsed values so
/// quoting and spacing do not matter, or `None` when they differ among them.
fn shared_options(requests: &[Request]) -> Option<BTreeMap<String, Value>> {
    let parsed = |request: &Request| -> BTreeMap<String, Value> {
        request
            .options
            .iter()
            .map(|(key, option)| (key.clone(), value(option)))
            .collect()
    };
    let first = parsed(requests.first()?);
    requests
        .iter()
        .all(|request| parsed(request) == first)
        .then_some(first)
}

/// A tool a task installs again, merged into its declaration in the
/// configuration. mise locks one version with other options as an entry of its
/// own, which one declaration cannot express, so options must agree; another
/// version with the same options is requested beside the first, as a string
/// or, with options, as one table per version. `None` leaves it as declared.
fn merged(
    name: &str,
    declared: &toml_edit::Item,
    task: &toml_edit::Item,
) -> Result<Option<toml_edit::Item>, String> {
    let (Some(mut all), Some(more)) = (request_list(declared), request_list(task)) else {
        return Err(format!(
            "{name} has a declaration the Count cannot compare with a task's"
        ));
    };
    let (Some(options), Some(other)) = (shared_options(&all), shared_options(&more)) else {
        return Err(format!(
            "{name} is declared with several sets of options, beside a task that declares it too"
        ));
    };
    if options != other {
        return Err(format!(
            "{name} is declared with different options by a task and elsewhere"
        ));
    }
    let known = all.len();
    for request in more {
        if !all.iter().any(|known| known.version == request.version) {
            all.push(request);
        }
    }
    if all.len() == known {
        return Ok(None);
    }
    let mut array = toml_edit::Array::new();
    for request in &all {
        if request.options.is_empty() {
            array.push(request.version.as_str());
        } else {
            let mut table = toml_edit::InlineTable::new();
            table.insert("version", request.version.as_str().into());
            for (key, option) in &request.options {
                let mut option = option.clone();
                option.decor_mut().clear();
                table.insert(key, option);
            }
            array.push(table);
        }
    }
    Ok(Some(toml_edit::value(array)))
}

/// The selected tools a task installs for itself.
fn task_declarations(
    task: &toml_edit::Item,
    selected: &BTreeSet<String>,
    out: &mut Vec<(String, toml_edit::Item)>,
) {
    if let Some(declarations) = task.get("tools").and_then(toml_edit::Item::as_table_like) {
        out.extend(
            declarations
                .iter()
                .filter(|(name, _)| selected.contains(*name))
                .map(|(name, spec)| (name.to_owned(), spec.clone())),
        );
    }
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
        let mut task_tools = Vec::new();
        let mut includes: Vec<&str> = Vec::new();
        let mut paths: Vec<_> = baseline
            .keys()
            .filter(|path| {
                matches!(
                    path.rsplit('/').next(),
                    Some("mise.toml" | ".mise.toml" | ".tool-versions")
                ) && (super::directory(path) == directory.as_str()
                    || under(directory, super::directory(path)))
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
                for task in doc
                    .get("tasks")
                    .and_then(toml_edit::Item::as_table_like)
                    .into_iter()
                    .flat_map(|tasks| tasks.iter().map(|(_, task)| task))
                {
                    task_declarations(task, &selected, &mut task_tools);
                }
                // TOML task files the configuration includes, by file, glob or
                // directory, hold tasks as top-level tables.
                for include in doc
                    .get("task_config")
                    .and_then(|config| config.get("includes"))
                    .and_then(toml_edit::Item::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(toml_edit::Value::as_str)
                {
                    includes.extend(patchula_manager::included_files(
                        super::directory(path),
                        include,
                        baseline.keys().map(String::as_str),
                    ));
                }
            }
        }
        includes.sort_unstable();
        includes.dedup();
        for include in includes {
            // A task file the update leaves alone is not checked out.
            let text = match std::fs::read_to_string(root.join(include)) {
                Ok(text) => text,
                Err(_) => baseline[include].content.clone(),
            };
            let doc = toml_edit::Document::parse(text).map_err(|e| e.to_string())?;
            for (_, task) in doc.iter() {
                task_declarations(task, &selected, &mut task_tools);
            }
        }
        // A tool a task installs is locked beside the tools the configuration
        // installs.
        for (name, spec) in task_tools {
            match tools.get(&name) {
                None => {
                    tools.insert(&name, spec);
                }
                Some(declared) => {
                    if let Some(item) = merged(&name, declared, &spec)? {
                        tools[name.as_str()] = item;
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
        let mut existing_platforms: BTreeSet<_> = selected
            .iter()
            .flat_map(|name| platforms(&before["tools"][name]))
            .collect();
        // Some tools, such as core:rust, record no platform entries. They lock
        // for the platforms the rest of the lockfile records.
        if existing_platforms.is_empty() {
            existing_platforms = old_tools.values().flat_map(platforms).collect();
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
        ]);
        // A lockfile without any platform entry has none to preserve.
        if !existing_platforms.is_empty() {
            command.args.extend([
                "--platform".into(),
                existing_platforms.into_iter().collect::<Vec<_>>().join(","),
            ]);
        }
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
            // The lock must answer the request the manifest makes: `latest` or
            // `24` for `2.10.0` means a declaration other than the edited one
            // was locked, and `mise` would call the lock out of date.
            let declared = requests(&doc["tools"][name.as_str()]);
            let recorded: BTreeSet<String> = entries
                .iter()
                .filter_map(|entry| entry["specifiers"].as_array())
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            if !declared.is_empty() && recorded != declared {
                return Err(format!(
                    "mise lock recorded the requests {recorded:?} for {name}, not the manifest's {declared:?}"
                ));
            }
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
