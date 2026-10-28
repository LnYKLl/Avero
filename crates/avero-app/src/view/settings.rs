//! The settings page: accounts, appearance, about.

use super::{dot, hr, tip, to_color};
use crate::app::{AccountMsg, AddMsg, App, Message};
use crate::icons::{self, Icon, Tone};
use crate::settings::ThemeChoice;
use crate::theme::{self, MEDIUM, SEMIBOLD};
use avero_core::{Account, AuthKind};
use iced::widget::{Column, button, column, container, row, scrollable, space, text, tooltip};
use iced::{Alignment, Element, Fill};

/// Width of the settings column.
const MAX_WIDTH: f32 = 720.0;

/// Project page.
pub const REPOSITORY: &str = "https://github.com/LnYKLl/Avero";

pub fn view(app: &App) -> Element<'_, Message> {
    let page = column![
        text("Réglages").size(26).font(SEMIBOLD),
        section("Comptes", accounts(app)),
        section("Apparence", appearance(app)),
        section("À propos", about()),
    ]
    .spacing(28)
    .max_width(MAX_WIDTH);
    container(
        scrollable(container(page).padding([36, 40]).center_x(Fill))
            .height(Fill)
            .style(theme::scrollbar),
    )
    .width(Fill)
    .height(Fill)
    .style(theme::paper)
    .into()
}

fn section<'a>(title: &'a str, body: Element<'a, Message>) -> Element<'a, Message> {
    column![
        text(title).size(13).font(SEMIBOLD).style(theme::muted),
        container(body).width(Fill).style(theme::panel),
    ]
    .spacing(10)
    .into()
}

fn accounts(app: &App) -> Element<'_, Message> {
    let mut list = Column::new();
    for (index, account) in app.accounts.iter().enumerate() {
        if index > 0 {
            list = list.push(hr());
        }
        list = list.push(account_row(app, account));
    }
    list = list.push(hr()).push(
        container(
            button(
                row![
                    icons::icon(Icon::Plus, 16.0, Tone::Accent),
                    text("Ajouter un compte").size(13).font(MEDIUM),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .padding([6, 8])
            .on_press(Message::Add(AddMsg::Open(None)))
            .style(theme::link),
        )
        .padding([8, 12]),
    );
    list.into()
}

fn auth_label(auth: AuthKind) -> &'static str {
    match auth {
        AuthKind::Password => "Mot de passe",
        AuthKind::OAuthGoogle => "Connexion Google",
        AuthKind::OAuthMicrosoft => "Connexion Microsoft",
    }
}

fn account_row<'a>(app: &'a App, account: &'a Account) -> Element<'a, Message> {
    let id = account.id;
    let (secret_icon, secret_label, secret_action) = match account.auth {
        AuthKind::Password => (
            Icon::Key,
            "Changer le mot de passe",
            AccountMsg::OpenPassword(id),
        ),
        _ => (Icon::Refresh, "Reconnecter", AccountMsg::Reconnect(id)),
    };
    let mut details = column![
        row![
            text(account.name.clone()).size(14).font(SEMIBOLD),
            container(text(auth_label(account.auth)).size(11).font(MEDIUM))
                .padding([2, 8])
                .style(theme::note),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        text(account.email.clone()).size(12).style(theme::muted),
    ]
    .spacing(3)
    .width(Fill);
    if let Some(error) = app.status.get(&id).and_then(|s| s.error.as_ref()) {
        details = details.push(
            text(error.message.clone())
                .size(12)
                .style(theme::danger_text),
        );
    }
    let action = |icon, label: &'a str, message: AccountMsg, tone| {
        tip(
            button(icons::icon(icon, 17.0, tone))
                .padding(8)
                .on_press(Message::Account(message))
                .style(theme::ghost),
            label,
            tooltip::Position::Bottom,
        )
    };
    row![
        dot(to_color(account.color), 12.0),
        details,
        action(
            Icon::Pencil,
            "Renommer",
            AccountMsg::StartRename(id),
            Tone::Text
        ),
        action(secret_icon, secret_label, secret_action, Tone::Text),
        action(
            Icon::Trash,
            "Supprimer",
            AccountMsg::AskDelete(id),
            Tone::Danger
        ),
    ]
    .spacing(12)
    .padding([14, 18])
    .align_y(Alignment::Center)
    .into()
}

fn appearance(app: &App) -> Element<'_, Message> {
    let choice = |icon, label: &'static str, value: ThemeChoice| {
        let selected = app.settings.theme == value;
        button(
            container(
                column![
                    icons::icon(
                        icon,
                        22.0,
                        if selected { Tone::Accent } else { Tone::Muted }
                    ),
                    text(label).size(13).font(MEDIUM),
                ]
                .spacing(8)
                .align_x(Alignment::Center),
            )
            .center_x(Fill),
        )
        .width(Fill)
        .padding([16, 12])
        .on_press(Message::ThemeChosen(value))
        .style(theme::choice(selected))
    };
    column![
        text("Thème de l'interface").size(14).font(MEDIUM),
        row![
            choice(Icon::Settings, "Système", ThemeChoice::System),
            choice(Icon::Sun, "Clair", ThemeChoice::Light),
            choice(Icon::Moon, "Sombre", ThemeChoice::Dark),
        ]
        .spacing(10),
    ]
    .spacing(12)
    .padding(18)
    .into()
}

fn about<'a>() -> Element<'a, Message> {
    row![
        icons::icon(Icon::Logo, 44.0, Tone::Original),
        column![
            text(format!("Avero {}", env!("CARGO_PKG_VERSION")))
                .size(15)
                .font(SEMIBOLD),
            text("Client mail léger, privé et open source.")
                .size(13)
                .style(theme::muted),
            text("Licence GPL-3.0-or-later. Polices Inter (OFL), icônes Lucide (ISC).")
                .size(12)
                .style(theme::muted),
            button(text("github.com/LnYKLl/Avero").size(13).font(MEDIUM))
                .padding(0)
                .on_press(Message::OpenUrl(REPOSITORY.to_string()))
                .style(theme::link),
        ]
        .spacing(4),
        space().width(Fill),
    ]
    .spacing(16)
    .padding(18)
    .align_y(Alignment::Center)
    .into()
}
