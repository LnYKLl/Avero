//! The welcome screen shown while there is no account.

use crate::app::{AddMsg, App, Message};
use crate::icons::{self, Icon, Tone};
use crate::theme::{self, BOLD, MEDIUM};
use avero_oauth::Provider;
use iced::widget::{button, center, column, container, row, space, text};
use iced::{Alignment, Element, Fill};

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
        provider_button(
            Icon::Google,
            "Continuer avec Google",
            Some(Provider::Google)
        ),
        space().height(10),
        provider_button(
            Icon::Microsoft,
            "Continuer avec Microsoft",
            Some(Provider::Microsoft)
        ),
        space().height(18),
        row![
            divider(),
            text("ou").size(12).style(theme::muted),
            divider()
        ]
        .spacing(12)
        .align_y(Alignment::Center)
        .width(BUTTON_WIDTH),
        space().height(18),
        provider_button(Icon::Mail, "Autre adresse e-mail", None),
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
    center(content).style(theme::paper).into()
}

fn divider<'a>() -> Element<'a, Message> {
    container(super::hr()).width(Fill).into()
}

fn provider_button<'a>(
    icon: Icon,
    label: &'a str,
    provider: Option<Provider>,
) -> Element<'a, Message> {
    let tone = if provider.is_some() {
        Tone::Original
    } else {
        Tone::Text
    };
    button(
        row![
            icons::icon(icon, 20.0, tone),
            text(label).size(15).font(MEDIUM),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .width(BUTTON_WIDTH)
    .padding([13, 20])
    .on_press(Message::Add(AddMsg::Open(provider)))
    .style(theme::provider_button)
    .into()
}
