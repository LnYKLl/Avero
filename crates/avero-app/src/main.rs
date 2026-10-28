// In release builds on Windows, don't open a console window next to the app.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod account_form;
mod app;
mod auth;
mod background;
mod compose;
mod demo;
mod icons;
mod paths;
mod settings;
mod theme;
mod view;
mod worker;

use app::{App, Backend, Message};
use avero_core::secrets::KeyringSecrets;
use avero_core::store::Store;
use iced::{Size, Task, window};
use settings::{Settings, ThemeChoice};
use std::collections::HashMap;
use std::sync::Arc;

fn main() -> iced::Result {
    // Without a console (release builds), a database that cannot be
    // opened must be explained in a window, not by a silent crash.
    if demo::from_env_value(std::env::var("AVERO_DEMO").ok().as_deref()).is_none() {
        let db_path = paths::database_path();
        if let Err(e) = Store::open(&db_path) {
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title("Avero")
                .set_description(database_error_text(&db_path, &e))
                .set_buttons(rfd::MessageButtons::Ok)
                .show();
            std::process::exit(1);
        }
    }
    let mut application = iced::application(boot, App::update, App::view)
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

/// Opens the database (or the demo one) and builds the window's state.
fn boot() -> (App, Task<Message>) {
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

    match demo {
        Some(kind) => {
            let store = match kind {
                demo::DemoKind::Seeded => demo::seeded_store(),
                demo::DemoKind::Empty => Store::open_in_memory().expect("in-memory database"),
            };
            // Demo mode never writes the settings file.
            App::new(store, Backend::Demo, settings, None)
        }
        None => {
            let db_path = paths::database_path();
            // Checked in `main` before the window opened.
            let store = Store::open(&db_path).expect("open database");
            let backend = Backend::Live {
                db_path,
                secrets: Arc::new(KeyringSecrets),
                config_dir,
                workers: HashMap::new(),
            };
            // An AVERO_THEME override is not saved either.
            let path = forced.is_none().then_some(settings_path);
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
    for pixel in data.chunks_exact_mut(4) {
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
    fn unpremultiply_restores_colour() {
        assert_eq!(unpremultiply(vec![64, 0, 0, 128]), vec![128, 0, 0, 128]);
        assert_eq!(unpremultiply(vec![1, 2, 3, 0]), vec![1, 2, 3, 0]);
        assert_eq!(unpremultiply(vec![9, 8, 7, 255]), vec![9, 8, 7, 255]);
    }
}
