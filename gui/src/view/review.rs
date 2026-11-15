//! Reviewing the picked updates: the diff `patchula apply --dry-run` wrote
//! out, the button that applies it, and what the Count says he did.
use super::parts;
use crate::{
    app::{App, Outcome, Plan, Review},
    cli::Written,
};
use rune::{El, ScrollSpec, Tx, col, el, icon, row, text};
use rune_kit::{
    Button, ButtonVariant, ControlSize, Kit, SegmentedControl, Spinner,
    diff::{DiffMode, DiffView},
};

pub fn page(s: &App, k: &Kit) -> El {
    match &s.review {
        Review::Closed => el(),
        Review::Planning => parts::blank(
            Spinner::new().size(28.0).build(k),
            "The Count drafts the changes",
            "He looks the tree up once more and writes out exactly what would change. Nothing is written yet.",
            k,
        ),
        Review::Ready(plan) => ready(s, plan, false, k),
        Review::Writing(plan) => ready(s, plan, true, k),
        Review::Done(outcome) => done(s, outcome, k),
        Review::Failed(message) => col()
            .w_full()
            .h_full()
            .child(
                parts::blank(
                    parts::disc(&parts::WARNINGS, k.c.danger.icon, k),
                    "The Count could not finish",
                    message.clone(),
                    k,
                )
                .grow(1.0)
                .min_h(0.0),
            )
            .child(
                footer(k)
                    .child(el().grow(1.0))
                    .child(back("Back to the updates", k)),
            ),
    }
}

fn back(label: &str, k: &Kit) -> El {
    Button::new(label)
        .variant(ButtonVariant::Neutral)
        .control_size(ControlSize::Large)
        .shortcut("Esc")
        .on_press(|s: &mut App| s.close_review())
        .build(k)
        .key("review-back")
}

fn footer(k: &Kit) -> El {
    row()
        .key("review-footer")
        .w_full()
        .h(56.0)
        .px(16.0)
        .gap(12.0)
        .center_y()
        .fill(k.c.layer_01)
        .border_sides([1.0, 0.0, 0.0, 0.0], k.c.border_muted)
}

/// A line for one update: its type, its name, where it moves, its file.
fn line(update: &Written, k: &Kit) -> El {
    let target = update
        .value
        .clone()
        .or_else(|| update.version.clone())
        .or_else(|| update.digest.clone())
        .unwrap_or_default();
    row()
        .w_full()
        .gap(10.0)
        .center_y()
        .child(parts::kind(update.kind, k))
        .child(
            text(update.name.clone())
                .font(k.ty.ui_medium)
                .color(k.c.text_base)
                .truncate()
                .shrink()
                .min_w(0.0),
        )
        .child(
            text(format!("{} → {target}", update.current))
                .font(k.ty.mono)
                .color(k.c.text_muted)
                .truncate()
                .shrink()
                .min_w(0.0),
        )
        .child(el().grow(1.0))
        .child(
            text(update.file.clone())
                .font(k.ty.text_small)
                .color(k.c.text_faint)
                .truncate_middle()
                .shrink()
                .min_w(0.0),
        )
}

/// A tinted note above the diff.
fn note(hue: rune::Color, title: String, lines: Vec<String>, k: &Kit) -> El {
    let mut body = col()
        .grow(1.0)
        .min_w(0.0)
        .gap(2.0)
        .child(text(title).font(k.ty.ui_medium).color(k.c.text_base).wrap());
    for words in lines {
        body = body.child(
            text(words)
                .font(k.ty.text_small)
                .color(k.c.text_muted)
                .wrap(),
        );
    }
    row()
        .w_full()
        .gap(10.0)
        .px(16.0)
        .py(10.0)
        .items_start()
        .fill(hue.alpha(if k.dark { 0.12 } else { 0.08 }))
        .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_muted)
        .child(icon(&parts::WARNINGS, 16.0).color(hue).mt(1.0))
        .child(body)
}

fn ready(s: &App, plan: &Plan, writing: bool, k: &Kit) -> El {
    let applied = &plan.applied;
    let (adds, dels) = s.diff.files().iter().fold((0, 0), |(a, d), file| {
        (a + file.additions(), d + file.deletions())
    });
    let split = s.diff.mode() == DiffMode::Split;
    let modes = SegmentedControl::new(&["Unified", "Split"], Some(usize::from(split)))
        .label("Diff layout")
        .width(Some(160.0))
        .on_pick(|s: &mut App, i, tx: &mut Tx| {
            tx.motion(k_motion());
            s.diff.set_mode(if i == 1 {
                DiffMode::Split
            } else {
                DiffMode::Unified
            });
        })
        .build(k);
    let head = row()
        .key("review-head")
        .w_full()
        .h(48.0)
        .px(16.0)
        .gap(12.0)
        .center_y()
        .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_muted)
        .child(
            text(format!(
                "Review {}",
                parts::count(applied.updates.len(), "update", "updates")
            ))
            .font(k.ty.ui_medium)
            .color(k.c.text_base),
        )
        .child(
            text(format!(
                "{}  +{adds} −{dels}",
                parts::count(s.diff.files().len(), "file", "files")
            ))
            .font(k.ty.ui)
            .color(k.c.text_muted),
        )
        .child(el().grow(1.0))
        .child(modes);

    let mut page = col().key("review").w_full().h_full().child(head);
    if !plan.extras.is_empty() {
        page = page.child(note(
            k.c.warning.icon,
            format!(
                "{} you did not pick would be written too",
                parts::count(plan.extras.len(), "update", "updates")
            ),
            std::iter::once(
                "The Count picks by manager, name and type, and these share all three with a pick:"
                    .to_owned(),
            )
            .chain(
                plan.extras
                    .iter()
                    .map(|extra| format!("{} in {}", extra.name, extra.file)),
            )
            .collect(),
            k,
        ));
    }
    if !applied.skipped.is_empty() {
        page = page.child(note(
            k.c.warning.icon,
            format!(
                "{} cannot be written in place and will be left alone",
                parts::count(applied.skipped.len(), "update", "updates")
            ),
            applied
                .skipped
                .iter()
                .map(|skipped| {
                    format!(
                        "{} in {}: {}",
                        skipped.name,
                        skipped.file,
                        skipped.reason.as_deref().unwrap_or("no reason given")
                    )
                })
                .collect(),
            k,
        ));
    }
    page = if applied.files.is_empty() {
        page.child(
            parts::blank(
                parts::disc(&parts::DONE, k.c.icon_muted, k),
                "Nothing would change",
                "None of the picked updates can be written into the files in place.",
                k,
            )
            .grow(1.0)
            .min_h(0.0),
        )
    } else {
        page.child(
            DiffView::new(|s: &App| &s.diff, |s: &mut App| &mut s.diff)
                .key("review-diff")
                .build(s, k)
                .grow(1.0)
                .min_h(0.0),
        )
    };

    let mut bar = footer(k).child(
        text(if s.is_demo() {
            "This is the demo: applying writes nothing."
        } else {
            "This is exactly what will be written. Lockfiles are not regenerated."
        })
        .font(k.ty.ui)
        .color(k.c.text_muted)
        .truncate()
        .shrink()
        .min_w(0.0),
    );
    bar = bar.child(el().grow(1.0));
    if writing {
        bar = bar
            .child(Spinner::new().size(16.0).build(k))
            .child(text("Writing…").font(k.ty.ui_medium).color(k.c.text_base));
    } else {
        bar = bar.child(back("Back", k)).child(
            Button::new(format!(
                "Apply {}",
                parts::count(applied.updates.len(), "update", "updates")
            ))
            .variant(ButtonVariant::Contrast)
            .control_size(ControlSize::Large)
            .shortcut("⌘↩")
            .disabled(applied.updates.is_empty())
            .on_press(|s: &mut App| s.write())
            .build(k)
            .key("review-apply"),
        );
    }
    page.child(bar)
}

/// The spring a layout switch in the diff moves on.
fn k_motion() -> rune::Spring {
    rune::tok::motion::SNAPPY
}

fn done(s: &App, outcome: &Outcome, k: &Kit) -> El {
    let applied = &outcome.applied;
    let files = applied.files.len();
    let headline = if applied.updates.is_empty() {
        format!(
            "The Count could not write {} in place.",
            parts::count(applied.skipped.len(), "update", "updates")
        )
    } else {
        format!(
            "The Count applied {} to {}.",
            parts::count(applied.updates.len(), "update", "updates"),
            parts::count(files, "file", "files")
        )
    };
    let mut list = col().w_full().max_w(720.0).gap(16.0).child(
        col()
            .gap(10.0)
            .items_center()
            .w_full()
            .child(parts::disc(&parts::DONE, k.c.success.icon, k))
            .child(
                text(headline)
                    .key("done-headline")
                    .font(k.ty.body_medium)
                    .color(k.c.text_base)
                    .wrap(),
            ),
    );
    if !applied.updates.is_empty() {
        let mut card = parts::card(k).padding(14.0).gap(10.0);
        for update in &applied.updates {
            card = card.child(line(update, k));
        }
        list = list.child(card);
    }
    if !applied.skipped.is_empty() {
        let mut card = parts::card(k)
            .padding(14.0)
            .gap(10.0)
            .child(parts::heading("Left alone", k));
        for skipped in &applied.skipped {
            card = card.child(line(skipped, k));
            if let Some(reason) = &skipped.reason {
                card = card.child(
                    text(reason.clone())
                        .font(k.ty.text_small)
                        .color(k.c.text_muted)
                        .wrap(),
                );
            }
        }
        list = list.child(card);
    }
    let mut notes = Vec::new();
    if s.is_demo() {
        notes.push("This is the demo, so no file changed.".to_owned());
    }
    if outcome.locked {
        notes.push(
            "Lockfiles are not regenerated, so refresh them with your package manager.".to_owned(),
        );
    }
    notes.extend(applied.warnings.iter().cloned());
    for words in notes {
        list = list.child(text(words).font(k.ty.ui).color(k.c.text_muted).wrap());
    }
    col()
        .key("review-done")
        .w_full()
        .h_full()
        .child(
            col()
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .child(col().w_full().items_center().padding(32.0).child(list)),
        )
        .child(
            footer(k)
                .child(el().grow(1.0))
                .child(
                    Button::new("Look again")
                        .variant(ButtonVariant::Ghost)
                        .control_size(ControlSize::Large)
                        .shortcut("⌘R")
                        .on_press(|s: &mut App| s.refresh())
                        .build(k)
                        .key("done-refresh"),
                )
                .child(back("Back to the updates", k)),
        )
}
