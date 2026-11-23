//! The window driven as a person drives it: presses on what the view drew,
//! and keys through the same routing a real window gives them. Rune's runtime
//! runs the view and its handlers with no GPU.
use super::{on_key, view};
use crate::app::{App, Page, Review};
use patchula_words::Update as UpdateKind;
use rune::{Env, Key, KeyEvent, Mods, Runtime};

fn window() -> Runtime<App> {
    let mut window = Runtime::new(App::demo(), view, (1280.0, 800.0), Env::new(true, 2.0));
    window.on_key = Some(on_key);
    window.frame(0.0);
    window
}

fn press(window: &mut Runtime<App>, key: &str) {
    // What the last press brought in is hit where it is drawn, so let it land.
    for _ in 0..30 {
        window.frame(1.0 / 60.0);
    }
    let (x, y) = window
        .find(key)
        .unwrap_or_else(|| panic!("nothing on screen is keyed {key}"));
    window.pointer_down(x, y);
    window.pointer_up(x, y);
    window.frame(1.0 / 60.0);
}

fn key(window: &mut Runtime<App>, key: Key) {
    window.key(KeyEvent {
        key,
        mods: Mods::default(),
    });
    window.frame(1.0 / 60.0);
}

fn typed(window: &mut Runtime<App>, text: &str) {
    for c in text.chars() {
        key(window, Key::Text(c.to_string()));
    }
}

/// The text of the first element with this key.
fn words(window: &Runtime<App>, key: &str) -> String {
    window
        .elements()
        .into_iter()
        .find(|element| element.key.as_deref() == Some(key))
        .and_then(|element| element.text)
        .unwrap_or_else(|| panic!("no text keyed {key}"))
}

fn index(window: &Runtime<App>, name: &str) -> usize {
    window
        .state
        .data
        .updates
        .iter()
        .position(|update| update.name == name)
        .unwrap()
}

#[test]
fn the_sidebar_and_the_digits_change_the_page() {
    let mut window = window();
    assert_eq!(words(&window, "title"), "Updates");
    press(&mut window, "page-2");
    assert_eq!(window.state.page, Page::Held);
    assert_eq!(words(&window, "title"), "Held back");
    typed(&mut window, "6");
    assert_eq!(window.state.page, Page::Stats);
    assert_eq!(
        words(&window, "hero"),
        window.state.data.updates.len().to_string()
    );
    typed(&mut window, "1");
    assert_eq!(window.state.page, Page::Updates);
}

#[test]
fn a_box_picks_its_update_and_the_bar_counts_the_picks() {
    let mut window = window();
    let serde = index(&window, "serde");
    assert_eq!(
        words(&window, "summary"),
        "Pick updates to write them into the working tree."
    );
    press(&mut window, &format!("u{serde}/pick/box"));
    assert!(window.state.picked.contains(&serde));
    assert_eq!(words(&window, "summary"), "1 update picked");
    press(&mut window, &format!("u{serde}/pick/box"));
    assert!(window.state.picked.is_empty());
}

#[test]
fn the_tuis_keys_move_pick_filter_and_select_all() {
    let mut window = window();
    assert_eq!(window.state.cursor(), Some(0));
    key(&mut window, Key::Down);
    typed(&mut window, "j");
    assert_eq!(window.state.cursor(), Some(2));
    typed(&mut window, "k ");
    assert_eq!(window.state.picked.iter().copied().collect::<Vec<_>>(), [1]);
    typed(&mut window, "t");
    assert_eq!(window.state.filter, Some(UpdateKind::Major));
    typed(&mut window, "a");
    let majors = window.state.rows.updates.len();
    assert_eq!(window.state.picked.len(), majors);
    typed(&mut window, "a");
    assert!(window.state.picked.is_empty());
}

#[test]
fn slash_focuses_the_search_and_typing_narrows_the_rows() {
    let mut window = window();
    typed(&mut window, "/");
    typed(&mut window, "react");
    assert_eq!(window.state.search.text, "react");
    assert_eq!(window.state.rows.updates.len(), 3);
    // In the field, a digit is a character, not a page.
    typed(&mut window, "1");
    assert_eq!(window.state.page, Page::Updates);
    assert_eq!(window.state.search.text, "react1");
    key(&mut window, Key::Escape);
    assert_eq!(window.state.search.text, "");
    assert_eq!(
        window.state.rows.updates.len(),
        window.state.data.updates.len()
    );
}

#[test]
fn review_shows_the_plan_and_apply_writes_it() {
    let mut window = window();
    let (serde, lodash) = (index(&window, "serde"), index(&window, "lodash"));
    press(&mut window, &format!("u{serde}/pick/box"));
    press(&mut window, &format!("u{lodash}/pick/box"));
    press(&mut window, "review");
    assert!(
        matches!(window.state.review, Review::Ready(_)),
        "{:?}",
        window.state.review
    );
    assert_eq!(words(&window, "title"), "Review");
    press(&mut window, "review-apply");
    assert!(
        matches!(window.state.review, Review::Done(_)),
        "{:?}",
        window.state.review
    );
    assert_eq!(
        words(&window, "done-headline"),
        "The Count applied 2 updates to 2 files."
    );
    press(&mut window, "review-back");
    assert_eq!(window.state.review, Review::Closed);
    // Written updates keep their row and lose their box, once it has faded out.
    for _ in 0..60 {
        window.frame(1.0 / 60.0);
    }
    assert!(window.find(&format!("u{serde}/pick/box")).is_none());
    assert!(window.find(&format!("u{serde}")).is_some());
}

#[test]
fn w_reviews_and_escape_leaves_without_writing() {
    let mut window = window();
    typed(&mut window, " w");
    assert!(matches!(window.state.review, Review::Ready(_)));
    key(&mut window, Key::Escape);
    assert_eq!(window.state.review, Review::Closed);
    assert_eq!(window.state.picked.len(), 1);
    assert!(window.state.applied.is_empty());
}

#[test]
fn the_keys_sheet_opens_and_escape_closes_it() {
    let mut window = window();
    press(&mut window, "keys");
    assert!(window.state.help);
    assert!(window.find("keys-dialog").is_some());
    key(&mut window, Key::Escape);
    assert!(!window.state.help);
}
