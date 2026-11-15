//! `patchula-gui`: the Count's window. It shows what `patchula lookup` finds
//! (updates, held-back releases, dependencies, templates, warnings and the
//! run's numbers) and, in a working tree, writes the updates you pick through
//! `patchula apply`. Every lookup and every write is the CLI's own.
mod app;
mod cli;
mod demo;
mod model;
mod theme;
mod time;
mod view;

use app::{App, Lookup, Page, Review};
use cli::{Failure, Launch, Source};
use patchula_words::Update as UpdateKind;
use rune::{AppSpec, Chrome, ImageCache, TextEdit, Tx, Waker};

/// The Count, as the docs site draws him.
const LOGO: &[u8] = include_bytes!("../../docs/public/logo.png");

fn init() -> App {
    let mut launch = Launch::parse(rune::extra_args(), std::env::var(cli::PROGRAM_VAR).ok());
    // A headless run renders a picture and has no tree to look up, so it
    // draws the demo unless it was handed a saved report.
    if rune::tooling::headless_run().is_some()
        && let Ok(launch) = &mut launch
        && !matches!(launch.source, Source::File(_))
    {
        launch.source = Source::Demo;
    }
    App::new(launch)
}

fn setup(s: &mut App, waker: Waker) {
    let images = ImageCache::new(waker.clone());
    let logo = images.load(LOGO);
    // Decoded before the first frame, so the window never opens without him.
    let _ = logo.wait();
    s.logo = Some(logo);
    s.images = Some(images);
    s.set_waker(waker);
    if !s.is_demo() {
        s.refresh();
    }
}

fn poll(s: &mut App, _: f64, _: &mut Tx) -> bool {
    s.poll()
}

/// Pick the first update of each of a few kinds, as a person browsing might.
fn pick_some(s: &mut App) {
    for name in [
        "vite",
        "lodash",
        "serde",
        "rustls",
        "golang.org/x/net",
        "actions/setup-node",
    ] {
        if let Some(index) = s.data.updates.iter().position(|update| update.name == name) {
            s.toggle(index);
        }
    }
}

fn main() {
    rune::prewarm_gpu();
    let spec = AppSpec {
        min_size: Some((900.0, 560.0)),
        // Named states for headless pictures; each starts from the demo report.
        scenes: &[
            ("held", |s: &mut App| s.page = Page::Held),
            ("dependencies", |s: &mut App| s.page = Page::Dependencies),
            ("templates", |s: &mut App| s.page = Page::Templates),
            ("warnings", |s: &mut App| s.page = Page::Warnings),
            ("stats", |s: &mut App| s.page = Page::Stats),
            ("picked", |s: &mut App| {
                pick_some(s);
                let lodash = s
                    .data
                    .updates
                    .iter()
                    .find(|update| update.name == "lodash")
                    .map(|update| update.id.clone());
                if let Some(id) = lodash {
                    s.tables.updates.select(id);
                }
            }),
            ("major", |s: &mut App| s.set_filter(Some(UpdateKind::Major))),
            ("search", |s: &mut App| {
                s.search = TextEdit::new("react");
                s.arrange();
            }),
            ("review", |s: &mut App| {
                pick_some(s);
                s.plan();
            }),
            ("done", |s: &mut App| {
                pick_some(s);
                s.plan();
                s.write();
            }),
            ("keys", |s: &mut App| s.help = true),
            ("loading", |s: &mut App| {
                s.data = model::Data::default();
                s.lookup = Lookup::Running;
            }),
            ("uninvited", |s: &mut App| {
                s.lookup = Lookup::Failed(Failure {
                    message:
                        "no repository named, and /home/you/castle-web has no patchula.pkl to read"
                            .into(),
                    uninvited: true,
                });
            }),
            ("failed", |s: &mut App| {
                s.review = Review::Closed;
                s.lookup = Lookup::Failed(Failure {
                    message: "The Count is not at home: there is no `patchula` to run. Install patchula, or name the binary with --patchula or PATCHULA_BIN.".into(),
                    uninvited: false,
                });
            }),
        ],
        families: ("Inter", "ui-monospace, Inter"),
        setup: Some(setup),
        poll: Some(poll),
        on_key: Some(view::on_key),
        ..AppSpec::new("Count Patchula", (1280.0, 800.0), init, view::view)
    };
    // The traffic lights sit in the toolbar's band, 8 points down in the frame.
    let chrome = Chrome {
        full_size_content: true,
        traffic_lights: Some((16.0, 25.0)),
        drag_regions: true,
    };
    rune::App::new(spec).chrome(chrome).run();
}
