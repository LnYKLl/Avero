//! The welcome screen shown while there is no account.

use crate::app::{AddMsg, App, Message};
use crate::icons::{self, Icon, Tone};
use crate::theme::{self, BOLD, MEDIUM};
use iced::widget::{button, column, container, row, space, text};
use iced::{Alignment, Element};

/// Width of the sign-in buttons.
const BUTTON_WIDTH: f32 = 340.0;

pub fn view(app: &App) -> Element<'_, Message> {
    let mut content = column![
        icons::icon(Icon::Logo, 76.0, Tone::Original),
        space().height(22),
        text("Avero").size(40).font(BOLD),
        space().height(8),
        text("Tous vos e-mails, un seul endroit.")
            .size(17)
            .font(MEDIUM),
        text("Léger, privé, open source.")
            .size(15)
            .style(theme::muted),
        space().height(36),
        button(
            row![
                icons::icon(Icon::Mail, 20.0, Tone::Text),
                text("Ajouter une adresse e-mail").size(15).font(MEDIUM),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        )
        .width(BUTTON_WIDTH)
        .padding([13, 20])
        .on_press(Message::Add(AddMsg::Open(None)))
        .style(theme::provider_button),
        space().height(10),
        text("Gmail, iCloud, Yahoo, Orange, Free, Outlook…")
            .size(12)
            .style(theme::muted),
        space().height(36),
        text("Vos messages restent sur cet ordinateur et chez votre fournisseur de messagerie.")
            .size(12)
            .style(theme::muted)
            .align_x(Alignment::Center),
    ]
    .align_x(Alignment::Center)
    .max_width(460);
    if app.is_demo() {
        content = content.push(space().height(12)).push(
            container(
                text("Mode démo : aucune connexion réseau")
                    .size(12)
                    .font(MEDIUM),
            )
            .padding([4, 10])
            .style(theme::note),
        );
    }
    // Drawn in the window's card (see `view::app`).
    container(content).padding([40, 56]).into()
}
