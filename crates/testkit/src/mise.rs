//! Replays and records `mise` runs. A subprocess has no HTTP cassette, so these
//! fixtures hold what mise printed for each lookup command.
use crate::{Redactor, failure};
use patchula_http::Error;
use patchula_process::{CommandSpec, Executor};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    future::Future,
    io::Write,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
};

/// What one lookup command printed, or how it failed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    /// The arguments after `mise`, for example `ls-remote --json -- jq`.
    pub args: Vec<String>,
    #[serde(flatten)]
    pub result: Outcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Standard output, which is JSON.
    Output(serde_json::Value),
    /// The executor's error: the exit status and what mise printed.
    Error(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MiseFixture {
    pub schema_version: u32,
    pub provenance: String,
    pub mise_version: String,
    pub runs: Vec<Run>,
}

/// One command a replayed lookup issued, with the environment it ran in.
#[derive(Clone, Debug)]
pub struct Call {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
    pub timeout: std::time::Duration,
}
impl Call {
    /// The arguments after the pinned `mise`, when this is a lookup command.
    pub fn lookup(&self) -> Option<&[String]> {
        let at = self.args.iter().position(|a| a == "--")?;
        (self.args.get(at + 1).map(String::as_str) == Some("mise")).then(|| &self.args[at + 2..])
    }
}

pub fn read(path: &Path, redactor: &Redactor) -> Result<MiseFixture, Error> {
    let bytes = std::fs::read(path).map_err(|_| {
        failure(format!(
            "cannot read mise fixture {} (record with mise run fixtures:record)",
            path.display()
        ))
    })?;
    redactor.check(&bytes)?;
    parse(&bytes)
}
pub(crate) fn parse(bytes: &[u8]) -> Result<MiseFixture, Error> {
    let fixture: MiseFixture =
        serde_json::from_slice(bytes).map_err(|_| failure("invalid mise fixture JSON"))?;
    if fixture.schema_version != 1 {
        return Err(failure("unsupported mise fixture schema version"));
    }
    Ok(fixture)
}

/// Answers `mise install` with success and each lookup from the fixture. A
/// lookup the fixture lacks fails the test: replay never runs mise.
pub struct MiseReplay {
    runs: Vec<Run>,
    calls: Mutex<Vec<Call>>,
}
impl MiseReplay {
    pub fn open(path: &Path) -> Result<Self, Error> {
        Ok(Self::new(read(path, &Redactor::default())?.runs))
    }
    /// A replay of runs written in the test, for listings no recording has.
    pub fn new(runs: Vec<Run>) -> Self {
        Self {
            runs,
            calls: Mutex::default(),
        }
    }
    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().expect("calls lock").clone()
    }
    /// The lookup commands issued so far, as mise would receive them.
    pub fn lookups(&self) -> Vec<Vec<String>> {
        self.calls()
            .iter()
            .filter_map(|call| call.lookup().map(<[String]>::to_vec))
            .collect()
    }
}
fn snapshot(spec: &CommandSpec) -> Call {
    Call {
        program: spec.program.clone(),
        args: spec.args.clone(),
        env: spec
            .env
            .iter()
            .map(|(k, v)| (k.to_string_lossy().into(), v.to_string_lossy().into()))
            .collect(),
        cwd: spec.cwd.clone(),
        timeout: spec.timeout,
    }
}
impl Executor for MiseReplay {
    fn run(
        &self,
        spec: CommandSpec,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + '_>> {
        Box::pin(async move {
            let call = snapshot(&spec);
            self.calls.lock().expect("calls lock").push(call.clone());
            let Some(args) = call.lookup() else {
                return if call.args.iter().any(|a| a == "install") {
                    Ok(Vec::new())
                } else {
                    Err(format!("unexpected mise command {:?}", call.args))
                };
            };
            match self.runs.iter().find(|run| run.args == args) {
                Some(Run {
                    result: Outcome::Output(value),
                    ..
                }) => Ok(serde_json::to_vec(value).expect("JSON value serializes")),
                Some(Run {
                    result: Outcome::Error(message),
                    ..
                }) => Err(message.clone()),
                None => Err(format!(
                    "mise fixture has no run for `mise {}`; replay never runs mise",
                    args.join(" ")
                )),
            }
        })
    }
}

/// Runs the real mise and writes what each lookup printed, redacted and
/// checked for secrets, to `path`.
pub struct MiseRecorder {
    path: PathBuf,
    mise: Arc<dyn Executor>,
    fixture: Mutex<MiseFixture>,
}
impl MiseRecorder {
    pub fn new(path: &Path, mise: Arc<dyn Executor>, mise_version: &str, provenance: &str) -> Self {
        Self {
            path: path.to_owned(),
            mise,
            fixture: Mutex::new(MiseFixture {
                schema_version: 1,
                provenance: provenance.into(),
                mise_version: mise_version.into(),
                runs: Vec::new(),
            }),
        }
    }
}
impl Executor for MiseRecorder {
    fn run(
        &self,
        spec: CommandSpec,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + '_>> {
        Box::pin(async move {
            let args = snapshot(&spec).lookup().map(<[String]>::to_vec);
            let result = self.mise.run(spec).await;
            let Some(args) = args else {
                return result;
            };
            let outcome = match &result {
                Ok(bytes) => Outcome::Output(
                    serde_json::from_slice(bytes).map_err(|e| format!("not JSON: {e}"))?,
                ),
                Err(message) => Outcome::Error(message.clone()),
            };
            let mut fixture = self.fixture.lock().expect("fixture lock");
            fixture.runs.retain(|run| run.args != args);
            fixture.runs.push(Run {
                args,
                result: outcome,
            });
            let bytes = serde_json::to_vec_pretty(&*fixture).map_err(|e| e.to_string())?;
            Redactor::default()
                .check(&bytes)
                .map_err(|e| e.to_string())?;
            // A temporary file in the same directory, renamed over the fixture, so a
            // stopped recording never leaves half a file.
            let parent = self
                .path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
            file.write_all(&bytes)
                .and_then(|()| file.write_all(b"\n"))
                .map_err(|e| e.to_string())?;
            file.persist(&self.path).map_err(|e| e.to_string())?;
            result
        })
    }
}
