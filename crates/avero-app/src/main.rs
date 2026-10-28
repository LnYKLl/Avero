// In release builds on Windows, don't open a console window next to the app.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod account_form;
mod app;
mod auth;
mod background;
mod compose;
mod demo;
mod icons;
mod later;
mod notify;
mod oauth_setup;
mod paths;
mod settings;
mod theme;
mod unsubscribe;
mod view;
mod worker;

use app::{AddMsg, App, Backend, Message};
use avero_core::secrets::KeyringSecrets;
use avero_core::store::Store;
use iced::{Size, Task, window};
use settings::{Settings, ThemeChoice};
use std::collections::HashMap;
use std::sync::Arc;

fn main() -> iced::Result {
    // Windows: before any window, so the taskbar and the notifications
    // know the process as Avero.
    notify::set_process_app_id();
    // Without a console (release builds), a database that cannot be
    // opened must be explained in a window, not by a silent crash. It is
    // opened here once, and that connection goes to the window's state:
    // opening migrates the database, and what the migration leaves for
    // the app (see `Store::merged_accounts`) is read from it.
    let mut opened = None;
    if demo::from_env_value(std::env::var("AVERO_DEMO").ok().as_deref()).is_none() {
        let db_path = paths::database_path();
        match Store::open(&db_path) {
            Ok(store) => opened = Some(store),
            Err(e) => {
                rfd::MessageDialog::new()
                    .set_level(rfd::MessageLevel::Error)
                    .set_title("Avero")
                    .set_description(database_error_text(&db_path, &e))
                    .set_buttons(rfd::MessageButtons::Ok)
                    .show();
                std::process::exit(1);
            }
        }
    }
    // `boot` may in theory be called more than once: the next calls open
    // the database again.
    let opened = std::cell::Cell::new(opened);
    let mut application = iced::application(move || boot(opened.take()), App::update, App::view)
        .title("Avero")
        .theme(App::theme)
        .subscription(App::subscription)
        .default_font(theme::INTER)
        .window(window::Settings {
            size: Size::new(1280.0, 800.0),
            min_size: Some(Size::new(900.0, 600.0)),
            icon: window_icon(),
            ..window::Settings::default()
        });
    for font in theme::FONT_FILES {
        application = application.font(font);
    }
    application.run()
}

/// Builds the window's state on the database `main` opened (or the demo
/// one).
fn boot(opened: Option<Store>) -> (App, Task<Message>) {
    let demo = demo::from_env_value(std::env::var("AVERO_DEMO").ok().as_deref());
    let config_dir = paths::config_dir();
    let settings_path = settings::path_in(&config_dir);
    let mut settings = Settings::load(&settings_path);
    let forced = std::env::var("AVERO_THEME")
        .ok()
        .and_then(|v| ThemeChoice::from_env_value(&v));
    if let Some(choice) = forced {
        settings.theme = choice;
    }
    // AVERO_COLOR (avero, orange, ocean) picks the colours, for screenshots.
    let forced_color = std::env::var("AVERO_COLOR")
        .ok()
        .and_then(|v| settings::ColorTheme::from_env_value(&v));
    if let Some(color) = forced_color {
        settings.color = color;
    }

    match demo {
        Some(kind) => {
            let store = match kind {
                demo::DemoKind::Seeded => demo::seeded_store(),
                demo::DemoKind::Empty | demo::DemoKind::Setup(_) => {
                    Store::open_in_memory().expect("in-memory database")
                }
            };
            // Demo mode never writes the settings file.
            let (mut app, task) = App::new(store, Backend::Demo, settings, None);
            if let demo::DemoKind::Setup(provider) = kind {
                let open = app.update(Message::Add(AddMsg::Open(Some(provider))));
                return (app, Task::batch([task, open]));
            }
            // AVERO_DEMO_SCREEN opens another screen, for screenshots.
            let screen = match std::env::var("AVERO_DEMO_SCREEN").as_deref() {
                Ok("settings") => Some(Message::ShowSettings),
                Ok("cleanup") => Some(Message::ShowCleanup),
                _ => None,
            };
            if let Some(screen) = screen {
                let shown = app.update(screen);
                return (app, Task::batch([task, shown]));
            }
            (app, task)
        }
        None => {
            let db_path = paths::database_path();
            // Checked in `main` before the window opened.
            let store = opened.unwrap_or_else(|| Store::open(&db_path).expect("open database"));
            let backend = Backend::Live {
                db_path,
                secrets: Arc::new(KeyringSecrets),
                config_dir,
                workers: HashMap::new(),
            };
            // An AVERO_THEME or AVERO_COLOR override is not saved either.
            let path = (forced.is_none() && forced_color.is_none()).then_some(settings_path);
            App::new(store, backend, settings, path)
        }
    }
}

/// What the user reads when the database cannot be opened.
fn database_error_text(path: &std::path::Path, error: &dyn std::fmt::Display) -> String {
    format!(
        "Avero n'a pas pu ouvrir sa base de données :\n{}\n\n{error}\n\n\
         Vérifiez que le dossier existe et n'est pas en lecture seule, et qu'Avero \
         n'est pas déjà ouvert.",
        path.display()
    )
}

/// The window icon: `avero-icon.svg` rasterised at startup.
fn window_icon() -> Option<window::Icon> {
    const SIZE: u32 = 128;
    let svg = icons::bytes(icons::Icon::Logo);
    let tree = resvg::usvg::Tree::from_data(svg, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(SIZE, SIZE)?;
    let scale = SIZE as f32 / tree.size().width();
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // tiny-skia stores premultiplied alpha; window icons want straight alpha.
    let rgba = unpremultiply(pixmap.take());
    window::icon::from_rgba(rgba, SIZE, SIZE).ok()
}

fn unpremultiply(mut data: Vec<u8>) -> Vec<u8> {
    for pixel in data.as_chunks_mut::<4>().0 {
        let alpha = u16::from(pixel[3]);
        if alpha > 0 && alpha < 255 {
            for channel in &mut pixel[..3] {
                *channel = ((u16::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_error_names_the_file_and_the_cause() {
        let path = std::path::Path::new("C:/Users/x/AppData/Avero/avero.db");
        let text = database_error_text(path, &"disk I/O error");
        assert!(text.contains("avero.db"), "{text}");
        assert!(text.contains("disk I/O error"), "{text}");
        assert!(text.starts_with("Avero n'a pas pu ouvrir"));
    }

    #[test]
    fn window_icon_is_rendered() {
        assert!(window_icon().is_some());
    }

    #[test]
    fn windows_icon_has_every_size() {
        // Made by `cargo run -p avero-app --example make_icon`.
        let ico = include_bytes!("../assets/logo/avero.ico");
        assert_eq!(&ico[..4], &[0, 0, 1, 0], "an .ico header");
        let count = usize::from(u16::from_le_bytes([ico[4], ico[5]]));
        // Each entry starts with the width; 0 means 256.
        let widths: Vec<u8> = (0..count).map(|i| ico[6 + 16 * i]).collect();
        for width in [16, 24, 32, 48, 0] {
            assert!(widths.contains(&width), "size {width} missing: {widths:?}");
        }
    }

    #[test]
    fn unpremultiply_restores_colour() {
        assert_eq!(unpremultiply(vec![64, 0, 0, 128]), vec![128, 0, 0, 128]);
        assert_eq!(unpremultiply(vec![1, 2, 3, 0]), vec![1, 2, 3, 0]);
        assert_eq!(unpremultiply(vec![9, 8, 7, 255]), vec![9, 8, 7, 255]);
    }
}
