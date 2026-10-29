//! The ink rail on the left: logo, Courrier, Rechercher, Écrire, then
//! Réglages and "Ajouter un compte" at the bottom.

use super::tip;
use crate::app::{AddMsg, App, ComposeMsg, Message, Screen};
use crate::icons::{self, Icon, Tone};
use crate::theme;
use iced::widget::{button, center, column, container, space, tooltip};
use iced::{Alignment, Color, Element, Fill};

/// Width of the rail, in pixels.
pub const WIDTH: f32 = 64.0;

pub fn view(app: &App) -> Element<'_, Message> {
    let mail = app.screen == Screen::Mail && app.composer.is_none();
    let writing = app.screen == Screen::Mail && app.composer.is_some();
    let settings = app.screen == Screen::Settings;
    let rail = column![
        container(icons::icon(Icon::Logo, 36.0, Tone::Original)).padding([4, 0]),
        space().height(14),
        item(Icon::Inbox, "Courrier", mail, Message::ShowMail),
        item(
            Icon::Search,
            "Rechercher (Ctrl+F)",
            false,
            Message::Key(
                iced::keyboard::Key::Character("f".into()),
                iced::keyboard::Modifiers::COMMAND,
            ),
        ),
        item(
            Icon::Compose,
            "Écrire (Ctrl+N)",
            writing,
            Message::Compose(ComposeMsg::New),
        ),
        space().height(Fill),
        item(Icon::Settings, "Réglages", settings, Message::ShowSettings),
        item(
            Icon::Plus,
            "Ajouter un compte",
            false,
            Message::Add(AddMsg::Open(None)),
        ),
    ]
    .spacing(6)
    .padding([16, 0])
    .align_x(Alignment::Center);

    container(rail)
        .width(WIDTH)
        .height(Fill)
        .style(theme::rail)
        .into()
}

/// One rail button with its tooltip on the right.
fn item<'a>(icon: Icon, label: &'a str, active: bool, on_press: Message) -> Element<'a, Message> {
    let tone = if active {
        Tone::Fixed(Color::WHITE)
    } else {
        Tone::Rail
    };
    let button = button(center(icons::icon(icon, 20.0, tone)))
        .width(42)
        .height(42)
        .padding(0)
        .on_press(on_press)
        .style(theme::rail_button(active));
    tip(button, label, tooltip::Position::Right)
}
