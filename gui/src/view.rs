//! The window: a sidebar of pages, the page in the main pane under a
//! toolbar, and an inspector beside the updates. Every page reads [`App`] and
//! draws; nothing here changes state except through a handler.
mod lists;
mod parts;
mod review;
mod stats;
#[cfg(test)]
mod tests;
mod updates;

use crate::{
    app::{App, Lookup, Page, Review},
    cli::Failure,
    theme,
};
use rune::{Color, Cx, El, Key, KeyEvent, Tx, col, el, row, text};
use rune_kit::{
    Button, ButtonVariant, ControlSize, Dialog, IconButton, IconButtonVariant, Kit, Spinner,
    TextInput,
    shell::{self, RowStatus, Sidebar, SidebarRow, SplitView, Toolbar},
};

/// The keys the window answers, for the Keys sheet: the TUI's where there is one.
const KEYS: [(&str, &str); 12] = [
    ("1 – 6", "Jump to a page"),
    ("/  or  ⌘F", "Search names, packages and files"),
    ("T", "Cycle the update type filter"),
    ("↑ ↓", "Move through the rows"),
    ("Space", "Pick or put back the update under the cursor"),
    ("A", "Pick every update shown, or clear them"),
    ("W", "Review what the picks would write"),
    ("⌘↩", "In the review, apply"),
    ("J  K", "In the review, next and previous change"),
    ("Esc", "Leave the review, the search or this sheet"),
    ("⌘R", "Look again"),
    ("⇧⌘B  ⌥⌘I", "Show or hide the sidebar and the inspector"),
];

pub fn view(s: &App, cx: &mut Cx<App>) -> El {
    let dark = cx.system_dark();
    let k = theme::kit(dark);
    let mut layer = Vec::new();
    let reviewing = s.review != Review::Closed;
    let ready = s.lookup == Lookup::Ready || !s.data.repositories.is_empty();
    let main = match &s.lookup {
        _ if reviewing => review::page(s, &k),
        Lookup::Failed(failure) => failed(s, failure, &k),
        _ if !ready => loading(s, &k),
        _ => match s.page {
            Page::Updates => updates::page(s, &k),
            Page::Held => lists::held(s, &k),
            Page::Dependencies => lists::dependencies(s, &k),
            Page::Templates => lists::templates(s, &k),
            Page::Warnings => lists::warnings(s, &k),
            Page::Stats => stats::page(s, &k),
        },
    };
    // A page of the report is on screen, not a state in its place.
    let browsing = ready && !reviewing && !matches!(s.lookup, Lookup::Failed(_));
    let inspecting = browsing && s.page == Page::Updates;

    // The shell paints nothing behind its panes, so the night sky shows through.
    let mut clear = k.clone();
    clear.c.bg_deep = Color::TRANSPARENT;
    let mut split = SplitView::new(main)
        .sidebar(
            &s.sidebar,
            |s: &mut App| &mut s.sidebar,
            sidebar(s, reviewing, &k),
        )
        .toolbar(toolbar(s, reviewing, browsing, &k))
        .titlebar_inset(cx.titlebar_inset())
        .frame_inset(8.0)
        .toolbar_in_panel()
        .reduced_motion(cx.is_reduced_motion());
    if inspecting {
        split = split.inspector(
            &s.inspector,
            |s: &mut App| &mut s.inspector,
            updates::inspector(s, &k),
        );
    }
    Dialog::new("keys", "Keys")
        .description("The window answers the TUI's keys where it has the same thing to do.")
        .open(s.help)
        .width(460.0)
        .fit()
        .on_close(|s: &mut App| s.help = false)
        .child(keys(&k))
        .build(&k, &mut layer);
    rune_kit::root(sky(dark, &k).child(split.build(&clear)), layer)
}

/// The window's backdrop: the docs' night sky, with the coral and purple
/// glows of its landing page behind the sidebar and the frame.
fn sky(dark: bool, k: &Kit) -> El {
    let brand = theme::brand(dark);
    let strength = if dark { 1.0 } else { 0.6 };
    let glow = |center: [f32; 2], radius: [f32; 2], hue: Color, alpha: f32| {
        el().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
            .a11y_hidden()
            .radial_gradient(
                center,
                radius,
                [(0.0, hue.alpha(alpha * strength)), (1.0, hue.alpha(0.0))],
            )
    };
    el().key("sky")
        .w_full()
        .h_full()
        .fill(k.c.bg_deep)
        .child(glow([0.0, 0.0], [520.0, 420.0], brand.coral, 0.16))
        .child(glow(
            [1.0, 0.0],
            [760.0, 460.0],
            rune::color::hex("#6b4a7a").unwrap_or(brand.purple),
            0.34,
        ))
}

fn sidebar(s: &App, reviewing: bool, k: &Kit) -> El {
    let place = s.launch.as_ref().map_or_else(
        |_| "no launch".to_owned(),
        |launch| format!("{} · {}", launch.place(), launch.origin()),
    );
    let mut mark = el().size(34.0, 34.0);
    if let Some(logo) = &s.logo {
        mark = mark.pixels(logo.pixels());
    }
    let brand = row()
        .key("brand")
        .w_full()
        .gap(10.0)
        .center_y()
        .px(6.0)
        .pb(10.0)
        .child(mark)
        .child(
            col()
                .min_w(0.0)
                .shrink()
                .child(
                    text("Count Patchula")
                        .font(k.ty.ui_medium)
                        .color(k.c.text_base),
                )
                .child(
                    text(place.clone())
                        .font(k.ty.text_small)
                        .color(k.c.text_muted)
                        .truncate()
                        .help_when_truncated(place),
                ),
        );
    let data = &s.data;
    let counts = [
        data.updates.len(),
        data.held.len(),
        data.found.len(),
        data.templates.len(),
        data.warnings.len(),
        0,
    ];
    let icons = [
        &parts::UPDATES,
        &parts::HELD,
        &parts::FOUND,
        &parts::TEMPLATES,
        &parts::WARNINGS,
        &parts::STATS,
    ];
    let mut bar = Sidebar::new().label("Pages").header(brand);
    for (i, page) in Page::ALL.into_iter().enumerate() {
        let mut item = SidebarRow::new(page.title())
            .icon(icons[i])
            .height(32.0)
            .selected(s.page == page && !reviewing)
            .on_select(move |s: &mut App, _| {
                s.close_review();
                s.page = page;
            });
        if counts[i] > 0 {
            item = item.detail(counts[i].to_string());
        }
        bar = bar.child(item.build(k).key(format!("page-{}", i + 1)));
    }
    let mut again = SidebarRow::new(if s.lookup == Lookup::Running {
        "Looking…"
    } else {
        "Look again"
    })
    .icon(&parts::REFRESH)
    .height(32.0)
    .detail("⌘R")
    .select_only()
    .on_select(|s: &mut App, _| s.refresh());
    if s.lookup == Lookup::Running {
        again = again.status(RowStatus::Busy);
    }
    bar.footer(again.build(k).key("refresh"))
        .footer(
            SidebarRow::new("Keys")
                .icon(&parts::KEYS)
                .height(32.0)
                .detail("?")
                .select_only()
                .on_select(|s: &mut App, _| s.help = true)
                .build(k)
                .key("keys"),
        )
        .build(k)
}

fn toolbar(s: &App, reviewing: bool, browsing: bool, k: &Kit) -> Toolbar {
    let title = if reviewing { "Review" } else { s.page.title() };
    let mut bar = Toolbar::new().leading(
        text(title)
            .key("title")
            .font(k.ty.ui_medium)
            .color(k.c.text_base),
    );
    if browsing && s.page.searchable() {
        let mut search = TextInput::new(&s.search)
            .label("Search names, packages and files")
            .placeholder("Search")
            .leading_icon(k.glyph.search);
        if !s.search.text.is_empty() {
            search = search.clear_button();
        }
        bar = bar.trailing(
            search
                .width(Some(260.0))
                .on_edit(|s: &mut App| &mut s.search)
                .on_change(|s: &mut App, _: &mut Tx| s.arrange())
                .on_key(|s: &mut App, ev: &KeyEvent, tx: &mut Tx| {
                    if ev.key != Key::Escape {
                        return false;
                    }
                    s.search.clear();
                    s.arrange();
                    tx.blur();
                    true
                })
                .build(k)
                .key("search"),
        );
    }
    if browsing && s.page == Page::Updates {
        let open = s.inspector.is_open();
        bar = bar.trailing(
            IconButton::new(&shell::INSPECTOR)
                .variant(IconButtonVariant::GhostMuted)
                .expanded(open)
                .label(if open {
                    "Hide inspector"
                } else {
                    "Show inspector"
                })
                .help(if open {
                    "Hide inspector"
                } else {
                    "Show inspector"
                })
                .shortcut("⌥⌘I")
                .on_press(|s: &mut App| s.inspector.toggle())
                .build(k)
                .key("inspector-toggle"),
        );
    }
    bar
}

fn logo(s: &App, size: f32) -> El {
    let mark = el().size(size, size);
    match &s.logo {
        Some(logo) => mark.pixels(logo.pixels()),
        None => mark,
    }
}

fn loading(s: &App, k: &Kit) -> El {
    col()
        .key("loading")
        .w_full()
        .h_full()
        .center()
        .gap(8.0)
        .child(logo(s, 120.0))
        .child(
            row()
                .gap(10.0)
                .center_y()
                .mt(8.0)
                .child(Spinner::new().size(16.0).build(k))
                .child(text("The Count is reading the manifests").font(k.ty.body_medium).color(k.c.text_base)),
        )
        .child(
            text("He looks up every dependency he finds. A first visit takes longest; after that he remembers.")
                .font(k.ty.ui)
                .color(k.c.text_muted)
                .wrap()
                .align(rune::Align::Center)
                .max_w(420.0),
        )
}

fn failed(s: &App, failure: &Failure, k: &Kit) -> El {
    let again = |label: &str, variant| {
        Button::new(label)
            .variant(variant)
            .control_size(ControlSize::Large)
            .on_press(|s: &mut App| s.refresh())
            .build(k)
            .key("failed-again")
    };
    let (mark, title, buttons) = if failure.uninvited {
        (
            logo(s, 120.0),
            "The Count has not been invited in",
            row()
                .gap(8.0)
                .child(
                    Button::new("Use his defaults")
                        .variant(ButtonVariant::Contrast)
                        .control_size(ControlSize::Large)
                        .help("For this window only: the same as --default-config")
                        .on_press(|s: &mut App| s.use_defaults())
                        .build(k)
                        .key("failed-defaults"),
                )
                .child(again("Look again", ButtonVariant::Neutral)),
        )
    } else {
        (
            parts::disc(&parts::WARNINGS, k.c.danger.icon, k),
            if s.launch.is_err() {
                "The window could not start"
            } else {
                "The lookup failed"
            },
            row().gap(8.0).children(
                s.launch
                    .is_ok()
                    .then(|| again("Try again", ButtonVariant::Contrast)),
            ),
        )
    };
    let detail = if failure.uninvited {
        "This tree has no patchula.pkl, so he waits at the door. Use his defaults for now, or create patchula.pkl containing `amends \"PatchulaConfig.pkl\"` to invite him for good.".to_owned()
    } else {
        failure.message.clone()
    };
    col()
        .key("failed")
        .w_full()
        .h_full()
        .center()
        .gap(6.0)
        .padding(32.0)
        .child(mark.mb(8.0))
        .child(text(title).font(k.ty.body_medium).color(k.c.text_base))
        .child(
            text(detail)
                .key("failed-detail")
                .font(k.ty.ui)
                .color(k.c.text_muted)
                .wrap()
                .align(rune::Align::Center)
                .max_w(520.0)
                .selectable(true),
        )
        .child(buttons.mt(14.0))
}

fn keys(k: &Kit) -> El {
    let mut list = col().w_full().px(16.0).pb(16.0).gap(6.0);
    for (keys, does) in KEYS {
        list = list.child(
            row()
                .w_full()
                .gap(12.0)
                .center_y()
                .child(
                    row().w(112.0).child(
                        row()
                            .h(22.0)
                            .px(7.0)
                            .center_y()
                            .radius(5.0)
                            .fill(k.c.layer_03)
                            .border(1.0, k.c.border_muted)
                            .child(text(keys).font(k.ty.text_small_medium).color(k.c.text_base)),
                    ),
                )
                .child(text(does).font(k.ty.ui).color(k.c.text_muted)),
        );
    }
    list
}

/// What a key does when no control took it. True consumes the key.
pub fn on_key(s: &mut App, ev: &KeyEvent, tx: &mut Tx) -> bool {
    if shell::is_sidebar_toggle(ev) {
        s.sidebar.toggle();
        return true;
    }
    if ev.is_command('i') && ev.mods.alt {
        s.inspector.toggle();
        return true;
    }
    if ev.is_command('r') {
        s.refresh();
        return true;
    }
    if ev.is_command('f') && s.review == Review::Closed && s.page.searchable() {
        s.search.focused = true;
        return true;
    }
    if s.help {
        if ev.key == Key::Escape {
            s.help = false;
        }
        return ev.key == Key::Escape;
    }
    if s.review != Review::Closed {
        if ev.key == Key::Escape {
            s.close_review();
            return true;
        }
        if ev.key == Key::Enter && ev.mods.cmd {
            s.write();
            return true;
        }
        return matches!(s.review, Review::Ready(_)) && s.diff.key(ev, tx);
    }
    if ev.mods.cmd || ev.mods.ctrl || ev.mods.alt {
        return false;
    }
    let typed = match &ev.key {
        // With no table focused, the arrows still move its cursor.
        Key::Down => "j",
        Key::Up => "k",
        Key::Text(typed) => typed.as_str(),
        _ => return false,
    };
    match typed {
        "j" | "k" => {
            if !s.step(if typed == "j" { 1 } else { -1 }, tx) {
                return false;
            }
        }
        "?" => s.help = true,
        "/" if s.page.searchable() => s.search.focused = true,
        "t" | "T" if s.page == Page::Updates => s.cycle_filter(),
        " " if s.page == Page::Updates => match s.cursor() {
            Some(index) => s.toggle(index),
            None => return false,
        },
        "a" | "A" if s.page == Page::Updates => s.toggle_visible(),
        "w" | "W" if s.page == Page::Updates => s.plan(),
        digit => match digit
            .parse::<usize>()
            .ok()
            .and_then(|n| Page::ALL.get(n.wrapping_sub(1)))
        {
            Some(page) => s.page = *page,
            None => return false,
        },
    }
    true
}
