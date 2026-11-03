//! The index the Count gives each Python package, checked against the index
//! real uv, pip and Poetry take it from.
//!
//! A local server stands in for any number of package indexes, each serving
//! every project at 1.0.0. PyPI itself is out of reach behind a dead proxy,
//! so a tool that takes a package from PyPI fails. Each package is resolved
//! on its own, so one that only PyPI serves cannot hide another's index.
//!
//! The Count may be stricter than a tool (an ambiguous or unusable index
//! fails its lookup), but where it names an index it must be the tool's, and
//! where it names none the tool must not take the package from an index the
//! project configures.
use super::{config, default_spec};
use patchula_config::Manager;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Copy, Debug)]
enum Tool {
    Uv,
    Pip,
    Poetry,
}

struct Case {
    name: &'static str,
    tool: Tool,
    /// Files, with `{base}` for the server's address.
    files: &'static [(&'static str, &'static str)],
    /// The directory the tool runs in, and for pip the file it installs.
    run: &'static str,
    packages: &'static [&'static str],
}

const UV_PROJECT: &str = "[project]\nname = \"fixture\"\nversion = \"1.0.0\"\nrequires-python = \">=3.12\"\ndependencies = [\"fixture-a>=1\", \"fixture-b>=1\"]\n";

const CASES: &[Case] = &[
    Case {
        name: "uv without indexes",
        tool: Tool::Uv,
        files: &[("pyproject.toml", UV_PROJECT)],
        run: ".",
        packages: &["fixture-a"],
    },
    Case {
        name: "uv default index",
        tool: Tool::Uv,
        files: &[(
            "pyproject.toml",
            concat!(
                "[project]\nname = \"fixture\"\nversion = \"1.0.0\"\nrequires-python = \">=3.12\"\ndependencies = [\"fixture-a>=1\", \"fixture-b>=1\"]\n",
                "[[tool.uv.index]]\nname = \"corp\"\nurl = \"{base}/corp/simple\"\ndefault = true\n"
            ),
        )],
        run: ".",
        packages: &["fixture-a", "fixture-b"],
    },
    Case {
        name: "uv explicit index and a source pin",
        tool: Tool::Uv,
        files: &[(
            "pyproject.toml",
            concat!(
                "[project]\nname = \"fixture\"\nversion = \"1.0.0\"\nrequires-python = \">=3.12\"\ndependencies = [\"fixture-a>=1\", \"fixture-b>=1\"]\n",
                "[[tool.uv.index]]\nname = \"corp\"\nurl = \"{base}/corp/simple\"\nexplicit = true\n",
                "[tool.uv.sources]\nfixture-a = { index = \"corp\" }\n"
            ),
        )],
        run: ".",
        packages: &["fixture-a", "fixture-b"],
    },
    Case {
        name: "uv index-url",
        tool: Tool::Uv,
        files: &[(
            "pyproject.toml",
            concat!(
                "[project]\nname = \"fixture\"\nversion = \"1.0.0\"\nrequires-python = \">=3.12\"\ndependencies = [\"fixture-a>=1\", \"fixture-b>=1\"]\n",
                "[tool.uv]\nindex-url = \"{base}/corp/simple\"\n"
            ),
        )],
        run: ".",
        packages: &["fixture-a"],
    },
    Case {
        name: "uv.toml beside the project",
        tool: Tool::Uv,
        files: &[
            ("pyproject.toml", UV_PROJECT),
            ("uv.toml", "index-url = \"{base}/corp/simple\"\n"),
        ],
        run: ".",
        packages: &["fixture-a"],
    },
    Case {
        name: "uv.toml beside a project with a default index",
        tool: Tool::Uv,
        files: &[
            (
                "pyproject.toml",
                concat!(
                    "[project]\nname = \"fixture\"\nversion = \"1.0.0\"\nrequires-python = \">=3.12\"\ndependencies = [\"fixture-a>=1\", \"fixture-b>=1\"]\n",
                    "[[tool.uv.index]]\nname = \"corp\"\nurl = \"{base}/corp/simple\"\ndefault = true\n"
                ),
            ),
            ("uv.toml", "native-tls = false\n"),
        ],
        run: ".",
        packages: &["fixture-a"],
    },
    Case {
        name: "uv workspace member",
        tool: Tool::Uv,
        files: &[
            (
                "pyproject.toml",
                concat!(
                    "[project]\nname = \"root\"\nversion = \"1.0.0\"\nrequires-python = \">=3.12\"\ndependencies = []\n",
                    "[tool.uv.workspace]\nmembers = [\"./packages/*/\"]\n",
                    "[[tool.uv.index]]\nname = \"corp\"\nurl = \"{base}/corp/simple\"\ndefault = true\n"
                ),
            ),
            (
                "packages/app/pyproject.toml",
                concat!(
                    "[project]\nname = \"app\"\nversion = \"1.0.0\"\nrequires-python = \">=3.12\"\ndependencies = [\"fixture-a>=1\", \"fixture-b>=1\"]\n",
                    "[tool.uv]\nindex-url = \"{base}/other/simple\"\n"
                ),
            ),
        ],
        run: ".",
        packages: &["fixture-a"],
    },
    Case {
        name: "pip index-url",
        tool: Tool::Pip,
        files: &[(
            "requirements.txt",
            "--index-url {base}/corp/simple\nfixture-a==1.0.0\n",
        )],
        run: "requirements.txt",
        packages: &["fixture-a"],
    },
    Case {
        name: "pip options from the file that includes",
        tool: Tool::Pip,
        files: &[
            (
                "requirements-dev.txt",
                "-i{base}/corp/simple\n-r requirements.txt\nfixture-b==1.0.0\n",
            ),
            ("requirements.txt", "fixture-a==1.0.0\n"),
        ],
        run: "requirements-dev.txt",
        packages: &["fixture-a", "fixture-b"],
    },
    Case {
        name: "pip options from a quoted include",
        tool: Tool::Pip,
        files: &[
            ("requirements.txt", "-r \"base.txt\"\nfixture-a==1.0.0\n"),
            ("base.txt", "--index-url={base}/corp/simple\n"),
        ],
        run: "requirements.txt",
        packages: &["fixture-a"],
    },
    Case {
        name: "pip with a byte order mark",
        tool: Tool::Pip,
        files: &[(
            "requirements.txt",
            "\u{feff}--index-url {base}/corp/simple\nfixture-a==1.0.0\n",
        )],
        run: "requirements.txt",
        packages: &["fixture-a"],
    },
    Case {
        name: "pip without indexes",
        tool: Tool::Pip,
        files: &[("requirements.txt", "fixture-a==1.0.0\n")],
        run: "requirements.txt",
        packages: &["fixture-a"],
    },
    Case {
        name: "Poetry primary source",
        tool: Tool::Poetry,
        files: &[(
            "pyproject.toml",
            concat!(
                "[tool.poetry]\npackage-mode = false\n",
                "[tool.poetry.dependencies]\npython = \">=3.12\"\nfixture-a = \"^1.0\"\nfixture-b = \"^1.0\"\n",
                "[[tool.poetry.source]]\nname = \"corp\"\nurl = \"{base}/corp/simple\"\npriority = \"primary\"\n"
            ),
        )],
        run: ".",
        packages: &["fixture-a", "fixture-b"],
    },
    Case {
        name: "Poetry explicit source and a pin",
        tool: Tool::Poetry,
        files: &[(
            "pyproject.toml",
            concat!(
                "[tool.poetry]\npackage-mode = false\n",
                "[tool.poetry.dependencies]\npython = \">=3.12\"\nfixture-a = { version = \"^1.0\", source = \"Corp\" }\nfixture-b = \"^1.0\"\n",
                "[[tool.poetry.source]]\nname = \"corp\"\nurl = \"{base}/corp/simple\"\npriority = \"explicit\"\n"
            ),
        )],
        run: ".",
        packages: &["fixture-a", "fixture-b"],
    },
    Case {
        name: "Poetry supplemental source",
        tool: Tool::Poetry,
        files: &[(
            "pyproject.toml",
            concat!(
                "[tool.poetry]\npackage-mode = false\n",
                "[tool.poetry.dependencies]\npython = \">=3.12\"\nfixture-a = \"^1.0\"\nfixture-b = \"^1.0\"\n",
                "[[tool.poetry.source]]\nname = \"corp\"\nurl = \"{base}/corp/simple\"\npriority = \"supplemental\"\n"
            ),
        )],
        run: ".",
        packages: &["fixture-a"],
    },
];

#[test]
fn real_python_index_choices_match_uv_pip_and_poetry() {
    let base = serve();
    let temp = tempfile::tempdir().unwrap();
    let mut config = config();
    config.managers = vec![Manager::Requirements, Manager::Pyproject];
    let scanner = patchula_manager::Scanner::new(&config).unwrap();
    let mut mismatches = Vec::new();
    let mut compared = 0;
    for (number, case) in CASES.iter().enumerate() {
        for package in case.packages {
            let files: patchula_manager::Files = case
                .files
                .iter()
                .map(|(path, text)| ((*path).to_owned(), only(path, text, package, &base)))
                .collect();
            let Some(expected) = expected(&scanner, &files, package, case.name) else {
                continue;
            };
            let dir = temp.path().join(format!("{number}-{package}"));
            for (path, text) in &files {
                let path = dir.join(path);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, text).unwrap();
            }
            let (observed, log) = resolve(case, &dir, temp.path(), package, &base);
            compared += 1;
            if observed != expected {
                mismatches.push(format!(
                    "{} / {package}: the Count says {}, {:?} took it from {}\n{log}",
                    case.name,
                    expected.as_deref().unwrap_or("PyPI"),
                    case.tool,
                    observed.as_deref().unwrap_or("PyPI or nowhere"),
                ));
            }
        }
    }
    assert!(compared > 0, "every case was ambiguous");
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n\n"));
}

/// `text` with `{base}` filled in and every requirement but `package`'s
/// removed, so the tool resolves that package alone.
fn only(path: &str, text: &str, package: &str, base: &str) -> String {
    let text = text.replace("{base}", base);
    if path.ends_with(".txt") {
        return text
            .lines()
            .filter(|line| {
                let line = line.trim_start_matches('\u{feff}').trim();
                line.is_empty() || line.starts_with('-') || line.starts_with(package)
            })
            .map(|line| format!("{line}\n"))
            .collect();
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

/// The index the Count gives `package`: `Some(None)` for PyPI, `Some(url)`
/// for an index, and none when it is ambiguous or unusable, whose lookup
/// fails rather than guess.
fn expected(
    scanner: &patchula_manager::Scanner,
    files: &patchula_manager::Files,
    package: &str,
    case: &str,
) -> Option<Option<String>> {
    let extraction = scanner.extract(files).unwrap();
    let registries: Vec<Option<String>> = extraction
        .dependencies
        .iter()
        .filter(|d| d.package == package)
        .map(|d| d.registry.clone())
        .collect();
    let first = registries
        .first()
        .unwrap_or_else(|| panic!("{case}: {package} was not extracted"))
        .clone();
    assert!(
        registries.iter().all(|r| *r == first),
        "{case}: {package} has several registries {registries:?}"
    );
    match first {
        None => Some(None),
        Some(url) if url.starts_with("http") && !url.contains(' ') => Some(Some(url)),
        Some(_) => None,
    }
}

/// The local index the tool took `package` from, if any, and its output.
fn resolve(
    case: &Case,
    dir: &Path,
    temp: &Path,
    package: &str,
    base: &str,
) -> (Option<String>, String) {
    let index_of = |url: &str| -> Option<String> {
        let rest = url.strip_prefix(base)?.trim_start_matches('/');
        let index = rest.split('/').next()?;
        Some(format!("{base}/{index}/simple"))
    };
    let python = default_spec("python");
    let (output, observed) = match case.tool {
        Tool::Uv => {
            let output = tool(dir, temp, &[python, default_spec("uv")], &["uv", "lock"]);
            let observed = std::fs::read_to_string(dir.join("uv.lock"))
                .ok()
                .and_then(|lock| locked(&lock, package, &["source", "registry"]));
            (output, observed)
        }
        Tool::Poetry => {
            let output = tool(
                dir,
                temp,
                &[python, default_spec("poetry")],
                &["poetry", "lock"],
            );
            let observed = std::fs::read_to_string(dir.join("poetry.lock"))
                .ok()
                .and_then(|lock| locked(&lock, package, &["source", "url"]));
            (output, observed)
        }
        Tool::Pip => {
            let report = dir.join("report.json");
            let output = tool(
                dir,
                temp,
                &[python],
                &[
                    "python",
                    "-m",
                    "pip",
                    "install",
                    "--dry-run",
                    "--ignore-installed",
                    "--quiet",
                    "--report",
                    report.to_str().unwrap(),
                    "-r",
                    case.run,
                ],
            );
            let observed = std::fs::read_to_string(&report)
                .ok()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .and_then(|report| {
                    report["install"].as_array()?.iter().find_map(|item| {
                        (item["metadata"]["name"].as_str() == Some(package))
                            .then(|| item["download_info"]["url"].as_str().map(str::to_owned))
                            .flatten()
                    })
                });
            (output, observed)
        }
    };
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (observed.as_deref().and_then(index_of), log)
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
        .env("PIP_CONFIG_FILE", "/dev/null")
        .env("PIP_NO_CACHE_DIR", "1")
        .env("PIP_DISABLE_PIP_VERSION_CHECK", "1")
        .env("PIP_RETRIES", "0")
        .env("PIP_TIMEOUT", "5")
        .env("POETRY_CONFIG_DIR", temp.join("config/poetry"))
        .env("POETRY_CACHE_DIR", temp.join("cache/poetry"))
        .env("POETRY_REQUESTS_MAX_RETRIES", "0")
        .env("POETRY_VIRTUALENVS_CREATE", "false")
        .env("POETRY_KEYRING_ENABLED", "false")
        .env("PYTHON_KEYRING_BACKEND", "keyring.backends.null.Keyring")
        .env("PIP_NO_INPUT", "1")
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
        "PIP_INDEX_URL",
        "PIP_EXTRA_INDEX_URL",
        "PIP_FIND_LINKS",
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
