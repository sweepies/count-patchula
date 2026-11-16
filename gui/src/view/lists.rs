//! The pages that only show: held-back releases, every dependency found,
//! template updates and warnings.
use super::parts;
use crate::{
    app::{App, Page},
    model::{Held, Tone},
    theme, time,
};
use rune::{Align, El, ScrollSpec, Tx, col, el, row, text};
use rune_kit::{Button, ButtonVariant, Cell, ControlSize, Kit, Table, TableColumn};
use std::borrow::Cow;

/// A page that scrolls its content in a column of at most `width` points.
fn scrolling(key: &str, width: f32, content: El) -> El {
    col()
        .key(key.to_owned())
        .w_full()
        .h_full()
        .min_h(0.0)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .child(
            col()
                .w_full()
                .items_center()
                .padding(24.0)
                .child(content.w_full().max_w(width)),
        )
}

/// How far through its wait a held release is, and what is left of it.
fn wait(held: &Held, now: i64) -> (f32, String) {
    match (
        time::parse(&held.published_at),
        time::parse(&held.eligible_at),
    ) {
        (Some(published), Some(eligible)) if eligible > published => {
            let done = (now - published) as f32 / (eligible - published) as f32;
            let left = if now >= eligible {
                "ready".to_owned()
            } else {
                format!("in {}", time::span(eligible - now))
            };
            (done.clamp(0.0, 1.0), left)
        }
        _ => (0.0, String::new()),
    }
}

pub fn held(s: &App, k: &Kit) -> El {
    if s.rows.held.is_empty() {
        let (title, detail) = if s.data.held.is_empty() {
            (
                "Nothing is waiting",
                "No release is inside its release-age wait.",
            )
        } else {
            (
                "No held release matches",
                "Nothing held back fits the search.",
            )
        };
        return parts::blank(
            parts::disc(&parts::HELD, k.c.icon_muted, k),
            title,
            detail,
            k,
        );
    }
    let kit = k.clone();
    Table::new(
        |s: &App| &s.tables.held,
        |s: &mut App| &mut s.tables.held,
        |s: &App| s.rows.held.len(),
        |s: &App, i| Cow::Borrowed(s.data.held[s.rows.held[i]].id.as_str()),
    )
    .name("Held back")
    .column(
        TableColumn::new("name", "Dependency")
            .flex(2.0)
            .min(170.0)
            .sortable(),
    )
    .column(
        TableColumn::new("current", "Current")
            .width(112.0)
            .min(70.0)
            .max(260.0),
    )
    .column(
        TableColumn::new("version", "Waiting")
            .width(132.0)
            .min(80.0)
            .max(300.0)
            .sortable(),
    )
    .column(
        TableColumn::new("type", "Type")
            .width(92.0)
            .resizable(false)
            .sortable(),
    )
    .column(
        TableColumn::new("eligible", "Eligible")
            .width(196.0)
            .min(150.0)
            .max(320.0)
            .sortable(),
    )
    .column(
        TableColumn::new("file", "File")
            .flex(1.3)
            .min(110.0)
            .sortable(),
    )
    .cell(move |s: &App, i, column| {
        let k = &kit;
        let held = &s.data.held[s.rows.held[i]];
        match column {
            "name" => Cell::text(&held.name),
            "current" => Cell::text(&held.current).mono().muted(),
            "version" if held.count > 1 => Cell::el(
                row()
                    .gap(6.0)
                    .center_y()
                    .min_w(0.0)
                    .child(
                        text(held.version.clone())
                            .font(k.ty.mono)
                            .color(k.c.text_base)
                            .truncate()
                            .shrink()
                            .min_w(0.0),
                    )
                    .child(
                        text(format!("+{}", held.count - 1))
                            .font(k.ty.text_small)
                            .color(k.c.text_faint)
                            .help(format!("{} releases wait behind this one", held.count - 1)),
                    ),
            ),
            "version" => Cell::text(&held.version).mono(),
            "type" => Cell::el(parts::kind(held.kind, k)),
            "eligible" => {
                let (done, left) = wait(held, s.now);
                let hue = theme::brand(k.dark).gold;
                // A meter: the track is a lighter step of the fill's own hue.
                let meter = el()
                    .w(64.0)
                    .h(4.0)
                    .radius(2.0)
                    .fill(hue.alpha(0.22))
                    .child(el().w(64.0 * done).h(4.0).radius(2.0).fill(hue));
                Cell::el(
                    row()
                        .gap(8.0)
                        .center_y()
                        .min_w(0.0)
                        .help(format!(
                            "Published {}, eligible {}",
                            time::date(&held.published_at),
                            time::date(&held.eligible_at)
                        ))
                        .child(meter)
                        .child(text(left).font(k.ty.text_small).color(k.c.text_base))
                        .child(
                            text(time::date(&held.eligible_at).to_owned())
                                .font(k.ty.text_small)
                                .color(k.c.text_faint)
                                .truncate()
                                .shrink()
                                .min_w(0.0),
                        ),
                )
            }
            _ => Cell::text(&held.file).muted(),
        }
    })
    .on_sort(|s: &mut App, column, dir, _| s.sort(Page::Held, column, dir))
    .build(s, k)
    .grow(1.0)
    .min_h(0.0)
}

pub fn dependencies(s: &App, k: &Kit) -> El {
    if s.rows.found.is_empty() {
        let (title, detail) = if s.data.found.is_empty() {
            (
                "No dependencies found",
                "The Count read no manifest he knows in this tree.",
            )
        } else {
            ("No dependency matches", "Nothing he found fits the search.")
        };
        return parts::blank(
            parts::disc(&parts::FOUND, k.c.icon_muted, k),
            title,
            detail,
            k,
        );
    }
    let kit = k.clone();
    Table::new(
        |s: &App| &s.tables.found,
        |s: &mut App| &mut s.tables.found,
        |s: &App| s.rows.found.len(),
        |s: &App, i| Cow::Borrowed(s.data.found[s.rows.found[i]].id.as_str()),
    )
    .name("Dependencies")
    .column(
        TableColumn::new("name", "Dependency")
            .flex(2.0)
            .min(170.0)
            .sortable(),
    )
    .column(
        TableColumn::new("manager", "Manager")
            .width(132.0)
            .min(90.0)
            .max(220.0)
            .sortable(),
    )
    .column(
        TableColumn::new("current", "Current")
            .width(124.0)
            .min(70.0)
            .max(300.0)
            .sortable(),
    )
    .column(
        TableColumn::new("updates", "Updates")
            .width(190.0)
            .min(110.0)
            .max(280.0)
            .sortable(),
    )
    .column(
        TableColumn::new("file", "File")
            .flex(1.4)
            .min(110.0)
            .sortable(),
    )
    .cell(move |s: &App, i, column| {
        let k = &kit;
        let found = &s.data.found[s.rows.found[i]];
        match column {
            "name" => Cell::text(&found.name),
            "manager" => Cell::el(parts::tag(found.manager.as_str(), k)),
            "current" => Cell::text(&found.current).mono(),
            "updates" => {
                // Counts in ink: the colours on this page belong to the types.
                let words = match (found.updates, found.held) {
                    (0, 0) => None,
                    (updates, 0) => Some(parts::count(updates, "update", "updates")),
                    (0, held) => Some(format!("{held} held back")),
                    (updates, held) => Some(format!(
                        "{}, {held} held back",
                        parts::count(updates, "update", "updates")
                    )),
                };
                match words {
                    Some(words) => Cell::text(words),
                    None => Cell::text("up to date").color(k.c.text_faint),
                }
            }
            _ => Cell::text(&found.file).muted(),
        }
    })
    .on_sort(|s: &mut App, column, dir, _| s.sort(Page::Dependencies, column, dir))
    .zebra(true)
    .build(s, k)
    .grow(1.0)
    .min_h(0.0)
}

pub fn templates(s: &App, k: &Kit) -> El {
    if s.data.templates.is_empty() {
        return parts::blank(
            parts::disc(&parts::TEMPLATES, k.c.icon_muted, k),
            "No template updates",
            "No template this tree was made from has a newer release.",
            k,
        );
    }
    let mut list = col().gap(12.0).child(
        text("The Count reports template updates and never edits them. Copier applies one; run it from a terminal, where it can ask its questions.")
            .font(k.ty.ui)
            .color(k.c.text_muted)
            .wrap(),
    );
    for (i, template) in s.data.templates.iter().enumerate() {
        let mut buttons = row().gap(8.0).child(
            Button::new("Copy the copier command")
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Regular)
                .icon(k.glyph.copy)
                .on_press_tx(|_: &mut App, tx: &mut Tx| tx.copy("patchula apply --copier"))
                .build(k)
                .key(format!("template-copy-{i}")),
        );
        if let Some(url) = template.compare_url.clone() {
            buttons = buttons.child(
                Button::new("Compare the releases")
                    .variant(ButtonVariant::Ghost)
                    .control_size(ControlSize::Regular)
                    .on_press_tx(move |_: &mut App, tx: &mut Tx| {
                        let _ = tx.open_url(&url);
                    })
                    .build(k)
                    .key(format!("template-compare-{i}")),
            );
        }
        list = list.child(
            parts::card(k)
                .key(format!("template-{i}"))
                .padding(16.0)
                .gap(10.0)
                .child(
                    row()
                        .w_full()
                        .gap(8.0)
                        .center_y()
                        .child(
                            text(template.name.clone())
                                .font(k.ty.body_medium)
                                .color(k.c.text_base)
                                .truncate()
                                .shrink()
                                .min_w(0.0),
                        )
                        .child(el().grow(1.0))
                        .child(parts::tag(template.path.clone(), k)),
                )
                .child(
                    row()
                        .gap(8.0)
                        .center_y()
                        .child(
                            text(template.current.clone())
                                .font(k.ty.mono)
                                .color(k.c.text_muted),
                        )
                        .child(rune::icon(&parts::ARROW, 14.0).color(k.c.icon_accent))
                        .child(
                            text(template.available.clone())
                                .font(k.ty.mono)
                                .color(k.c.text_base),
                        ),
                )
                .child(buttons),
        );
    }
    scrolling("templates", 720.0, list)
}

pub fn warnings(s: &App, k: &Kit) -> El {
    if s.data.warnings.is_empty() {
        return parts::blank(
            parts::disc(&parts::DONE, k.c.success.icon, k),
            "No warnings",
            "Every lookup answered, and the Count has nothing to add.",
            k,
        );
    }
    let failures = s
        .data
        .warnings
        .iter()
        .filter(|w| w.tone == Tone::Failure)
        .count();
    let mut list = col().gap(10.0);
    if failures > 0 {
        list = list.child(
            text(format!(
                "{} failed, so those dependencies may have updates the Count cannot see.",
                parts::count(failures, "lookup", "lookups")
            ))
            .font(k.ty.ui)
            .color(k.c.text_muted)
            .wrap()
            .mb(4.0),
        );
    }
    let several = s.data.repositories.len() > 1;
    for (i, warning) in s.data.warnings.iter().enumerate() {
        let hue = match warning.tone {
            Tone::Failure => k.c.danger.icon,
            Tone::Warning => k.c.warning.icon,
            Tone::Notice => k.c.info.icon,
        };
        let mut head =
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(parts::status(warning.tag.clone(), hue, k));
        if several {
            head = head.child(parts::tag(warning.repository.clone(), k));
        }
        let mut entry = parts::card(k)
            .key(format!("warning-{i}"))
            .padding(14.0)
            .gap(8.0)
            .child(head)
            .child(
                text(warning.title.clone())
                    .font(k.ty.ui_medium)
                    .color(k.c.text_base)
                    .wrap()
                    .selectable(true),
            );
        if !warning.detail.is_empty() {
            entry = entry.child(
                text(warning.detail.clone())
                    .font(k.ty.text_small)
                    .color(k.c.text_muted)
                    .wrap()
                    .selectable(true),
            );
        }
        list = list.child(entry);
    }
    scrolling("warnings", 720.0, list)
}

/// Right-aligned figures for the stats page's rows.
pub fn figure(value: impl Into<String>, k: &Kit) -> El {
    text(value)
        .font(k.ty.text_small_medium)
        .color(k.c.text_base)
        .align(Align::End)
}
