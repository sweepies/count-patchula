//! The small pieces every page shares: icons, the update-type mark, cards,
//! and the centred states a page shows when it has no rows.
use crate::theme;
use patchula_words::Update as UpdateKind;
use rune::{Color, El, Icon, col, el, icon, row, text};
use rune_kit::Kit;

macro_rules! icons {
    ($($name:ident = $id:literal, $body:literal;)+) => {
        $(pub static $name: Icon = Icon { name: $id, view: 16.0, body: $body };)+
    };
}

icons! {
    UPDATES = "count-updates", r#"<circle cx="8" cy="8" r="5.75" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M8 10.75V5.5M5.75 7.6L8 5.35L10.25 7.6" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#;
    HELD = "count-held", r#"<circle cx="8" cy="8" r="5.75" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M8 4.9V8L10.1 9.4" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#;
    FOUND = "count-found", r#"<path d="M8 2.25L13.25 5V11L8 13.75L2.75 11V5Z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" fill="none"/><path d="M2.9 5.1L8 7.75L13.1 5.1M8 7.75V13.6" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" fill="none"/>"#;
    TEMPLATES = "count-templates", r#"<path d="M8 2.75L13.5 5.75L8 8.75L2.5 5.75Z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" fill="none"/><path d="M2.5 8.75L8 11.75L13.5 8.75" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#;
    WARNINGS = "count-warnings", r#"<path d="M8 2.6L14 13H2Z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" fill="none"/><path d="M8 6.6V9.6" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/><circle cx="8" cy="11.3" r="0.7" fill="currentColor"/>"#;
    STATS = "count-stats", r#"<path d="M3.5 13V8.5M8 13V3.5M12.5 13V6" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>"#;
    REFRESH = "count-refresh", r#"<path d="M12.9 8A4.9 4.9 0 1 1 11.3 4.4" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" fill="none"/><path d="M11.6 2.1V4.7H9" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#;
    SHIELD = "count-shield", r#"<path d="M8 2L13 3.9V7.6C13 10.7 10.9 13 8 14C5.1 13 3 10.7 3 7.6V3.9Z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" fill="none"/><path d="M5.9 8L7.4 9.5L10.2 6.6" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#;
    ARROW = "count-arrow", r#"<path d="M3 8H12.5M9.25 4.75L12.5 8L9.25 11.25" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#;
    DONE = "count-done", r#"<circle cx="8" cy="8" r="5.75" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M5.5 8.2L7.2 9.9L10.5 6.4" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#;
    KEYS = "count-keys", r#"<rect x="1.75" y="4.25" width="12.5" height="7.5" rx="1.75" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M4.5 7H4.6M8 7H8.1M11.4 7H11.5M5.5 9.5H10.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>"#;
}

/// An update type as a mark and a word: the colour is on the dot, the word
/// stays in ink, so the type never rests on colour alone.
pub fn kind(kind: UpdateKind, k: &Kit) -> El {
    let hue = theme::kind(kind, k.dark);
    row()
        .h(20.0)
        .px(7.0)
        .gap(6.0)
        .center_y()
        .radius(10.0)
        .fill(hue.alpha(if k.dark { 0.16 } else { 0.12 }))
        .child(el().size(6.0, 6.0).radius(3.0).fill(hue))
        .child(
            text(kind.as_str())
                .font(k.ty.text_small_medium)
                .color(k.c.text_base),
        )
}

/// A quiet rounded tag, for a manager's name or a count.
pub fn tag(label: impl Into<String>, k: &Kit) -> El {
    row()
        .h(20.0)
        .px(7.0)
        .center_y()
        .radius(10.0)
        .fill(k.c.layer_02)
        .child(text(label).font(k.ty.text_small).color(k.c.text_muted))
}

/// A tag in a status colour, with its dot.
pub fn status(label: impl Into<String>, hue: Color, k: &Kit) -> El {
    row()
        .h(20.0)
        .px(7.0)
        .gap(6.0)
        .center_y()
        .radius(10.0)
        .fill(hue.alpha(if k.dark { 0.16 } else { 0.12 }))
        .child(el().size(6.0, 6.0).radius(3.0).fill(hue))
        .child(
            text(label)
                .font(k.ty.text_small_medium)
                .color(k.c.text_base),
        )
}

/// A raised surface for a group of related content.
pub fn card(k: &Kit) -> El {
    col()
        .w_full()
        .radius(k.radius.lg)
        .fill(k.c.layer_01)
        .border(1.0, k.c.border_muted)
}

/// A small heading over a group, in the muted caps-free style of the sidebar.
pub fn heading(label: impl Into<String>, k: &Kit) -> El {
    text(label)
        .font(k.ty.text_small_medium)
        .color(k.c.text_muted)
}

/// A page's centred state: an icon, what happened and what to do about it.
pub fn blank(mark: El, title: impl Into<String>, detail: impl Into<String>, k: &Kit) -> El {
    col()
        .w_full()
        .h_full()
        .center()
        .gap(6.0)
        .padding(32.0)
        .child(mark.mb(8.0))
        .child(text(title).font(k.ty.body_medium).color(k.c.text_base))
        .child(
            text(detail)
                .font(k.ty.ui)
                .color(k.c.text_muted)
                .wrap()
                .align(rune::Align::Center)
                .max_w(440.0),
        )
}

/// An icon on a soft disc, for a blank state.
pub fn disc(i: &'static Icon, hue: Color, k: &Kit) -> El {
    el().size(48.0, 48.0)
        .radius(24.0)
        .center()
        .fill(hue.alpha(if k.dark { 0.16 } else { 0.12 }))
        .child(icon(i, 22.0).color(hue))
}

/// "1 update" or "3 updates".
pub fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}
