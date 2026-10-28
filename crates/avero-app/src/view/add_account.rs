//! The add-account card (welcome screen and rail "+").

use super::{field_label, icon_button};
use crate::account_form::{AccountForm, Field, Stage};
use crate::app::{AddMsg, App, Message};
use crate::auth;
use crate::icons::{self, Icon, Tone};
use crate::theme::{self, MEDIUM, SEMIBOLD};
use avero_core::Security;
use avero_oauth::Provider;
use iced::widget::{Column, button, column, container, row, scrollable, text, text_input};
use iced::{Alignment, Element, Fill};

/// Width of the card.
pub const WIDTH: f32 = 460.0;

pub fn view<'a>(app: &'a App, form: &'a AccountForm) -> Element<'a, Message> {
    let header = row![
        text("Ajouter un compte")
            .size(20)
            .font(SEMIBOLD)
            .width(Fill),
        icon_button(
            Icon::Close,
            "Fermer (Échap)",
            Some(Message::Add(AddMsg::Close))
        ),
    ]
    .align_y(Alignment::Center);

    let mut card = Column::new().spacing(18).push(header);
    if let Some(note) = &form.note {
        card = card.push(note_box(note.clone()));
    }
    card = card.push(match form.stage {
        Stage::Email => email_step(form),
        Stage::Detecting => waiting(app, &form.email, "Recherche des serveurs…"),
        Stage::Credentials => credentials_step(form),
        Stage::Checking => waiting(app, &form.email, "Vérification de la connexion…"),
        Stage::Authorizing(provider) => authorizing(app, provider),
    });

    // Scrolls when the window is too short for the advanced settings.
    container(
        scrollable(container(card).padding(28))
            .style(theme::scrollbar)
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::new()
                    .width(4)
                    .scroller_width(4)
                    .margin(3),
            )),
    )
    .width(WIDTH)
    .style(theme::card)
    .into()
}

fn email_step(form: &AccountForm) -> Element<'_, Message> {
    let input = text_input("vous@exemple.fr", &form.email)
        .id(crate::app::EMAIL_ID)
        .on_input(|v| Message::Add(AddMsg::Field(Field::Email, v)))
        .on_submit(Message::Add(AddMsg::SubmitEmail))
        .padding([10, 12])
        .size(14)
        .style(theme::input);
    let mut step = column![
        text("Saisissez votre adresse : Avero trouve les réglages tout seul.")
            .size(13)
            .style(theme::muted),
        column![field_label("Adresse e-mail"), input].spacing(6),
    ]
    .spacing(14);
    if let Some(error) = &form.error {
        step = step.push(error_line(error));
    }
    step.push(
        button(container(text("Continuer").size(14).font(MEDIUM)).center_x(Fill))
            .width(Fill)
            .padding([10, 16])
            .on_press(Message::Add(AddMsg::SubmitEmail))
            .style(theme::primary),
    )
    .into()
}

/// The detected provider, the address and "Modifier".
fn identity(form: &AccountForm) -> Element<'_, Message> {
    let provider = form
        .provider_name()
        .map_or("Configuration manuelle".to_string(), |n| n.to_string());
    container(
        row![
            icons::icon(Icon::Globe, 18.0, Tone::Accent),
            column![
                text(form.email.trim().to_string()).size(14).font(SEMIBOLD),
                text(provider).size(12).style(theme::muted),
            ]
            .spacing(2)
            .width(Fill),
            button(text("Modifier").size(13).font(MEDIUM))
                .padding([4, 8])
                .on_press(Message::Add(AddMsg::Back))
                .style(theme::link),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .padding([10, 14])
    .style(theme::note)
    .into()
}

fn credentials_step(form: &AccountForm) -> Element<'_, Message> {
    let mut step = Column::new().spacing(16).push(identity(form));
    if form.offers_oauth()
        && let Some(provider) = form.oauth_provider()
    {
        step = step.push(oauth_button(provider));
        if form.allows_password() {
            step = step.push(
                container(
                    button(text("Utiliser plutôt un mot de passe d'application").size(13))
                        .on_press(Message::Add(AddMsg::UsePassword))
                        .style(theme::link),
                )
                .center_x(Fill),
            );
        }
        if let Some(error) = &form.error {
            step = step.push(error_line(error));
        }
        return step.into();
    }
    if let Some(note) = form.oauth_missing_note() {
        step = step.push(note_box(note));
    }
    if !form.shows_password() {
        return step.into();
    }

    let label = if form.app_password() {
        "Mot de passe d'application"
    } else {
        "Mot de passe"
    };
    let password = text_input("••••••••", &form.password)
        .id(crate::app::PASSWORD_ID)
        .secure(true)
        .on_input(|v| Message::Add(AddMsg::Field(Field::Password, v)))
        .on_submit(Message::Add(AddMsg::SubmitPassword))
        .padding([10, 12])
        .size(14)
        .style(theme::input);
    step = step.push(column![field_label(label), password].spacing(6));

    if let Some(help) = form
        .provider
        .as_ref()
        .and_then(|p| p.password_help.as_ref())
    {
        step = step.push(
            container(
                column![
                    text(help.text.clone()).size(12),
                    button(
                        row![
                            text("Ouvrir la page").size(12).font(MEDIUM),
                            icons::icon(Icon::Globe, 12.0, Tone::Accent),
                        ]
                        .spacing(6)
                        .align_y(Alignment::Center)
                    )
                    .padding(0)
                    .on_press(Message::OpenUrl(help.url.clone()))
                    .style(theme::link),
                ]
                .spacing(8),
            )
            .padding(12)
            .width(Fill)
            .style(theme::note),
        );
    }

    step = step.push(advanced(form));
    if let Some(error) = &form.error {
        step = step.push(error_line(error));
    }
    step.push(
        button(container(text("Se connecter").size(14).font(MEDIUM)).center_x(Fill))
            .width(Fill)
            .padding([10, 16])
            .on_press(Message::Add(AddMsg::SubmitPassword))
            .style(theme::primary),
    )
    .into()
}

fn oauth_button<'a>(provider: Provider) -> Element<'a, Message> {
    let logo = match provider {
        Provider::Google => Icon::Google,
        Provider::Microsoft => Icon::Microsoft,
    };
    button(
        container(
            row![
                icons::icon(logo, 20.0, Tone::Original),
                text(format!("Continuer avec {}", auth::provider_name(provider)))
                    .size(15)
                    .font(MEDIUM),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        )
        .center_x(Fill),
    )
    .width(Fill)
    .padding([12, 16])
    .on_press(Message::Add(AddMsg::SignIn(provider)))
    .style(theme::provider_button)
    .into()
}

fn advanced(form: &AccountForm) -> Element<'_, Message> {
    // Pointing right when closed, down when open.
    let turn = if form.advanced {
        0.0
    } else {
        -std::f32::consts::FRAC_PI_2
    };
    let toggle = button(
        row![
            text("Paramètres avancés").size(13).font(MEDIUM),
            icons::icon_rotated(Icon::ChevronDown, 14.0, Tone::Muted, turn),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
    .padding([2, 0])
    .on_press(Message::Add(AddMsg::ToggleAdvanced))
    .style(theme::link);
    if !form.advanced {
        return toggle.into();
    }
    let field = |label: &'static str, field: Field, value: &str, width: f32| {
        column![
            field_label(label),
            text_input("", value)
                .on_input(move |v| Message::Add(AddMsg::Field(field, v)))
                .padding([8, 10])
                .size(13)
                .style(theme::input),
        ]
        .spacing(4)
        .width(if width > 0.0 {
            iced::Length::Fixed(width)
        } else {
            Fill
        })
    };
    let security = |label: &'static str, value: Security| {
        button(text(label).size(12).font(MEDIUM))
            .padding([6, 10])
            .on_press(Message::Add(AddMsg::SmtpSecurity(value)))
            .style(theme::chip(form.smtp_security == value))
    };
    column![
        toggle,
        field("Identifiant", Field::Username, &form.username, 0.0),
        row![
            field("Serveur IMAP (TLS)", Field::ImapHost, &form.imap_host, 0.0),
            field("Port", Field::ImapPort, &form.imap_port, 80.0),
        ]
        .spacing(10),
        row![
            field("Serveur SMTP", Field::SmtpHost, &form.smtp_host, 0.0),
            field("Port", Field::SmtpPort, &form.smtp_port, 80.0),
        ]
        .spacing(10),
        row![
            text("Chiffrement SMTP")
                .size(12)
                .style(theme::muted)
                .width(Fill),
            security("TLS (465)", Security::Tls),
            security("STARTTLS (587)", Security::StartTls),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    ]
    .spacing(12)
    .into()
}

fn waiting<'a>(app: &'a App, email: &'a str, label: &'a str) -> Element<'a, Message> {
    column![
        text(email.trim().to_string()).size(14).font(SEMIBOLD),
        spinner_line(app, label),
        button(container(text("Annuler").size(14).font(MEDIUM)).center_x(Fill))
            .width(Fill)
            .padding([10, 16])
            .on_press(Message::Add(AddMsg::CancelWait))
            .style(theme::secondary),
    ]
    .spacing(16)
    .into()
}

fn authorizing<'a>(app: &'a App, provider: Provider) -> Element<'a, Message> {
    let name = auth::provider_name(provider);
    column![
        spinner_line(app, "Autorisez Avero dans votre navigateur…"),
        text(format!(
            "Une page {name} s'est ouverte dans votre navigateur. Connectez-vous et acceptez l'accès, puis revenez ici : la suite est automatique."
        ))
        .size(13)
        .style(theme::muted),
        button(container(text("Annuler").size(14).font(MEDIUM)).center_x(Fill))
            .width(Fill)
            .padding([10, 16])
            .on_press(Message::Add(AddMsg::CancelWait))
            .style(theme::secondary),
    ]
    .spacing(16)
    .into()
}

/// A turning loader with a label.
pub fn spinner_line<'a>(app: &App, label: &'a str) -> Element<'a, Message> {
    row![
        icons::icon_rotated(Icon::Loader, 18.0, Tone::Accent, app.spinner),
        text(label).size(14).font(MEDIUM),
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .into()
}

fn note_box<'a>(note: String) -> Element<'a, Message> {
    container(
        row![
            icons::icon(Icon::Alert, 16.0, Tone::Accent),
            text(note).size(12),
        ]
        .spacing(10),
    )
    .padding(12)
    .width(Fill)
    .style(theme::note)
    .into()
}

/// An error message in red with an icon.
pub fn error_line(error: &str) -> Element<'_, Message> {
    row![
        icons::icon(Icon::Alert, 15.0, Tone::Danger),
        text(error.to_string()).size(13).style(theme::danger_text),
    ]
    .spacing(8)
    .into()
}
