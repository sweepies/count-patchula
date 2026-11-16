//! The Updates page: the type filter, the table of proposed updates with a
//! box to pick each, the bar that reviews the picks, and the inspector.
use super::parts;
use crate::{
    app::{App, Page},
    model::Update,
    theme, time,
};
use patchula_words::Update as UpdateKind;
use rune::{Align, El, ScrollSpec, Tx, col, el, icon, row, text};
use rune_kit::{
    Button, ButtonVariant, Cell, Checkbox, ControlSize, Kit, SegmentedControl, State, Table,
    TableColumn,
};
use std::borrow::Cow;

pub fn page(s: &App, k: &Kit) -> El {
    col()
        .key("updates")
        .w_full()
        .h_full()
        .child(filters(s, k))
        .child(table(s, k))
        .child(actions(s, k))
}

/// The select-all box, the type filter with each type's count, and how many
/// rows the search and filter leave.
fn filters(s: &App, k: &Kit) -> El {
    let counts = &s.data.stats.kinds;
    let mut labels = vec![format!("All {}", s.data.updates.len())];
    labels.extend(counts.iter().map(|(kind, n)| {
        let word = kind.as_str();
        format!("{}{} {n}", word[..1].to_uppercase(), &word[1..])
    }));
    let selected = s.filter.map_or(0, |kind| usize::from(kind.rank()) + 1);
    let kinds = SegmentedControl::new(&labels, Some(selected))
        .label("Update type")
        .fit()
        .on_pick(|s: &mut App, i, _: &mut Tx| {
            s.set_filter(
                i.checked_sub(1)
                    .and_then(|i| UpdateKind::RANKED.get(i).copied()),
            );
        })
        .build(k);
    let (all, any) = s.visible_picked();
    let mut everything = Checkbox::new(all).indeterminate(any && !all);
    if any && !all {
        // A dash, not a tick, while only some of the rows are picked.
        everything = everything.icon(k.glyph.dash);
    }
    let everything = everything
        .label("Pick every update shown")
        .hide_label()
        .disabled(s.rows.updates.is_empty())
        .on_toggle(|s: &mut App| s.toggle_visible())
        .build(k)
        .key("pick-all")
        .help("Pick every update shown (A)");
    let shown = s.rows.updates.len();
    let total = s.data.updates.len();
    let tally = if shown == total {
        parts::count(total, "update", "updates")
    } else {
        format!("{shown} of {total}")
    };
    row()
        .key("filters")
        .w_full()
        .h(48.0)
        .pl(12.0)
        .pr(16.0)
        .gap(12.0)
        .center_y()
        .child(everything)
        .child(kinds)
        .child(el().grow(1.0))
        .child(text(tally).font(k.ty.text_small).color(k.c.text_muted))
}

fn table(s: &App, k: &Kit) -> El {
    if s.rows.updates.is_empty() {
        let (title, detail) = if s.data.updates.is_empty() {
            (
                "Nothing to update",
                "The Count found every dependency where it should be. Rest easy.",
            )
        } else {
            (
                "No update matches",
                "Nothing here fits the search and the type filter. Loosen one of them.",
            )
        };
        return parts::blank(
            parts::disc(&parts::DONE, k.c.success.icon, k),
            title,
            detail,
            k,
        )
        .grow(1.0)
        .min_h(0.0);
    }
    let kit = k.clone();
    Table::new(
        |s: &App| &s.tables.updates,
        |s: &mut App| &mut s.tables.updates,
        |s: &App| s.rows.updates.len(),
        |s: &App, i| Cow::Borrowed(s.data.updates[s.rows.updates[i]].id.as_str()),
    )
    .name("Updates")
    .column(TableColumn::new("pick", "").width(40.0).resizable(false))
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
            .max(260.0)
            .sortable(),
    )
    .column(
        TableColumn::new("target", "Target")
            .width(124.0)
            .min(70.0)
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
        TableColumn::new("age", "Age")
            .width(64.0)
            .min(48.0)
            .max(120.0)
            .align(Align::End)
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
        let index = s.rows.updates[i];
        let update = &s.data.updates[index];
        match column {
            "pick" if !s.pickable(index) => Cell::el(
                icon(&parts::DONE, 16.0)
                    .color(k.c.success.icon)
                    .help("Written in this session"),
            ),
            "pick" => Cell::el(
                Checkbox::new(s.picked.contains(&index))
                    .label(format!("Pick {}", update.name))
                    .hide_label()
                    .on_toggle(move |s: &mut App| s.toggle(index))
                    .build(k)
                    .key("box"),
            ),
            "name" => Cell::el(name(update, k)),
            "current" => Cell::text(&update.current).mono().muted(),
            "target" => Cell::text(update.target()).mono(),
            "type" => Cell::el(parts::kind(update.kind, k)),
            "age" => Cell::text(age(update, s.now)).muted(),
            _ => Cell::el(
                text(update.file.clone())
                    .font(k.ty.text_small)
                    .color(k.c.text_muted)
                    .truncate_middle()
                    .shrink()
                    .min_w(0.0)
                    .help_when_truncated(update.file.clone()),
            ),
        }
    })
    .on_sort(|s: &mut App, column, dir, _| s.sort(Page::Updates, column, dir))
    .on_open(|s: &mut App, id, _| {
        if let Some(index) = s.data.position(id) {
            s.toggle(index);
        }
    })
    .build(s, k)
    .grow(1.0)
    .min_h(0.0)
}

/// The dependency's name, with a shield when the update fixes advisories.
fn name(update: &Update, k: &Kit) -> El {
    let mut cell = row().gap(6.0).center_y().min_w(0.0).shrink().child(
        text(update.name.clone())
            .font(k.ty.ui_medium)
            .color(k.c.text_base)
            .truncate()
            .shrink()
            .min_w(0.0)
            .help_when_truncated(update.name.clone()),
    );
    if !update.fixes.is_empty() {
        cell = cell.child(
            icon(&parts::SHIELD, 14.0)
                .color(theme::severity(update.severity(), k))
                .help(format!(
                    "Fixes {}",
                    parts::count(update.fixes.len(), "advisory", "advisories")
                )),
        );
    }
    cell
}

fn age(update: &Update, now: i64) -> String {
    update
        .published_at
        .as_deref()
        .and_then(time::parse)
        .map_or_else(String::new, |published| time::span(now - published))
}

/// The bar under the table: what is picked, and the way into the review.
fn actions(s: &App, k: &Kit) -> El {
    let picked = s.picked.len();
    let blocked = s.review_blocked();
    let mut bar = row()
        .key("actions")
        .w_full()
        .h(56.0)
        .px(16.0)
        .gap(12.0)
        .center_y()
        .fill(k.c.layer_01)
        .border_sides([1.0, 0.0, 0.0, 0.0], k.c.border_muted);
    let summary = if picked == 0 {
        s.read_only()
            .map_or(
                "Pick updates to write them into the working tree.",
                |reason| reason,
            )
            .to_owned()
    } else {
        format!("{} picked", parts::count(picked, "update", "updates"))
    };
    bar = bar.child(
        text(summary)
            .key("summary")
            .font(if picked == 0 { k.ty.ui } else { k.ty.ui_medium })
            .color(if picked == 0 {
                k.c.text_muted
            } else {
                k.c.text_base
            })
            .truncate()
            .shrink()
            .min_w(0.0),
    );
    if picked > 0 {
        bar = bar.child(
            Button::new("Clear")
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Regular)
                .on_press(|s: &mut App| s.picked.clear())
                .build(k)
                .key("clear"),
        );
    }
    bar = bar.child(el().grow(1.0));
    if s.read_only().is_none() || picked > 0 {
        let mut review = Button::new("Review changes")
            .variant(ButtonVariant::Contrast)
            .control_size(ControlSize::Large)
            .shortcut("W")
            .disabled(blocked.is_some())
            .on_press(|s: &mut App| s.plan());
        if let Some(reason) = &blocked {
            review = review.help(reason.clone()).state(State::Disabled);
        }
        bar = bar.child(review.build(k).key("review"));
    }
    bar
}

/// Everything the report says about the update under the cursor.
pub fn inspector(s: &App, k: &Kit) -> El {
    let Some(index) = s.cursor() else {
        return parts::blank(
            parts::disc(&parts::UPDATES, k.c.icon_muted, k),
            "No update selected",
            "Choose a row to see where it comes from and what it fixes.",
            k,
        );
    };
    let update = &s.data.updates[index];
    let hue = theme::kind(update.kind, k.dark);
    let mut top = col().gap(8.0).child(
        text(update.name.clone())
            .key("inspector-name")
            .font(k.ty.body_medium)
            .color(k.c.text_base)
            .wrap_anywhere()
            .selectable(true),
    );
    if update.package != update.name {
        top = top.child(
            text(update.package.clone())
                .font(k.ty.text_small)
                .color(k.c.text_muted)
                .wrap_anywhere(),
        );
    }
    top = top.child(
        row()
            .gap(6.0)
            .flex_wrap()
            .child(parts::kind(update.kind, k))
            .child(parts::tag(update.manager.as_str(), k)),
    );

    let version = |label: &str, value: String, ink| {
        col()
            .grow(1.0)
            .min_w(0.0)
            .gap(2.0)
            .child(
                text(label.to_owned())
                    .font(k.ty.text_small)
                    .color(k.c.text_faint),
            )
            .child(
                text(value)
                    .font(k.ty.mono)
                    .color(ink)
                    .wrap_anywhere()
                    .selectable(true),
            )
    };
    let versions = parts::card(k).key("versions").padding(12.0).child(
        row()
            .w_full()
            .gap(10.0)
            .center_y()
            .child(version("Current", update.current.clone(), k.c.text_muted))
            .child(icon(&parts::ARROW, 16.0).color(hue))
            .child(version("Target", update.target(), k.c.text_base)),
    );

    let fact = |label: &str, value: String| {
        row()
            .w_full()
            .gap(12.0)
            .items_start()
            .child(
                text(label.to_owned())
                    .font(k.ty.text_small)
                    .color(k.c.text_faint)
                    .w(76.0),
            )
            .child(
                text(value)
                    .font(k.ty.text_small)
                    .color(k.c.text_base)
                    .wrap_anywhere()
                    .grow(1.0)
                    .min_w(0.0)
                    .selectable(true),
            )
    };
    let mut facts = col().gap(8.0).child(fact("File", update.file.clone()));
    if s.data.repositories.len() > 1 {
        facts = facts.child(fact("Repository", update.repository.clone()));
    }
    if let Some(published) = &update.published_at {
        let age = time::parse(published).map_or_else(String::new, |at| {
            format!(" · {} ago", time::span(s.now - at))
        });
        facts = facts.child(fact("Published", format!("{}{age}", time::date(published))));
    }
    if let Some(digest) = &update.digest {
        facts = facts.child(fact("Digest", digest.clone()));
    }
    if let Some(group) = &update.group {
        facts = facts.child(fact("Group", group.clone()));
    }

    let mut body = col()
        .key("inspector")
        .w_full()
        .padding(16.0)
        .gap(16.0)
        .child(top)
        .child(versions)
        .child(facts);

    let note = |words: &str| {
        row()
            .w_full()
            .gap(8.0)
            .padding(10.0)
            .radius(k.radius.md)
            .fill(k.c.warning.bg)
            .items_start()
            .child(icon(&parts::WARNINGS, 14.0).color(k.c.warning.icon).mt(2.0))
            .child(
                text(words.to_owned())
                    .font(k.ty.text_small)
                    .color(k.c.text_base)
                    .wrap()
                    .grow(1.0)
                    .min_w(0.0),
            )
    };
    if update.transitive {
        body = body.child(note(
            "Only its lockfile names this package, so only its package manager can update it.",
        ));
    } else if update.locked {
        body = body.child(note(
            "The current version comes from a lockfile. Writing this leaves the lockfile stale; refresh it with your package manager.",
        ));
    }

    if !update.fixes.is_empty() {
        let mut fixes = col().gap(8.0).child(parts::heading(
            format!(
                "Fixes {}",
                parts::count(update.fixes.len(), "advisory", "advisories")
            ),
            k,
        ));
        for fix in &update.fixes {
            let severity = fix.severity.map_or("unrated", |severity| severity.as_str());
            let mut entry = parts::card(k).padding(12.0).gap(6.0).child(
                row()
                    .w_full()
                    .gap(8.0)
                    .center_y()
                    .child(parts::status(severity, theme::severity(fix.severity, k), k))
                    .child(
                        text(fix.id.clone())
                            .font(k.ty.mono)
                            .color(k.c.text_base)
                            .truncate()
                            .shrink()
                            .min_w(0.0),
                    ),
            );
            if let Some(summary) = &fix.summary {
                entry = entry.child(
                    text(summary.clone())
                        .font(k.ty.text_small)
                        .color(k.c.text_base)
                        .wrap(),
                );
            }
            if !fix.cves.is_empty() {
                entry = entry.child(
                    text(fix.cves.join(", "))
                        .font(k.ty.text_small)
                        .color(k.c.text_muted),
                );
            }
            if let Some(url) = fix.url.clone() {
                entry = entry
                    .key(format!("fix-{}", fix.id))
                    .role("link")
                    .label(format!("Open {}", fix.id))
                    .focusable(true)
                    .cursor(rune::Cursor::Pointer)
                    .hover(k.c.hover)
                    .on_press_tx(move |_: &mut App, tx: &mut Tx| {
                        // A refused link opens nothing, which is all there is to do.
                        let _ = tx.open_url(&url);
                    });
            }
            fixes = fixes.child(entry);
        }
        body = body.child(fixes);
    }

    let picked = s.picked.contains(&index);
    let pick = if s.pickable(index) {
        Button::new(if picked { "Picked" } else { "Pick this update" })
            .variant(if picked {
                ButtonVariant::Neutral
            } else {
                ButtonVariant::Outline
            })
            .control_size(ControlSize::Large)
            .shortcut("Space")
            .on_press(move |s: &mut App| s.toggle(index))
            .build(k)
            .key("inspector-pick")
    } else {
        row()
            .gap(6.0)
            .center_y()
            .child(icon(&parts::DONE, 16.0).color(k.c.success.icon))
            .child(
                text("Written in this session")
                    .font(k.ty.ui)
                    .color(k.c.text_muted),
            )
    };
    body = body.child(pick);

    col()
        .key("inspector-scroll")
        .w_full()
        .h_full()
        .min_h(0.0)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .child(body)
}
