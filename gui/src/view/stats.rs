//! The Stats page: the run in numbers. Counts are figures, the split of
//! updates by type is one thin segmented bar over its own list, and the
//! per-manager and per-phase numbers are ranked bars in one colour. Every bar
//! sits beside its figure, so no value is read from a bar's length alone.
use super::{lists::figure, parts};
use crate::{app::App, theme};
use rune::{Color, El, ScrollSpec, TextStyle, col, el, row, text};
use rune_kit::Kit;

/// A number with its label under it.
fn tile(label: &str, value: String, k: &Kit) -> El {
    parts::card(k)
        .padding(14.0)
        .gap(2.0)
        .grow(1.0)
        .min_w(120.0)
        .child(
            text(value)
                .font(
                    TextStyle {
                        size: 22.0,
                        weight: 600.0,
                        ..k.ty.body_medium
                    }
                    .lh(28.0),
                )
                .color(k.c.text_base),
        )
        .child(
            text(label.to_owned())
                .font(k.ty.text_small)
                .color(k.c.text_muted),
        )
}

/// 1284 as "1,284".
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Microseconds as the CLI prints phase times.
fn millis(micros: u64) -> String {
    format!("{} ms", grouped(micros.div_ceil(1000)))
}

/// A titled card.
fn section(title: &str, note: Option<String>, k: &Kit) -> El {
    let mut head = row().w_full().gap(8.0).center_y().child(
        text(title.to_owned())
            .font(k.ty.ui_medium)
            .color(k.c.text_base),
    );
    if let Some(note) = note {
        head = head
            .child(el().grow(1.0))
            .child(text(note).font(k.ty.text_small).color(k.c.text_muted));
    }
    parts::card(k).padding(16.0).gap(14.0).child(head)
}

/// One ranked bar: a name, a thin bar on a shared baseline, and its figure.
fn ranked(
    name: String,
    share: f32,
    value: String,
    note: Option<String>,
    hue: Color,
    k: &Kit,
) -> El {
    let bar = el()
        .w_frac(share.clamp(0.0, 1.0))
        .min_w(2.0)
        .h(8.0)
        // Square at the baseline, rounded at the data end.
        .corners([0.0, 4.0, 4.0, 0.0])
        .fill(hue);
    let mut line = row()
        .w_full()
        .h(24.0)
        .gap(12.0)
        .center_y()
        .help(format!("{name}: {value}"))
        .child(
            text(name)
                .font(k.ty.text_small)
                .color(k.c.text_base)
                .truncate()
                .w(132.0),
        )
        .child(row().grow(1.0).min_w(0.0).center_y().child(bar))
        .child(figure(value, k).w(64.0));
    if let Some(note) = note {
        line = line.child(
            text(note)
                .font(k.ty.text_small)
                .color(k.c.text_muted)
                .w(84.0),
        );
    }
    line
}

pub fn page(s: &App, k: &Kit) -> El {
    let data = &s.data;
    let stats = &data.stats;
    let accent = k.c.text_accent;

    // The figure the page leads with, and the counts beside it.
    let lead = col()
        .gap(0.0)
        .child(
            text(data.updates.len().to_string())
                .key("hero")
                .font(
                    TextStyle {
                        size: 56.0,
                        weight: 600.0,
                        ..k.ty.body_medium
                    }
                    .lh(60.0)
                    .tracking(-1.0),
                )
                .color(k.c.text_base),
        )
        .child(
            text(format!(
                "{} across {}",
                if data.updates.len() == 1 {
                    "update awaits"
                } else {
                    "updates await"
                },
                parts::count(data.found.len(), "dependency", "dependencies")
            ))
            .font(k.ty.ui)
            .color(k.c.text_muted),
        );
    let counts = row()
        .w_full()
        .gap(12.0)
        .flex_wrap()
        .child(tile("Dependencies", grouped(data.found.len() as u64), k))
        .child(tile("Held back", grouped(data.held.len() as u64), k))
        .child(tile(
            "Template updates",
            grouped(data.templates.len() as u64),
            k,
        ))
        .child(tile("Warnings", grouped(data.warnings.len() as u64), k));

    let mut page = col().gap(16.0).child(lead).child(counts);

    // One whole split by type: a thin segmented bar over the list that names
    // each part and gives its figure.
    let total = data.updates.len();
    if total > 0 {
        let mut bar = row().w_full().h(8.0).gap(2.0);
        let present: Vec<_> = stats.kinds.iter().filter(|(_, n)| *n > 0).collect();
        for (i, (kind, n)) in present.iter().enumerate() {
            let (first, last) = (i == 0, i + 1 == present.len());
            let (left, right) = (if first { 4.0 } else { 0.0 }, if last { 4.0 } else { 0.0 });
            bar = bar.child(
                el().grow(*n as f32)
                    .h(8.0)
                    .min_w(4.0)
                    .corners([left, right, right, left])
                    .fill(theme::kind(*kind, k.dark))
                    .help(format!("{kind}: {n} of {total}")),
            );
        }
        let mut list = col().gap(2.0);
        for (kind, n) in &stats.kinds {
            let share = *n as f32 * 100.0 / total as f32;
            list = list.child(
                row()
                    .w_full()
                    .h(24.0)
                    .gap(8.0)
                    .center_y()
                    .child(
                        el().size(8.0, 8.0)
                            .radius(2.0)
                            .fill(theme::kind(*kind, k.dark)),
                    )
                    .child(
                        text(kind.as_str())
                            .font(k.ty.text_small)
                            .color(k.c.text_base)
                            .w(120.0),
                    )
                    .child(el().grow(1.0))
                    .child(figure(n.to_string(), k).w(40.0))
                    .child(
                        figure(format!("{share:.0}%"), k)
                            .color(k.c.text_muted)
                            .w(44.0),
                    ),
            );
        }
        page = page.child(
            section("Updates by type", None, k)
                .key("by-type")
                .child(bar)
                .child(list),
        );
    }

    if let Some(most) = stats
        .managers
        .iter()
        .map(|(_, found, _)| *found)
        .max()
        .filter(|most| *most > 0)
    {
        let mut rows = col().gap(4.0);
        for (manager, found, updates) in &stats.managers {
            rows = rows.child(ranked(
                manager.to_string(),
                *found as f32 / most as f32,
                found.to_string(),
                Some(if *updates == 0 {
                    "up to date".to_owned()
                } else {
                    parts::count(*updates, "update", "updates")
                }),
                accent,
                k,
            ));
        }
        page = page.child(
            section("Dependencies by manager", Some("dependencies".into()), k)
                .key("by-manager")
                .child(rows),
        );
    }

    if let Some(network) = &stats.network {
        let tiles = row()
            .w_full()
            .gap(12.0)
            .flex_wrap()
            .child(tile("Requests", grouped(network.requests), k))
            .child(tile("Not modified (304)", grouped(network.not_modified), k))
            .child(tile("Cache hits", grouped(network.cache_hits), k))
            .child(tile("Unique lookups", grouped(network.lookups), k));
        page = page.child(
            col()
                .gap(8.0)
                .child(parts::heading("The lookup", k))
                .child(tiles),
        );
    }

    if let Some(longest) = stats
        .phases
        .first()
        .map(|(_, micros)| *micros)
        .filter(|longest| *longest > 0)
    {
        let mut rows = col().gap(4.0);
        for (phase, micros) in &stats.phases {
            rows = rows.child(ranked(
                phase.clone(),
                *micros as f32 / longest as f32,
                millis(*micros),
                None,
                accent,
                k,
            ));
        }
        page = page.child(
            section("Where the time went", None, k)
                .key("phases")
                .child(rows),
        );
    }

    col()
        .key("stats")
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
                .child(page.w_full().max_w(760.0)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn figures_group_thousands_and_round_times_up() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1284), "1,284");
        assert_eq!(grouped(1_234_567), "1,234,567");
        assert_eq!(millis(1_312_000), "1,312 ms");
        assert_eq!(millis(900), "1 ms");
    }
}
