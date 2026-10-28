//! The one-time assistant "Configurer la connexion Google / Microsoft",
//! drawn inside the add-account card.

use super::add_account::error_line;
use super::field_label;
use crate::account_form::{AccountForm, Field};
use crate::app::{AddMsg, Message};
use crate::auth;
use crate::icons::{self, Icon, Tone};
use crate::oauth_setup::{self, Action, Step};
use crate::theme::{self, MEDIUM, SEMIBOLD};
use avero_oauth::Provider;
use iced::widget::{Column, Row, button, column, container, row, text, text_input};
use iced::{Alignment, Element, Fill};

/// Id of the client id field.
pub const CLIENT_ID: &str = "setup-client-id";

pub fn view(form: &AccountForm, provider: Provider) -> Element<'_, Message> {
    let name = auth::provider_name(provider);
    let logo = match provider {
        Provider::Google => Icon::Google,
        Provider::Microsoft => Icon::Microsoft,
    };
    let title = row![
        container(icons::icon(logo, 22.0, Tone::Original))
            .padding(9)
            .style(theme::note),
        column![
            text(format!("Configurer la connexion {name}"))
                .size(15)
                .font(SEMIBOLD),
            text("Une seule fois, environ 5 minutes")
                .size(12)
                .style(theme::muted),
        ]
        .spacing(2),
    ]
    .spacing(12)
    .align_y(Alignment::Center);

    let why = match provider {
        Provider::Google => {
            "Google demande que chaque application de messagerie soit enregistrée par son \
             propriétaire. Faites-le une fois avec votre compte Google : ensuite, chaque compte \
             Gmail se connecte en un clic."
        }
        Provider::Microsoft => {
            "Microsoft demande que chaque application de messagerie soit inscrite par son \
             propriétaire. Faites-le une fois avec votre compte Microsoft : ensuite, chaque \
             compte Outlook se connecte en un clic."
        }
    };

    let mut steps = Column::new().spacing(14);
    for (index, step) in oauth_setup::steps(provider, &form.setup.client_id)
        .into_iter()
        .enumerate()
    {
        steps = steps.push(step_row(index + 1, step));
    }

    let guide = column![title, text(why).size(13).style(theme::muted), steps]
        .spacing(18)
        .width(Fill);

    let mut body = Column::new()
        .spacing(16)
        .push(
            column![
                text("Collez vos identifiants").size(14).font(SEMIBOLD),
                text(match provider {
                    Provider::Google => "Affichés à la fin de l'étape 5.",
                    Provider::Microsoft =>
                        "Affiché sur la page « Vue d'ensemble » de l'application.",
                })
                .size(12)
                .style(theme::muted),
            ]
            .spacing(3),
        )
        .push(fields(form, provider));
    if let Some(error) = &form.error {
        body = body.push(error_line(error));
    }
    let label = if form.setup_only {
        "Enregistrer"
    } else {
        "Enregistrer et se connecter"
    };
    body = body.push(
        button(container(text(label).size(14).font(MEDIUM)).center_x(Fill))
            .width(Fill)
            .padding([10, 16])
            .on_press_maybe(
                form.setup
                    .is_complete(provider)
                    .then_some(Message::Add(AddMsg::SaveSetup)),
            )
            .style(theme::primary),
    );
    if !form.setup_only {
        let (alternative, message) = match provider {
            Provider::Google => (
                "Utiliser plutôt un mot de passe d'application",
                AddMsg::UsePassword,
            ),
            Provider::Microsoft => ("Utiliser une autre adresse", AddMsg::Back),
        };
        body = body.push(
            button(
                text(alternative)
                    .size(13)
                    .width(Fill)
                    .align_x(Alignment::Center),
            )
            .width(Fill)
            .padding([2, 0])
            .on_press(Message::Add(message))
            .style(theme::link),
        );
    }
    body = body.push(
        row![
            icons::icon(Icon::Shield, 13.0, Tone::Muted),
            text("Ils restent sur cet ordinateur (fichier oauth.json).")
                .size(11)
                .style(theme::muted),
        ]
        .spacing(6),
    );
    row![
        guide,
        container(body)
            .padding(18)
            .width(FORM_WIDTH)
            .style(theme::note),
    ]
    .spacing(24)
    .into()
}

/// Width of the column with the fields.
const FORM_WIDTH: f32 = 290.0;

fn step_row<'a>(number: usize, step: Step) -> Element<'a, Message> {
    let badge = container(text(number.to_string()).size(12).font(SEMIBOLD))
        .center_x(24)
        .center_y(24)
        .style(theme::step_badge);
    let mut actions = Row::new().spacing(8);
    for action in step.actions {
        actions = actions.push(action_button(action));
    }
    row![
        badge,
        column![
            text(step.title).size(14).font(SEMIBOLD),
            text(step.detail).size(12).style(theme::muted),
            actions,
        ]
        .spacing(6)
        .width(Fill),
    ]
    .spacing(12)
    .into()
}

fn action_button<'a>(action: Action) -> Element<'a, Message> {
    let (label, icon, message) = match action {
        Action::Open(label, url) => (label, Icon::ExternalLink, Message::OpenUrl(url)),
        Action::Copy(label, value) => (label, Icon::Copy, Message::CopyText(value.to_string())),
    };
    button(
        row![
            text(label).size(12).font(MEDIUM),
            icons::icon(icon, 13.0, Tone::Accent),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
    .padding([5, 10])
    .on_press(message)
    .style(theme::secondary)
    .into()
}

fn fields(form: &AccountForm, provider: Provider) -> Element<'_, Message> {
    let (id_label, id_placeholder) = match provider {
        Provider::Google => ("ID client", "xxxx.apps.googleusercontent.com"),
        Provider::Microsoft => ("ID d'application (client)", "00000000-0000-0000-…"),
    };
    let id_input = text_input(id_placeholder, &form.setup.client_id)
        .id(CLIENT_ID)
        .on_input(|v| Message::Add(AddMsg::Field(Field::ClientId, v)))
        .on_submit(Message::Add(AddMsg::SaveSetup))
        .padding([10, 12])
        .size(13)
        .style(theme::input);
    let id_ok = avero_oauth::check_client_id(provider, &form.setup.client_id).is_ok();
    let mut column = Column::new().spacing(14).push(
        column![
            field_label(id_label),
            id_input,
            feedback(form.setup.id_error(provider), id_ok, "ID client reconnu"),
        ]
        .spacing(6),
    );
    if provider == Provider::Google {
        let secret_input = text_input("GOCSPX-…", &form.setup.client_secret)
            .secure(true)
            .on_input(|v| Message::Add(AddMsg::Field(Field::ClientSecret, v)))
            .on_submit(Message::Add(AddMsg::SaveSetup))
            .padding([10, 12])
            .size(13)
            .style(theme::input);
        let secret_ok = avero_oauth::check_client_secret(&form.setup.client_secret).is_ok();
        column = column.push(
            column![
                field_label("Code secret du client"),
                secret_input,
                feedback(form.setup.secret_error(), secret_ok, "Code secret saisi"),
            ]
            .spacing(6),
        );
    }
    column.into()
}

/// The line under a field: what is wrong, a confirmation, or nothing.
fn feedback<'a>(error: Option<&'static str>, ok: bool, done: &'a str) -> Element<'a, Message> {
    if let Some(error) = error {
        return row![
            icons::icon(Icon::Alert, 13.0, Tone::Danger),
            text(error).size(12).style(theme::danger_text),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .into();
    }
    if ok {
        return row![
            icons::icon(Icon::Check, 13.0, Tone::Fixed(theme::LIGHT.success)),
            text(done).size(12).style(theme::success_text),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .into();
    }
    iced::widget::space().height(0).into()
}
