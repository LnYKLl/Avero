// In release builds on Windows, don't open a console window next to the app.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod paths;
mod view;
mod worker;

use app::{App, Message};
use avero_core::secrets::KeyringSecrets;
use avero_core::store::Store;
use iced::Task;

fn main() -> iced::Result {
    iced::application(boot, App::update, App::view)
        .title("Avero")
        .window_size((1200.0, 760.0))
        .run()
}

/// Opens the database and builds the main window's state.
fn boot() -> (App, Task<Message>) {
    let db_path = paths::database_path();
    let store = Store::open(&db_path).expect("open database");
    App::new(store, Box::new(KeyringSecrets), db_path)
}
