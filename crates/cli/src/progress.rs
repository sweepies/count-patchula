//! A spinner on stderr while a batch runs, so a long lookup says what it is
//! doing. Only on a terminal: pipes and logs never see it.
use crate::{cli, output, palette};
use patchula_engine::{Progress, Stage};
use std::sync::{Arc, Mutex};

use clx::progress::{ProgressJob, ProgressJobBuilder, ProgressJobDoneBehavior};

pub struct Spinner {
    job: Arc<ProgressJob>,
    state: Mutex<(Stage, usize, usize)>,
}

impl Spinner {
    /// A spinner when stderr is a terminal that can draw one: a bold braille
    /// wheel in the accent, then the current step.
    pub fn start(color: cli::Color) -> Option<Arc<Self>> {
        Self::start_with(color, "Preparing…")
    }

    /// A spinner starting with a custom message.
    pub fn start_with(color: cli::Color, initial: &str) -> Option<Arc<Self>> {
        let terminal = console::Term::stderr().is_term()
            && std::env::var_os("TERM").is_none_or(|term| term != "dumb");
        if !terminal {
            return None;
        }
        // clx paints its spinner blue when stderr is coloured; the frame is
        // repainted in the accent, or left plain when we may not colour.
        let wheel = if output::colored(color, true) {
            format!("\x1b[1;{}m", palette::ACCENT.sgr(palette::depth()))
        } else {
            String::new()
        };
        let job = ProgressJobBuilder::new()
            .body(format!(
                "{{{{ spinner(name=\"dot\") | replace(from=\"\x1b[34m\", to=\"{wheel}\") }}}} {{{{ message | flex }}}}"
            ))
            .prop("message", initial)
            .on_done(ProgressJobDoneBehavior::Hide)
            .start();
        Some(Arc::new(Self {
            job,
            state: Mutex::new((Stage::Discovery, 0, 0)),
        }))
    }

    fn show(&self, (stage, total, done): (Stage, usize, usize)) {
        let counted = total > 0
            && !matches!(stage, Stage::Writing)
            && !(stage == Stage::Extraction && total == 1);
        let done = done.min(total);
        let message = match stage {
            Stage::Discovery => "Discovering repositories…".to_owned(),
            Stage::Extraction if total == 1 => "Reading manifests…".to_owned(),
            Stage::Extraction if counted => format!("Reading repositories ({done}/{total})…"),
            Stage::Extraction => "Reading repositories…".to_owned(),
            Stage::Lookup if counted => format!("Running lookups ({done}/{total})…"),
            Stage::Lookup => "Running lookups…".to_owned(),
            Stage::Digests if counted => format!("Resolving digests ({done}/{total})…"),
            Stage::Digests => "Resolving digests…".to_owned(),
            Stage::Lockfiles if counted => format!("Updating lockfiles ({done}/{total})…"),
            Stage::Lockfiles => "Updating lockfiles…".to_owned(),
            Stage::Writing => format!(
                "Writing {} and pull requests…",
                crate::output::count(total, "branch", "branches")
            ),
        };
        self.job.prop("message", &message);
        if counted {
            self.job.progress_total(total);
            self.job.progress_current(done);
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
