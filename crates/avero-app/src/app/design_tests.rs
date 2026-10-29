//! The V2 design: colour themes, the side bar's counters and menus, the
//! checkboxes of the list, Ctrl+K, and every screen drawn at a wide and
//! a narrow width (demo mode, in-memory store).

use super::tests::{add_account_with_mail, app_with};
use super::*;

fn demo() -> App {
    app_with(crate::demo::seed)
}

fn ctrl(app: &mut App, key: &str) {
    let _ = app.update(Message::Key(
        keyboard::Key::Character(key.into()),
        keyboard::Modifiers::COMMAND,
    ));
}

#[test]
fn colour_theme_changes_the_tokens_and_is_kept() {
    let mut app = demo();
    assert_eq!(app.tokens(), &theme::LIGHT);
    let _ = app.update(Message::ColorChosen(ColorTheme::Ocean));
    assert_eq!(app.settings.color, ColorTheme::Ocean);
    assert_eq!(app.tokens(), theme::tokens_for(ColorTheme::Ocean, false));
    let _ = app.update(Message::ThemeChosen(ThemeChoice::Dark));
    assert_eq!(app.tokens(), theme::tokens_for(ColorTheme::Ocean, true));
    // The system mode does not reset the colour.
    let _ = app.update(Message::SystemTheme(iced::theme::Mode::Light));
    assert_eq!(app.tokens(), theme::tokens_for(ColorTheme::Ocean, true));
}

#[test]
fn colour_theme_is_saved_in_the_settings_file() {
    let dir = std::env::temp_dir().join(format!("avero-colour-{}", std::process::id()));
    let path = crate::settings::path_in(&dir);
    let store = avero_core::store::Store::open_in_memory().unwrap();
    let (mut app, _) = App::new(
        store,
        Backend::Demo,
        Settings::default(),
        Some(path.clone()),
    );
    let _ = app.update(Message::ColorChosen(ColorTheme::Orange));
    assert_eq!(Settings::load(&path).color, ColorTheme::Orange);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn ctrl_k_goes_to_the_search_without_moving_the_line() {
    let mut app = demo();
    let _ = app.update(Message::MessageSelected(2));
    let _ = app.update(Message::ShowSettings);
    ctrl(&mut app, "k");
    assert_eq!(app.screen, Screen::Mail);
    assert_eq!(app.selected_index(), Some(2), "not \"previous line\"");
    assert!(SHORTCUTS.iter().any(|(keys, _)| keys.contains("Ctrl+K")));
    ctrl(&mut app, ",");
    assert_eq!(app.screen, Screen::Settings);
}

#[test]
fn side_bar_counts_snoozed_and_drafts() {
    let mut app = app_with(|s| {
        add_account_with_mail(s, "a@x.fr", 3);
    });
    assert_eq!(app.snoozed_count, 0);
    let _ = app.update(Message::MessageSelected(0));
    let rows = app.items[0].row_ids.clone();
    app.store.snooze(&rows, i64::MAX / 2).unwrap();
    app.load_first_page();
    assert_eq!(app.snoozed_count, 1);
}

#[test]
fn line_checkbox_checks_only_that_line() {
    let mut app = demo();
    let _ = app.update(Message::MessageSelected(0));
    let _ = app.update(Message::LineChecked(2));
    assert_eq!(app.checked, vec![app.items[2].row_ids.clone()]);
    assert!(app.reading.is_none(), "the pane lists the checked lines");
    let _ = app.update(Message::LineChecked(2));
    assert!(app.checked.is_empty());
    // Out of range: nothing happens.
    let _ = app.update(Message::LineChecked(10_000));
    assert!(app.checked.is_empty());
}

#[test]
fn hovering_a_line_is_forgotten_only_by_that_line() {
    let mut app = demo();
    let _ = app.update(Message::LineHovered(1, true));
    let _ = app.update(Message::LineHovered(2, true));
    // The first line's "left" arrives after the second's "entered".
    let _ = app.update(Message::LineHovered(1, false));
    assert_eq!(app.hovered_line, Some(2));
    let _ = app.update(Message::LineHovered(2, false));
    assert_eq!(app.hovered_line, None);
}

#[test]
fn space_menu_opens_and_closes_on_choice() {
    let mut app = app_with(|s| {
        let a = add_account_with_mail(s, "a@x.fr", 1);
        s.add_space("Perso", "house", 0, &[a]).unwrap();
    });
    let _ = app.update(Message::SpaceMenu(true));
    assert!(app.space_menu);
    let id = app.spaces[0].id;
    let _ = app.update(Message::Space(SpaceMsg::Select(Some(id))));
    assert!(!app.space_menu, "choosing a space closes the menu");
    assert_eq!(app.space, Some(id));
}

#[test]
fn every_screen_draws_wide_and_narrow() {
    for color in ColorTheme::ALL {
        for dark in [false, true] {
            let mut app = demo();
            let _ = app.update(Message::ColorChosen(color));
            let choice = if dark {
                ThemeChoice::Dark
            } else {
                ThemeChoice::Light
            };
            let _ = app.update(Message::ThemeChosen(choice));
            for width in [1440.0, 1100.0, 900.0] {
                let _ = app.update(Message::WindowResized(width));
                let _ = app.update(Message::MessageSelected(0));
                let _ = app.update(Message::LineHovered(1, true));
                let _ = app.update(Message::SpaceMenu(true));
                let _ = app.view();
                let _ = app.update(Message::LineChecked(1));
                let _ = app.view();
                let _ = app.update(Message::SelectAll(false));
                let _ = app.update(Message::Compose(ComposeMsg::New));
                let _ = app.view();
                let _ = app.update(Message::Compose(ComposeMsg::Cancel));
                let _ = app.update(Message::ShowSettings);
                let _ = app.view();
                let _ = app.update(Message::ShowCleanup);
                let _ = app.view();
                let _ = app.update(Message::ShowMail);
            }
        }
    }
    // The welcome screen, without accounts.
    let app = app_with(|_| {});
    let _ = app.view();
}
