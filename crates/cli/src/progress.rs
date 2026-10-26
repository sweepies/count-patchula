//! A spinner on stderr while a batch runs, so a long lookup says what it is
//! doing. Only on a terminal: pipes and logs never see it.
use patchula_engine::{Progress, Stage};
use std::sync::{Arc, Mutex};

use clx::progress::{ProgressJob, ProgressJobBuilder, ProgressJobDoneBehavior};

pub struct Spinner {
    job: Arc<ProgressJob>,
    state: Mutex<(Stage, usize, usize)>,
}

impl Spinner {
    /// A spinner when stderr is a terminal that can draw one.
    pub fn start() -> Option<Arc<Self>> {
        let terminal = console::Term::stderr().is_term()
            && std::env::var_os("TERM").is_none_or(|term| term != "dumb");
        if !terminal {
            return None;
        }
        let job = ProgressJobBuilder::new()
            .body("{{ spinner() }} {{ message | flex }}")
            .prop("message", "the Count is getting ready…")
            .on_done(ProgressJobDoneBehavior::Hide)
            .start();
        Some(Arc::new(Self {
            job,
            state: Mutex::new((Stage::Discovery, 0, 0)),
        }))
    }

    fn show(&self, (stage, total, done): (Stage, usize, usize)) {
        let message = match stage {
            Stage::Discovery => "finding the repositories the token can reach…".to_owned(),
            Stage::Extraction if total == 1 => "reading the manifests…".to_owned(),
            Stage::Extraction => format!("reading {total} repositories…"),
            Stage::Lookup => format!(
                "looking up {}…",
                crate::output::count(total, "dependency", "dependencies")
            ),
            Stage::Digests => format!(
                "resolving {}…",
                crate::output::count(total, "digest", "digests")
            ),
            Stage::Lockfiles => format!(
                "regenerating lockfiles for {}…",
                crate::output::count(total, "branch", "branches")
            ),
            Stage::Writing => format!(
                "writing {} and their pull requests…",
                crate::output::count(total, "branch", "branches")
            ),
        };
        let counted = total > 0
            && !matches!(stage, Stage::Writing)
            && !(stage == Stage::Extraction && total == 1);
        self.job.prop(
            "message",
            &if counted {
                format!("{message} {}/{total}", done.min(total))
            } else {
                message
            },
        );
        if counted {
            self.job.progress_total(total);
            self.job.progress_current(done.min(total));
        }
        self.job.update();
    }
}

/// The spinner is cleared once the batch drops it, before the report is
/// printed, whether the batch finished or failed.
impl Drop for Spinner {
    fn drop(&mut self) {
        self.job.set_status(clx::progress::ProgressStatus::Done);
        clx::progress::stop_clear();
    }
}

impl Progress for Spinner {
    fn stage(&self, stage: Stage, total: usize) {
        let state = (stage, total, 0);
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = state;
        self.show(state);
    }

    fn advance(&self, done: usize) {
        let state = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.2 += done;
            *state
        };
        self.show(state);
    }
}
