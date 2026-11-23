//! Targeted mise.lock updates using a literal, tools-only configuration.
use super::*;
use serde_json::{Value, json};

pub(super) static DESCRIPTOR: Tool = Tool {
    steers: &[],
    tools: &[],
    install: Install::Mise,
    proof: Proof::None,
    run: Run::Mise,
    output: Output::Default,
};

/// A tool pin one mise configuration file declares.
pub(super) enum Pin {
    /// `.tool-versions`: the version words on the line, in order.
    Words(Vec<String>),
    /// `mise.toml` or `.mise.toml`: the `[tools]` declaration as written.
    Spec(toml_edit::Item),
}

/// The tool pins `path`'s text declares, the one parse `versions` and
/// `mise_lock` share: `.tool-versions` lines give a name plus its version
/// words; a TOML config's `[tools]` table gives each entry's item.
pub(super) fn pins(path: &str, text: &str) -> Result<Vec<(String, Pin)>, String> {
    if path.ends_with(".tool-versions") {
        return Ok(text
            .lines()
            .filter_map(|line| {
                let mut words = line.split('#').next().unwrap_or("").split_whitespace();
                let name = words.next()?;
                Some((
                    name.to_owned(),
                    Pin::Words(words.map(str::to_owned).collect()),
                ))
            })
            .collect());
    }
    // A `DocumentMut` so each spec's decor survives the clone into `Pin`.
    let doc = toml_edit::Document::parse(text)
        .map_err(|e| e.to_string())?
        .into_mut();
    Ok(doc
        .get("tools")
        .and_then(toml_edit::Item::as_table_like)
        .into_iter()
        .flat_map(|tools| tools.iter())
        .map(|(name, spec)| (name.to_owned(), Pin::Spec(spec.clone())))
        .collect())
}

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
/// The top-level keys, and the `tools` entries by name, that differ between
/// two parsed lockfiles.
fn changed(before: &Value, after: &Value) -> Vec<String> {
    let empty = serde_json::Map::new();
    let keys = |value: &Value| -> BTreeSet<String> {
        value
            .as_object()
            .unwrap_or(&empty)
            .keys()
            .cloned()
            .collect()
    };
    let mut changed = Vec::new();
    for key in keys(before).union(&keys(after)) {
        if key == "tools" {
            let (old, new) = (&before["tools"], &after["tools"]);
            for name in keys(old).union(&keys(new)) {
                if old.get(name) != new.get(name) {
                    changed.push(format!("tools.{name}"));
                }
            }
        } else if before.get(key) != after.get(key) {
            changed.push(key.clone());
        }
    }
    changed
}

/// The key mise records a tool under in `mise.lock`: its `BackendArg` short
/// name, which drops the bracketed options and a leading `core:`, and
/// unaliases `nodejs`, `golang` and `dotnet-core` (`BackendArg::from`,
/// `strip_opts` and `unalias_backend` in mise's source). Registry shorthands
/// stay as written: `gh` is recorded as `gh`. Only meaningful for a name
/// `patchula_mise::check` admits, whose brackets close at its end.
fn lock_key(tool: &str) -> String {
    let name = patchula_mise::split(tool).0;
    let name = name.strip_prefix("core:").unwrap_or(name);
    match name {
        "nodejs" => "node",
        "golang" => "go",
        "dotnet-core" => "dotnet",
        name => name,
    }
    .to_owned()
}

/// A parsed lockfile without its `tools` table.
fn without_tools(lock: &Value) -> Value {
    let mut lock = lock.clone();
    if let Some(top) = lock.as_object_mut() {
        top.remove("tools");
    }
    lock
}

/// Whether `after` is `before` with nothing changed but table keys left out,
/// as a mise that does not know a field writes an entry back. Arrays keep
/// their length and scalars their value.
fn omits_only(before: &Value, after: &Value) -> bool {
    match (before, after) {
        (Value::Object(before), Value::Object(after)) => after.iter().all(|(key, after)| {
            before
                .get(key)
                .is_some_and(|before| omits_only(before, after))
        }),
        (Value::Array(before), Value::Array(after)) => {
            before.len() == after.len()
                && before
                    .iter()
                    .zip(after)
                    .all(|(before, after)| omits_only(before, after))
        }
        (before, after) => before == after,
    }
}

/// One table section of a lockfile: its header line through the line before
/// the next header, and the `tools` entry it belongs to, if any.
struct Section<'a> {
    tool: Option<String>,
    text: &'a str,
}

/// `text` split at its table headers: what precedes the first header, then
/// each section. A header starts in the first column, as mise writes it; a
/// first-column `[` line that is no header is refused rather than guessed.
fn sections(text: &str) -> Result<(&str, Vec<Section<'_>>), String> {
    let mut starts = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if line.starts_with('[') {
            let header = line.trim_end();
            let inner = header
                .strip_prefix("[[")
                .and_then(|rest| rest.strip_suffix("]]"))
                .or_else(|| {
                    header
                        .strip_prefix('[')
                        .and_then(|rest| rest.strip_suffix(']'))
                })
                .ok_or_else(|| format!("mise.lock: unreadable table header {header}"))?;
            let keys = toml_edit::Key::parse(inner.trim())
                .map_err(|e| format!("mise.lock: unreadable table header {header}: {e}"))?;
            let tool = match keys.as_slice() {
                [tools, tool, ..] if tools.get() == "tools" => Some(tool.get().to_owned()),
                _ => None,
            };
            starts.push((offset, tool));
        }
        offset += line.len();
    }
    let preamble = &text[..starts.first().map_or(text.len(), |(start, _)| *start)];
    let sections = starts
        .iter()
        .enumerate()
        .map(|(i, (start, tool))| Section {
            tool: tool.clone(),
            text: &text[*start..starts.get(i + 1).map_or(text.len(), |(next, _)| *next)],
        })
        .collect();
    Ok((preamble, sections))
}

/// The committed lockfile with each `take` tool's sections replaced by the
/// generated lockfile's, every other byte kept. A taken tool the committed
/// lock lacks is inserted before the first tool that sorts after it.
fn graft(original: &str, generated: &str, take: &BTreeSet<String>) -> Result<String, String> {
    let (preamble, committed) = sections(original)?;
    let (_, fresh) = sections(generated)?;
    let mut blocks: BTreeMap<&str, String> = BTreeMap::new();
    for section in &fresh {
        if let Some(tool) = section.tool.as_deref().filter(|tool| take.contains(*tool)) {
            let block = blocks.entry(tool).or_default();
            block.push_str(section.text);
        }
    }
    // A block cut from the end of the generated file may lack the blank
    // line that separates it from the next section.
    for block in blocks.values_mut() {
        if !block.ends_with("\n\n") {
            block.push_str(if block.ends_with('\n') { "\n" } else { "\n\n" });
        }
    }
    let present: BTreeSet<&str> = committed.iter().filter_map(|s| s.tool.as_deref()).collect();
    let mut inserts: Vec<&str> = blocks
        .keys()
        .copied()
        .filter(|tool| !present.contains(tool))
        .collect();
    inserts.reverse();
    let mut out = String::from(preamble);
    let mut emitted = BTreeSet::new();
    for section in &committed {
        if let Some(tool) = section.tool.as_deref() {
            while inserts.last().is_some_and(|insert| *insert < tool) {
                out.push_str(&blocks[inserts.pop().unwrap()]);
            }
            if take.contains(tool) {
                if emitted.insert(tool) {
                    out.push_str(
                        blocks
                            .get(tool)
                            .ok_or("mise lock removed a selected tool")?,
                    );
                }
                continue;
            }
        }
        out.push_str(section.text);
    }
    while let Some(insert) = inserts.pop() {
        out.push_str(&blocks[insert]);
    }
    // The file ends as the committed one does.
    let ending = if original.ends_with('\n') { "\n" } else { "" };
    Ok(format!("{}{ending}", out.trim_end()))
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

/// One version a declaration requests, with the options it asks mise for.
struct Request {
    version: String,
    options: BTreeMap<String, toml_edit::Value>,
}

/// What a declaration requests, or `None` for a form that is not a version
/// string, a table whose `version` is a string or an array of strings, or an
/// array of those.
fn request_list(spec: &toml_edit::Item) -> Option<Vec<Request>> {
    fn table(table: &dyn toml_edit::TableLike, out: &mut Vec<Request>) -> Option<()> {
        let options: BTreeMap<_, _> = table
            .iter()
            .filter(|(key, _)| *key != "version")
            .map(|(key, value)| Some((key.to_owned(), value.as_value()?.clone())))
            .collect::<Option<_>>()?;
        let versions: Vec<&str> = match table.get("version")?.as_value()? {
            toml_edit::Value::Array(versions) => versions
                .iter()
                .map(toml_edit::Value::as_str)
                .collect::<Option<_>>()?,
            version => vec![version.as_str()?],
        };
        out.extend(versions.into_iter().map(|version| Request {
            version: version.to_owned(),
            options: options.clone(),
        }));
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

/// The tool arguments a declaration hands mise: the name, with each
/// request's options in brackets as a lookup passes them.
fn arguments(name: &str, spec: &toml_edit::Item) -> Result<Vec<String>, String> {
    let bracketed = |table: &dyn toml_edit::TableLike| {
        patchula_mise::bracketed(name, table)
            .map_err(|e| format!("mise.lock is not regenerated: {name}: {e}"))
    };
    let unreadable =
        || format!("mise.lock is not regenerated: {name} has a declaration the Count cannot check");
    match spec {
        toml_edit::Item::Table(table) => Ok(vec![bracketed(table)?]),
        toml_edit::Item::Value(toml_edit::Value::InlineTable(table)) => Ok(vec![bracketed(table)?]),
        toml_edit::Item::Value(toml_edit::Value::String(_)) => Ok(vec![name.to_owned()]),
        toml_edit::Item::Value(toml_edit::Value::Array(requests)) => requests
            .iter()
            .map(|request| match request {
                toml_edit::Value::String(_) => Ok(name.to_owned()),
                toml_edit::Value::InlineTable(table) => bracketed(table),
                _ => Err(unreadable()),
            })
            .collect(),
        _ => Err(unreadable()),
    }
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
                .filter(|(name, _)| selected.contains(&lock_key(name)))
                .map(|(name, spec)| (name.to_owned(), spec.clone())),
        );
    }
}

impl Runner {
    pub(super) async fn mise_lock(
        &self,
        context: context::Context<'_>,
        jobs: &[&Job],
    ) -> Result<(), Failure> {
        let context::Context {
            driver,
            root,
            baseline,
            directory,
            config,
            maintenance,
            public,
            ..
        } = context;
        let lock_path = context::join(directory, "mise.lock");
        let original = &baseline
            .get(&lock_path)
            .ok_or("missing committed mise.lock")?
            .content;
        let before = parsed(original)?;
        let old_tools = before["tools"]
            .as_object()
            .ok_or("mise.lock has no tool entries")?;
        // The selected tools by the key mise records them under in
        // mise.lock; a manifest may spell a tool otherwise (`nodejs`,
        // `x[bin=y]`), and mise is handed the manifest's spelling.
        let selected: BTreeSet<String> = if maintenance {
            old_tools.keys().cloned().collect()
        } else {
            jobs.iter()
                .filter_map(|job| job.package().map(lock_key))
                .collect()
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
                ) && within(directory, super::directory(path))
            })
            .collect();
        paths.sort_by_key(|path| path.len());
        for path in paths {
            let text = std::fs::read_to_string(root.join(path)).map_err(|e| e.to_string())?;
            for (name, pin) in pins(path, &text)? {
                if !selected.contains(&lock_key(&name)) {
                    continue;
                }
                match pin {
                    Pin::Words(words) => {
                        if words.len() == 1 {
                            tools[name.as_str()] = toml_edit::value(words[0].as_str());
                        } else {
                            tools[name.as_str()] =
                                toml_edit::value(words.into_iter().collect::<toml_edit::Array>());
                        }
                    }
                    Pin::Spec(spec) => {
                        tools.insert(name.as_str(), spec);
                    }
                }
            }
            if path.ends_with(".tool-versions") {
                continue;
            }
            let doc = toml_edit::Document::parse(text)
                .map_err(|e| e.to_string())?
                .into_mut();
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
        // mise locks each tool with its options as it would list it, so each
        // passes the checks a lookup makes before mise sees it.
        let requested = tools
            .iter()
            .map(|(name, spec)| arguments(name, spec))
            .collect::<Result<Vec<_>, _>>()?
            .concat();
        for tool in requested {
            patchula_mise::admit(&tool)
                .await
                .map_err(|e| format!("mise.lock is not regenerated: {e}"))?;
        }
        // Each selected lock key and the one manifest name that declares it,
        // read once every name is admitted, so its brackets close at its end.
        let mut manifest: BTreeMap<String, String> = BTreeMap::new();
        for (name, _) in tools.iter() {
            if let Some(other) = manifest.insert(lock_key(name), name.to_owned()) {
                return Err(format!(
                    "mise.lock records {name} and {other} under one name; the Count cannot tell which to lock"
                )
                .into());
            }
        }
        if selected.is_empty() || selected.iter().any(|key| !manifest.contains_key(key)) {
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
        let mut args = vec![
            "--cd".into(),
            work.to_string_lossy().into_owned(),
            "lock".into(),
        ];
        // A lockfile without any platform entry has none to preserve.
        if !existing_platforms.is_empty() {
            args.extend([
                "--platform".into(),
                existing_platforms.into_iter().collect::<Vec<_>>().join(","),
            ]);
        }
        if maintenance {
            args.extend([
                "--bump".into(),
                "--minimum-release-age".into(),
                format!("{}h", config.defaults.wait_hours),
            ]);
        }
        args.extend(manifest.values().cloned());
        self.tool(
            driver,
            Invocation {
                descriptor: &DESCRIPTOR,
                tools: vec![(
                    patchula_mise::PACKAGE.into(),
                    default_version("mise")?.into(),
                )],
                bin: caches::Bin::Mise,
                cwd: None,
                env: Vec::new(),
                held: credentials::Held::default(),
                refused: None,
                public,
                args,
            },
        )
        .await?;
        if std::fs::read_to_string(work.join("mise.toml")).map_err(|e| e.to_string())? != sanitized
        {
            return Err("mise lock modified the requested tool declarations".into());
        }
        let generated =
            std::fs::read_to_string(work.join("mise.lock")).map_err(|e| e.to_string())?;
        let after = parsed(&generated)?;
        for key in &selected {
            let name = &manifest[key];
            let entries = after["tools"][key]
                .as_array()
                .filter(|entries| !entries.is_empty())
                .ok_or("mise lock removed a selected tool")?;
            // The lock must answer the request the manifest makes: `latest` or
            // `24` for `2.10.0` means a declaration other than the edited one
            // was locked, and `mise` would call the lock out of date.
            // mise records each version a declaration requests as a
            // specifier. `arguments` admitted the declaration; one this
            // grammar cannot read names no request to compare.
            let declared: BTreeSet<String> = request_list(&doc["tools"][name.as_str()])
                .into_iter()
                .flatten()
                .map(|request| request.version)
                .collect();
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
                )
                .into());
            }
            if !platforms(&before["tools"][key]).is_subset(&platforms(&after["tools"][key])) {
                return Err(format!("mise lock dropped an existing platform for {name}").into());
            }
            for job in jobs
                .iter()
                .filter(|job| job.package().map(lock_key).as_ref() == Some(key))
            {
                if let Some(target) = job.target()
                    && !entries.iter().any(|entry| {
                        entry["version"].as_str().is_some_and(|v| {
                            v.trim_start_matches('v') == target.trim_start_matches('v')
                        })
                    })
                {
                    return Err(format!("mise lock did not resolve {name} to {target}").into());
                }
            }
        }
        // Everything outside `tools` is the lockfile's own metadata and
        // sections a tool entry may refer to: any change, a lost key
        // included, holds.
        if without_tools(&before) != without_tools(&after) {
            return Err(format!(
                "mise lock changed lockfile metadata: {}",
                changed(&without_tools(&before), &without_tools(&after)).join(", ")
            )
            .into());
        }
        // A tool entry mise left alone, or wrote back without fields it does
        // not know, keeps its committed bytes: a newer mise may have written
        // them. A selected tool mise changed takes mise's entry. Any other
        // change is one the Count did not ask for and holds.
        let empty = serde_json::Map::new();
        let old = before["tools"].as_object().unwrap_or(&empty);
        let new = after["tools"].as_object().unwrap_or(&empty);
        let mut take = BTreeSet::new();
        let mut unrelated = Vec::new();
        for key in old.keys().chain(new.keys()).collect::<BTreeSet<_>>() {
            match (old.get(key), new.get(key)) {
                (Some(old), Some(new)) if omits_only(old, new) => {}
                (_, Some(_)) if selected.contains(key) => {
                    take.insert(key.clone());
                }
                _ => unrelated.push(format!("tools.{key}")),
            }
        }
        if !unrelated.is_empty() {
            return Err(format!(
                "mise lock changed unrelated entries: {}",
                unrelated.join(", ")
            )
            .into());
        }
        let grafted = graft(original, &generated, &take)?;
        // The grafted text must read as the committed lock with exactly the
        // taken entries replaced by mise's.
        let written = parsed(&grafted)?;
        let mut expected = before.clone();
        for key in &take {
            expected["tools"][key] = after["tools"][key].clone();
        }
        if written != expected {
            return Err("the grafted mise.lock does not read back as planned".into());
        }
        if collect(&work)?
            .keys()
            .any(|path| !matches!(path.as_str(), "mise.toml" | "mise.lock"))
        {
            return Err("mise lock produced unsupported sidecar files".into());
        }
        write(root, &lock_path, &grafted).map_err(Failure::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each declaration and the key mise 2026.11.12 recorded it under in
    /// `mise.lock`, from `mise lock` runs on one-tool configurations.
    #[test]
    fn lock_keys_match_what_mise_records() {
        for (tool, key) in [
            ("node", "node"),
            ("nodejs", "node"),
            ("core:node", "node"),
            ("golang", "go"),
            ("aqua:jdx/mise", "aqua:jdx/mise"),
            (
                "github:github/gh-actions-lock[bin=gh-actions-lock]",
                "github:github/gh-actions-lock",
            ),
        ] {
            assert_eq!(lock_key(tool), key, "{tool}");
        }
    }

    fn spec(text: &str) -> toml_edit::Item {
        toml_edit::Document::parse(format!("tool = {text}"))
            .unwrap()
            .into_mut()["tool"]
            .clone()
    }
    fn versions(text: &str) -> Option<Vec<String>> {
        request_list(&spec(text)).map(|requests| {
            requests
                .into_iter()
                .map(|request| request.version)
                .collect()
        })
    }

    /// A table whose `version` is an array requests each version with the
    /// table's options, as an array of tables would.
    #[test]
    fn an_array_version_requests_each_version() {
        assert_eq!(
            versions("{ version = ['1.2', '1.3'], os = ['linux'] }").unwrap(),
            ["1.2", "1.3"]
        );
        let requests = request_list(&spec("{ version = ['1.2', '1.3'], os = 'linux' }")).unwrap();
        assert!(
            requests
                .iter()
                .all(|request| request.options.contains_key("os"))
        );
        let table = toml_edit::Document::parse("[tool]\nversion = ['1', '2']\n")
            .unwrap()
            .into_mut()["tool"]
            .clone();
        assert_eq!(request_list(&table).unwrap().len(), 2);
        assert_eq!(
            versions("['1', { version = ['2', '3'] }]").unwrap(),
            ["1", "2", "3"]
        );
        // A version that is not a string, or an array holding one, is not
        // read.
        assert!(versions("{ version = [1] }").is_none());
        assert!(versions("{ version = 1 }").is_none());
        assert!(versions("{ os = 'linux' }").is_none());
    }

    /// A task's tool merges into a declaration with an array `version`,
    /// written back as one table per version.
    #[test]
    fn a_task_merges_into_an_array_version() {
        let merged = merged(
            "tool",
            &spec("{ version = ['1', '2'], os = 'linux' }"),
            &spec("{ version = '3', os = 'linux' }"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            merged.to_string().trim(),
            r#"[{ version = "1", os = 'linux' }, { version = "2", os = 'linux' }, { version = "3", os = 'linux' }]"#
        );
        // A version already requested adds nothing.
        assert!(
            merged_none("{ version = ['1', '2'] }", "'2'"),
            "nothing to add"
        );
        // Options must still agree.
        assert!(
            super::merged(
                "tool",
                &spec("{ version = ['1', '2'], os = 'linux' }"),
                &spec("'3'"),
            )
            .is_err()
        );
    }
    fn merged_none(declared: &str, task: &str) -> bool {
        super::merged("tool", &spec(declared), &spec(task))
            .unwrap()
            .is_none()
    }
}
