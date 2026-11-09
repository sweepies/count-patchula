//! The index the Count gives each Python package, checked against the index
//! real uv and Poetry take it from, and the keys the Count reads checked
//! against the tools' own lists.
//!
//! A local server stands in for any number of package indexes, each serving
//! every project at 1.0.0. PyPI itself is out of reach behind a dead proxy,
//! so a tool that takes a package from PyPI fails. Each package is resolved
//! on its own, so one that only PyPI serves cannot hide another's index.
//!
//! The projects are generated from every combination of how an index is
//! declared, what it is for, how a pin is written and where the project
//! sits, rather than from cases picked by hand.
//!
//! The tool resolves each project first, and the Count then reads it with the
//! lockfile the tool wrote, as it finds it in a repository: a check of which
//! lockfile it reads, and how, against the tool's own output.
//!
//! The Count may be stricter than a tool (an ambiguous or unusable index
//! fails its lookup), but where it names an index it must be the tool's, and
//! where it names none the tool must not take the package from an index the
//! project configures.
use super::{config, default_spec};
use patchula_config::Manager;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Copy, Debug)]
enum Tool {
    Uv,
    Poetry,
}

/// One generated project: its files, with `{base}` for the server.
struct Case {
    name: String,
    tool: Tool,
    files: Vec<(String, String)>,
    /// The `pyproject.toml` whose dependencies the tool resolves.
    project: String,
}

const PACKAGES: [&str; 2] = ["fixture-a", "fixture-b"];

fn project_table(name: &str) -> String {
    format!(
        "[project]\nname = \"{name}\"\nversion = \"1.0.0\"\nrequires-python = \">=3.12\"\ndependencies = [\"fixture-a>=1\", \"fixture-b>=1\"]\n"
    )
}

/// How a uv project declares the index at `{base}/corp/simple`.
#[derive(Clone, Copy, Debug)]
enum UvDeclaration {
    ArrayOfTables,
    InlineArray,
    ArrayOfTablesBesideUvToml,
    UvToml,
    IndexUrl,
    UvTomlIndexUrl,
}

/// What the index is for; only a named index can be pinned to.
#[derive(Clone, Copy, Debug)]
enum Role {
    Default,
    Searched,
    Pinned(PinForm),
}

/// How `fixture-a` is pinned to the index.
#[derive(Clone, Copy, Debug)]
enum PinForm {
    Table,
    InlineList,
    ArrayOfTables,
}

fn uv_cases() -> Vec<Case> {
    use UvDeclaration::*;
    let mut cases = Vec::new();
    let roles = [
        Role::Default,
        Role::Searched,
        Role::Pinned(PinForm::Table),
        Role::Pinned(PinForm::InlineList),
        Role::Pinned(PinForm::ArrayOfTables),
    ];
    for declaration in [
        ArrayOfTables,
        InlineArray,
        ArrayOfTablesBesideUvToml,
        UvToml,
        IndexUrl,
        UvTomlIndexUrl,
    ] {
        for role in roles {
            if matches!(declaration, IndexUrl | UvTomlIndexUrl) && !matches!(role, Role::Default) {
                continue;
            }
            for member in [false, true] {
                cases.push(uv_case(declaration, role, member));
            }
        }
    }
    cases
}

fn uv_case(declaration: UvDeclaration, role: Role, member: bool) -> Case {
    use UvDeclaration::*;
    let flags = match role {
        Role::Default => "default = true\n",
        Role::Searched => "",
        Role::Pinned(_) => "explicit = true\n",
    };
    let entry = format!("name = \"corp\"\nurl = \"{{base}}/corp/simple\"\n{flags}");
    let inline_flags = match role {
        Role::Default => ", default = true",
        Role::Searched => "",
        Role::Pinned(_) => ", explicit = true",
    };
    // Settings in the project's `[tool.uv]`, or in a `uv.toml` beside it.
    let (tool_uv, uv_toml) = match declaration {
        ArrayOfTables => (format!("[[tool.uv.index]]\n{entry}"), None),
        InlineArray => (
            format!(
                "[tool.uv]\nindex = [{{ name = \"corp\", url = \"{{base}}/corp/simple\"{inline_flags} }}]\n"
            ),
            None,
        ),
        ArrayOfTablesBesideUvToml => (
            format!("[[tool.uv.index]]\n{entry}"),
            Some("native-tls = false\n".to_owned()),
        ),
        UvToml => (String::new(), Some(format!("[[index]]\n{entry}"))),
        IndexUrl => (
            "[tool.uv]\nindex-url = \"{base}/corp/simple\"\n".to_owned(),
            None,
        ),
        UvTomlIndexUrl => (
            String::new(),
            Some("index-url = \"{base}/corp/simple\"\n".to_owned()),
        ),
    };
    let pin = match role {
        Role::Pinned(PinForm::Table) => {
            "[tool.uv.sources]\nfixture-a = { index = \"corp\" }\n".to_owned()
        }
        Role::Pinned(PinForm::InlineList) => {
            "[tool.uv.sources]\nfixture-a = [{ index = \"corp\" }]\n".to_owned()
        }
        Role::Pinned(PinForm::ArrayOfTables) => {
            "[[tool.uv.sources.fixture-a]]\nindex = \"corp\"\n".to_owned()
        }
        _ => String::new(),
    };
    let mut files = Vec::new();
    let project = if member {
        // The workspace root holds the settings; the member, its own
        // `[tool.uv]` (which uv ignores) and the pin.
        files.push((
            "pyproject.toml".to_owned(),
            format!(
                "[project]\nname = \"root\"\nversion = \"1.0.0\"\nrequires-python = \">=3.12\"\ndependencies = []\n[tool.uv.workspace]\nmembers = [\"./packages/*/\"]\n{tool_uv}"
            ),
        ));
        files.push((
            "packages/app/pyproject.toml".to_owned(),
            format!(
                "{}[tool.uv]\nnative-tls = false\n{pin}",
                project_table("app")
            ),
        ));
        "packages/app/pyproject.toml".to_owned()
    } else {
        files.push((
            "pyproject.toml".to_owned(),
            format!("{}{tool_uv}{pin}", project_table("fixture")),
        ));
        "pyproject.toml".to_owned()
    };
    if let Some(uv_toml) = uv_toml {
        files.push(("uv.toml".to_owned(), uv_toml));
    }
    Case {
        name: format!(
            "uv {declaration:?} {role:?}{}",
            if member { " in a workspace member" } else { "" }
        ),
        tool: Tool::Uv,
        files,
        project,
    }
}

fn poetry_cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for inline in [false, true] {
        for (role, pin_name) in [
            ("primary", None),
            ("supplemental", None),
            ("explicit", Some("corp")),
            ("explicit", Some("Corp")),
            ("supplemental", Some("corp")),
        ] {
            let source = if inline {
                format!(
                    "source = [{{ name = \"corp\", url = \"{{base}}/corp/simple\", priority = \"{role}\" }}]\n"
                )
            } else {
                String::new()
            };
            let sources_table = if inline {
                String::new()
            } else {
                format!(
                    "[[tool.poetry.source]]\nname = \"corp\"\nurl = \"{{base}}/corp/simple\"\npriority = \"{role}\"\n"
                )
            };
            let fixture_a = match pin_name {
                Some(name) => format!("{{ version = \"^1.0\", source = \"{name}\" }}"),
                None => "\"^1.0\"".to_owned(),
            };
            let text = format!(
                "[tool.poetry]\npackage-mode = false\n{source}[tool.poetry.dependencies]\npython = \">=3.12\"\nfixture-a = {fixture_a}\nfixture-b = \"^1.0\"\n{sources_table}"
            );
            cases.push(Case {
                name: format!(
                    "Poetry {role}{}{}",
                    pin_name.map_or(String::new(), |n| format!(" pinned as {n}")),
                    if inline { " inline" } else { "" }
                ),
                tool: Tool::Poetry,
                files: vec![("pyproject.toml".to_owned(), text)],
                project: "pyproject.toml".to_owned(),
            });
        }
    }
    cases
}

/// Cases beyond the generated ones: where uv reads overrides, `no-sources`,
/// and pins a workspace root sets for its members.
fn uv_special_cases() -> Vec<Case> {
    let wheel = "{base}/other/files/fixture_b-1.0.0-py3-none-any.whl";
    let explicit =
        "[[tool.uv.index]]\nname = \"corp\"\nurl = \"{base}/corp/simple\"\nexplicit = true\n";
    let case = |name: &str, files: Vec<(&str, String)>, project: &str| Case {
        name: name.into(),
        tool: Tool::Uv,
        files: files
            .into_iter()
            .map(|(path, text)| (path.to_owned(), text))
            .collect(),
        project: project.into(),
    };
    vec![
        case(
            "uv override in pyproject.toml",
            vec![(
                "pyproject.toml",
                format!(
                    "{}[tool.uv]\noverride-dependencies = [\"fixture-b @ {wheel}\"]\n",
                    project_table("fixture")
                ),
            )],
            "pyproject.toml",
        ),
        case(
            "uv override in uv.toml",
            vec![
                ("pyproject.toml", project_table("fixture")),
                (
                    "uv.toml",
                    format!("override-dependencies = [\"fixture-b @ {wheel}\"]\n"),
                ),
            ],
            "pyproject.toml",
        ),
        case(
            "uv no-sources",
            vec![(
                "pyproject.toml",
                format!(
                    "{}{explicit}[tool.uv]\nno-sources = true\n[tool.uv.sources]\nfixture-a = {{ index = \"corp\" }}\n",
                    project_table("fixture")
                ),
            )],
            "pyproject.toml",
        ),
        case(
            "uv pin set by the workspace root",
            vec![
                (
                    "pyproject.toml",
                    format!(
                        "[project]\nname = \"root\"\nversion = \"1.0.0\"\nrequires-python = \">=3.12\"\ndependencies = []\n[tool.uv.workspace]\nmembers = [\"packages/*\"]\n{explicit}[tool.uv.sources]\nfixture-a = {{ index = \"corp\" }}\n"
                    ),
                ),
                ("packages/app/pyproject.toml", project_table("app")),
            ],
            "packages/app/pyproject.toml",
        ),
    ]
}

/// What the Count says of a package.
#[derive(Debug, PartialEq, Eq)]
enum Expect {
    /// Looked up on PyPI.
    PyPi,
    /// Looked up on this index.
    Index(String),
    /// Not looked up at all: it comes from a URL, path or git.
    NotFromIndex,
}

/// What the tool did with a package.
#[derive(Debug)]
enum Observed {
    /// Locked it from one of the local indexes.
    Local(String),
    /// Locked it from somewhere that is not a local index.
    Elsewhere,
    /// Could not lock it.
    Failed,
}

#[test]
fn real_python_index_choices_match_uv_and_poetry() {
    let base = serve();
    let temp = tempfile::tempdir().unwrap();
    let mut config = config();
    config.managers = vec![Manager::Pyproject];
    let scanner = patchula_manager::Scanner::new(&config).unwrap();
    let mut cases = vec![Case {
        name: "uv without indexes".into(),
        tool: Tool::Uv,
        files: vec![("pyproject.toml".into(), project_table("fixture"))],
        project: "pyproject.toml".into(),
    }];
    cases.extend(uv_cases());
    cases.extend(uv_special_cases());
    cases.extend(poetry_cases());
    let mut mismatches = Vec::new();
    let mut compared = 0;
    for (number, case) in cases.iter().enumerate() {
        for package in PACKAGES {
            let files: BTreeMap<String, String> = case
                .files
                .iter()
                .map(|(path, text)| (path.clone(), only(path, text, package, &base)))
                .collect();
            let dir = temp.path().join(format!("{number}-{package}"));
            for (path, text) in &files {
                let path = dir.join(path);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, text).unwrap();
            }
            // The tool resolves first; the Count then reads the project with
            // the lockfile the tool wrote, as it would in the repository.
            let (observed, log) = resolve(case.tool, &dir, temp.path(), package, &base);
            let mut files = files;
            for lock in ["uv.lock", "poetry.lock"] {
                if let Ok(text) = std::fs::read_to_string(dir.join(lock)) {
                    files.insert(lock.to_owned(), text);
                }
            }
            let Some(expected) = expected(
                &scanner,
                &patchula_manager::all_read(&files),
                &case.project,
                package,
                &case.name,
            ) else {
                continue;
            };
            compared += 1;
            let agrees = match (&expected, &observed) {
                (Expect::Index(url), Observed::Local(local)) => url == local,
                // A tool that took the package from PyPI failed here, since
                // PyPI is out of reach; it must have tried PyPI, not failed
                // for another reason.
                (Expect::PyPi, Observed::Failed) => log.contains("pypi.org"),
                (Expect::NotFromIndex, Observed::Elsewhere) => true,
                _ => false,
            };
            if !agrees {
                mismatches.push(format!(
                    "{} / {package}: the Count says {expected:?}, {:?} did {observed:?}\n{log}",
                    case.name, case.tool,
                ));
            }
        }
    }
    assert!(compared > 20, "only {compared} cases were not ambiguous");
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n\n"));
}

/// `text` with `{base}` filled in and every dependency but `package`
/// removed, so the tool resolves that package alone.
fn only(path: &str, text: &str, package: &str, base: &str) -> String {
    let text = text.replace("{base}", base);
    if !path.ends_with("pyproject.toml") {
        return text;
    }
    let mut doc: toml_edit::DocumentMut = text.parse().unwrap();
    if let Some(deps) = doc
        .get_mut("project")
        .and_then(|p| p.get_mut("dependencies"))
        .and_then(toml_edit::Item::as_array_mut)
    {
        deps.retain(|dep| dep.as_str().is_some_and(|d| d.starts_with(package)));
    }
    if let Some(deps) = doc
        .get_mut("tool")
        .and_then(|t| t.get_mut("poetry"))
        .and_then(|p| p.get_mut("dependencies"))
        .and_then(toml_edit::Item::as_table_mut)
    {
        deps.retain(|name, _| name == "python" || name == package);
    }
    doc.to_string()
}

/// What the Count says of `package` in `project`; none when it is ambiguous
/// or unusable, whose lookup fails rather than guess.
fn expected(
    scanner: &patchula_manager::Scanner,
    files: &patchula_manager::Files,
    project: &str,
    package: &str,
    case: &str,
) -> Option<Expect> {
    let extraction = scanner.extract(files).unwrap();
    let Some(dependency) = extraction
        .dependencies
        .iter()
        .find(|d| d.file == project && d.package == package)
    else {
        assert!(
            extraction
                .skipped
                .iter()
                .any(|s| s.file == project && s.value == package),
            "{case}: {package} was neither extracted nor skipped"
        );
        return Some(Expect::NotFromIndex);
    };
    match dependency.registry.clone() {
        None => Some(Expect::PyPi),
        Some(url) if url.starts_with("http") && !url.contains(' ') => Some(Expect::Index(url)),
        Some(_) => None,
    }
}

/// What the tool did with `package`, and its output.
fn resolve(
    tool_kind: Tool,
    dir: &Path,
    temp: &Path,
    package: &str,
    base: &str,
) -> (Observed, String) {
    let python = default_spec("python");
    let (output, lock, index_key) = match tool_kind {
        Tool::Uv => (
            tool(dir, temp, &[python, default_spec("uv")], &["uv", "lock"]),
            "uv.lock",
            "registry",
        ),
        Tool::Poetry => (
            tool(
                dir,
                temp,
                &[python, default_spec("poetry")],
                &["poetry", "lock"],
            ),
            "poetry.lock",
            "url",
        ),
    };
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let text = std::fs::read_to_string(dir.join(lock)).ok();
    let locked_package = text
        .as_deref()
        .is_some_and(|lock| locked(lock, package, &["name"]).is_some());
    if !output.status.success() || !locked_package {
        return (Observed::Failed, log);
    }
    let index = text
        .as_deref()
        .and_then(|lock| locked(lock, package, &["source", index_key]))
        .and_then(|url| {
            let rest = url.strip_prefix(base)?.trim_start_matches('/');
            let index = rest.split('/').next()?;
            // A Poetry or uv URL source points at a file, not an index.
            (!rest.contains("/files/")).then(|| format!("{base}/{index}/simple"))
        });
    (index.map_or(Observed::Elsewhere, Observed::Local), log)
}

/// The keys between backticks after "expected one of" in a serde error.
fn expected_keys(output: &str) -> BTreeSet<String> {
    let list = output
        .split("expected one of ")
        .nth(1)
        .unwrap_or_else(|| panic!("no list of keys in:\n{output}"));
    let list = list.lines().next().unwrap_or_default();
    list.split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

/// Every setting uv accepts is one the Count reads or knows to be
/// irrelevant, and every one it lists is one uv accepts.
#[test]
fn real_uv_settings_and_source_keys_are_the_ones_the_count_classifies() {
    use patchula_manager::known::{
        UV_IRRELEVANT, UV_PIP_IRRELEVANT, UV_PIP_READ, UV_READ, UV_SOURCE_KEYS,
    };
    let temp = tempfile::tempdir().unwrap();
    let run = |files: &[(&str, &str)]| {
        let dir = tempfile::tempdir_in(temp.path()).unwrap();
        for (path, text) in files {
            std::fs::write(dir.path().join(path), text).unwrap();
        }
        let output = tool(
            dir.path(),
            temp.path(),
            &[default_spec("python"), default_spec("uv")],
            &["uv", "lock"],
        );
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    let project = "[project]\nname = \"p\"\nversion = \"1\"\nrequires-python = \">=3.12\"\ndependencies = [\"a\"]\n";
    let settings = expected_keys(&run(&[
        ("pyproject.toml", project),
        ("uv.toml", "zzz-unknown = 1\n"),
    ]));
    let classified: BTreeSet<String> = UV_READ
        .iter()
        .chain(UV_IRRELEVANT)
        .map(|k| (*k).to_owned())
        .collect();
    assert_eq!(settings, classified);
    let sources = expected_keys(&run(&[(
        "pyproject.toml",
        &format!("{project}[tool.uv.sources]\na = {{ index = \"x\", zzz-unknown = 1 }}\n"),
    )]));
    let known: BTreeSet<String> = UV_SOURCE_KEYS.iter().map(|k| (*k).to_owned()).collect();
    assert_eq!(sources, known);
    let pip = expected_keys(&run(&[(
        "pyproject.toml",
        &format!("{project}[tool.uv.pip]\nzzz-unknown = 1\n"),
    )]));
    let classified: BTreeSet<String> = UV_PIP_READ
        .iter()
        .chain(UV_PIP_IRRELEVANT)
        .map(|k| (*k).to_owned())
        .collect();
    assert_eq!(pip, classified);
}

/// The keys the Count accepts in Poetry sources and dependency tables are
/// the ones Poetry's own schemas allow.
#[test]
fn real_poetry_source_and_dependency_keys_are_the_schemas() {
    use patchula_manager::known::{POETRY_DEPENDENCY_KEYS, POETRY_SOURCE_KEYS};
    let output = Command::new("mise")
        .args(["where", &default_spec("poetry")])
        .env("MISE_OFFLINE", "1")
        .output()
        .unwrap();
    let install = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    let find = |name: &str| -> serde_json::Value {
        let mut pending = vec![install.clone()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.file_name().is_some_and(|f| f == name) {
                    return serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
                }
            }
        }
        panic!("no {name} under {}", install.display());
    };
    let keys = |schema: &serde_json::Value, definition: &str| -> BTreeSet<String> {
        schema["definitions"][definition]["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("no {definition}"))
            .keys()
            .cloned()
            .collect()
    };
    let poetry = find("poetry.json");
    let sources: BTreeSet<String> = POETRY_SOURCE_KEYS.iter().map(|k| (*k).to_owned()).collect();
    assert_eq!(keys(&poetry, "repository"), sources);
    let core = find("poetry-schema.json");
    let mut dependency = BTreeSet::new();
    for definition in [
        "long-dependency",
        "git-dependency",
        "file-dependency",
        "path-dependency",
        "url-dependency",
        "dependency-options",
    ] {
        dependency.extend(keys(&core, definition));
    }
    let known: BTreeSet<String> = POETRY_DEPENDENCY_KEYS
        .iter()
        .map(|k| (*k).to_owned())
        .collect();
    assert_eq!(dependency, known);
}

/// `package`'s value at `keys` in a uv or Poetry lock.
fn locked(lock: &str, package: &str, keys: &[&str]) -> Option<String> {
    let doc: toml_edit::DocumentMut = lock.parse().ok()?;
    let entry = doc
        .get("package")?
        .as_array_of_tables()?
        .iter()
        .find(|p| p.get("name").and_then(toml_edit::Item::as_str) == Some(package))?;
    let mut item = entry.get(keys[0])?;
    for key in &keys[1..] {
        item = item.get(key)?;
    }
    item.as_str().map(str::to_owned)
}

/// Run a tool through the developer's mise store, with no user
/// configuration and every host but this one behind a dead proxy.
fn tool(dir: &Path, temp: &Path, specs: &[String], args: &[&str]) -> std::process::Output {
    let store = std::env::var_os("MISE_DATA_DIR").map_or_else(
        || PathBuf::from(std::env::var_os("HOME").unwrap()).join(".local/share/mise"),
        PathBuf::from,
    );
    let dead = "http://127.0.0.1:9";
    let mut command = Command::new("mise");
    command
        .arg("exec")
        .args(specs)
        .arg("--")
        .args(args)
        .current_dir(dir)
        .env("MISE_DATA_DIR", store)
        .env("MISE_AUTO_INSTALL", "false")
        .env("MISE_EXEC_AUTO_INSTALL", "false")
        .env("MISE_OFFLINE", "1")
        .env("XDG_CONFIG_HOME", temp.join("config"))
        .env("XDG_CACHE_HOME", temp.join("cache"))
        .env("UV_CACHE_DIR", temp.join("cache/uv"))
        .env("UV_PYTHON_DOWNLOADS", "never")
        .env("UV_HTTP_RETRIES", "0")
        .env("POETRY_CONFIG_DIR", temp.join("config/poetry"))
        .env("POETRY_CACHE_DIR", temp.join("cache/poetry"))
        .env("POETRY_REQUESTS_MAX_RETRIES", "0")
        .env("POETRY_VIRTUALENVS_CREATE", "false")
        .env("POETRY_KEYRING_ENABLED", "false")
        .env("PYTHON_KEYRING_BACKEND", "keyring.backends.null.Keyring")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost");
    for key in [
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "ALL_PROXY",
        "https_proxy",
        "http_proxy",
        "all_proxy",
    ] {
        command.env(key, dead);
    }
    for key in [
        "UV_INDEX",
        "UV_INDEX_URL",
        "UV_DEFAULT_INDEX",
        "UV_EXTRA_INDEX_URL",
        "UV_FIND_LINKS",
        "UV_OFFLINE",
    ] {
        command.env_remove(key);
    }
    // A tool that waits on something outside the test fails it, not hangs it.
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > std::time::Duration::from_secs(120) {
            let _ = child.kill();
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    child.wait_with_output().unwrap()
}

/// Serve every index under `/<index>/simple/`, returning the server's base.
fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || respond(stream));
        }
    });
    base
}

fn respond(mut stream: TcpStream) {
    let Ok(clone) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(clone);
    let mut request = String::new();
    if reader.read_line(&mut request).is_err() {
        return;
    }
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) <= 2 {
            break;
        }
    }
    let mut words = request.split_whitespace();
    let method = words.next().unwrap_or_default();
    let path = words
        .next()
        .unwrap_or_default()
        .split(['?', '#'])
        .next()
        .unwrap_or_default();
    let (status, kind, body) = match route(path) {
        Some((kind, body)) => ("200 OK", kind, body),
        None => ("404 Not Found", "text/plain", b"not found".to_vec()),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    if method != "HEAD" {
        let _ = stream.write_all(&body);
    }
}

/// A project page, a wheel, or a wheel's metadata, for any project name.
fn route(path: &str) -> Option<(&'static str, Vec<u8>)> {
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    match parts.as_slice() {
        [index, "simple", project] => {
            let (file, wheel, metadata) = wheel(project);
            let page = format!(
                "<!DOCTYPE html><html><body><a href=\"/{index}/files/{file}#sha256={}\" data-dist-info-metadata=\"sha256={metadata}\" data-core-metadata=\"sha256={metadata}\">{file}</a></body></html>",
                crate::plan::hash(&wheel),
                metadata = crate::plan::hash(&metadata),
            );
            Some(("text/html", page.into_bytes()))
        }
        [_, "files", file] => {
            let (name, want_metadata) = match file.strip_suffix(".metadata") {
                Some(name) => (name, true),
                None => (*file, false),
            };
            let project = name
                .strip_suffix("-1.0.0-py3-none-any.whl")?
                .replace('_', "-");
            let (_, wheel, metadata) = wheel(&project);
            Some(if want_metadata {
                ("application/octet-stream", metadata)
            } else {
                ("application/octet-stream", wheel)
            })
        }
        _ => None,
    }
}

/// A wheel's file name, its bytes, and its metadata.
fn wheel(project: &str) -> (String, Vec<u8>, Vec<u8>) {
    let dist = project.replace('-', "_");
    let info = format!("{dist}-1.0.0.dist-info");
    let metadata = format!("Metadata-Version: 2.1\nName: {project}\nVersion: 1.0.0\n").into_bytes();
    let wheel_info =
        b"Wheel-Version: 1.0\nGenerator: patchula-test\nRoot-Is-Purelib: true\nTag: py3-none-any\n"
            .to_vec();
    let record = format!("{info}/METADATA,,\n{info}/WHEEL,,\n{info}/RECORD,,\n").into_bytes();
    let bytes = zip(&[
        (format!("{info}/METADATA"), metadata.clone()),
        (format!("{info}/WHEEL"), wheel_info),
        (format!("{info}/RECORD"), record),
    ]);
    (format!("{dist}-1.0.0-py3-none-any.whl"), bytes, metadata)
}

/// An uncompressed zip archive.
fn zip(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    fn u16le(out: &mut Vec<u8>, value: usize) {
        out.extend_from_slice(&u16::try_from(value).unwrap().to_le_bytes());
    }
    fn u32le(out: &mut Vec<u8>, value: usize) {
        out.extend_from_slice(&u32::try_from(value).unwrap().to_le_bytes());
    }
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let crc = crc32(data) as usize;
        let offset = out.len();
        u32le(&mut out, 0x0403_4b50);
        for value in [20, 0, 0, 0, 0x21] {
            u16le(&mut out, value);
        }
        for value in [crc, data.len(), data.len()] {
            u32le(&mut out, value);
        }
        u16le(&mut out, name.len());
        u16le(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        u32le(&mut central, 0x0201_4b50);
        for value in [20, 20, 0, 0, 0, 0x21] {
            u16le(&mut central, value);
        }
        for value in [crc, data.len(), data.len()] {
            u32le(&mut central, value);
        }
        for value in [name.len(), 0, 0, 0, 0] {
            u16le(&mut central, value);
        }
        u32le(&mut central, 0);
        u32le(&mut central, offset);
        central.extend_from_slice(name.as_bytes());
    }
    let start = out.len();
    out.extend_from_slice(&central);
    u32le(&mut out, 0x0605_4b50);
    for value in [0, 0, entries.len(), entries.len()] {
        u16le(&mut out, value);
    }
    u32le(&mut out, central.len());
    u32le(&mut out, start);
    u16le(&mut out, 0);
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}
