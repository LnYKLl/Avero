//! Small dialogs: password, rename, signature, delete confirmation,
//! reconnection, keyboard shortcuts.

use super::add_account::{error_line, spinner_line};
use super::{field_label, icon_button};
use crate::app::{AccountMsg, App, Message, Overlay, PasswordDialog, ServerField, ServersForm};
use crate::icons::Icon;
use crate::theme::{self, MEDIUM, SEMIBOLD};
use avero_core::Security;
use iced::widget::{Column, button, column, container, row, space, text, text_editor, text_input};
use iced::{Alignment, Element, Fill};

/// Width of a small dialog.
const WIDTH: f32 = 420.0;

pub fn view<'a>(app: &'a App, overlay: &'a Overlay) -> Element<'a, Message> {
    match overlay {
        Overlay::AddAccount(form) => super::add_account::view(app, form),
        Overlay::Password(dialog) => password(app, dialog),
        Overlay::Rename { account, name } => rename(app, *account, name),
        Overlay::ConfirmDelete(account) => confirm_delete(app, *account),
        Overlay::Reconnect(account) => reconnect(app, *account),
        Overlay::Servers(form) => servers(app, form),
        Overlay::ConfirmDeleteForever(rows) => confirm_delete_forever(rows.len()),
        Overlay::Signature { account, content } => signature(app, *account, content),
        Overlay::Shortcuts => shortcuts(),
    }
}

fn card<'a>(title: &'a str, body: Element<'a, Message>) -> Element<'a, Message> {
    card_width(title, body, WIDTH)
}

fn card_width<'a>(title: &'a str, body: Element<'a, Message>, width: f32) -> Element<'a, Message> {
    container(
        column![
            row![
                text(title).size(18).font(SEMIBOLD).width(Fill),
                icon_button(
                    Icon::Close,
                    "Fermer (Échap)",
                    Some(Message::Account(AccountMsg::CloseDialog))
                ),
            ]
            .align_y(Alignment::Center),
            body,
        ]
        .spacing(16),
    )
    .width(width)
    .padding(24)
    .style(theme::card)
    .into()
}

fn buttons<'a>(
    confirm: &'a str,
    on_confirm: Option<Message>,
    style: fn(&iced::Theme, button::Status) -> button::Style,
) -> Element<'a, Message> {
    row![
        space().width(Fill),
        button(text("Annuler").size(13).font(MEDIUM))
            .padding([8, 14])
            .on_press(Message::Account(AccountMsg::CloseDialog))
            .style(theme::secondary),
        button(text(confirm).size(13).font(MEDIUM))
            .padding([8, 14])
            .on_press_maybe(on_confirm)
            .style(style),
    ]
    .spacing(8)
    .into()
}

fn password<'a>(app: &'a App, dialog: &'a PasswordDialog) -> Element<'a, Message> {
    let account = app.account(dialog.account);
    let email = account.map(|a| a.email.clone()).unwrap_or_default();
    let mut body = Column::new().spacing(14).push(
        text(format!(
            "Nouveau mot de passe pour {email}. Avero vérifie la connexion avant de l'enregistrer."
        ))
        .size(13)
        .style(theme::muted),
    );
    let input = text_input("Mot de passe", &dialog.password)
        .secure(true)
        .on_input(|v| Message::Account(AccountMsg::PasswordText(v)))
        .on_submit(Message::Account(AccountMsg::SubmitPassword))
        .padding([10, 12])
        .size(14)
        .style(theme::input);
    body = body.push(column![field_label("Mot de passe"), input].spacing(6));
    if let Some(error) = &dialog.error {
        body = body.push(error_line(error));
    }
    if dialog.checking {
        body = body.push(spinner_line(app, "Vérification de la connexion…"));
    }
    body = body.push(buttons(
        "Enregistrer",
        (!dialog.checking).then_some(Message::Account(AccountMsg::SubmitPassword)),
        theme::primary,
    ));
    card("Changer le mot de passe", body.into())
}

fn rename<'a>(app: &'a App, account: avero_core::AccountId, name: &'a str) -> Element<'a, Message> {
    let email = app
        .account(account)
        .map(|a| a.email.clone())
        .unwrap_or_default();
    let input = text_input("Nom", name)
        .on_input(|v| Message::Account(AccountMsg::RenameText(v)))
        .on_submit(Message::Account(AccountMsg::SaveRename))
        .padding([10, 12])
        .size(14)
        .style(theme::input);
    let body = column![
        text(format!(
            "Nom affiché pour {email}. Il sert aussi de nom d'expéditeur dans vos messages."
        ))
        .size(13)
        .style(theme::muted),
        column![field_label("Nom"), input].spacing(6),
        buttons(
            "Renommer",
            (!name.trim().is_empty()).then_some(Message::Account(AccountMsg::SaveRename)),
            theme::primary,
        ),
    ]
    .spacing(14);
    card("Renommer le compte", body.into())
}

fn confirm_delete(app: &App, account: avero_core::AccountId) -> Element<'_, Message> {
    let email = app
        .account(account)
        .map(|a| a.email.clone())
        .unwrap_or_default();
    let body = column![
        text(format!("Supprimer {email} d'Avero ?")).size(14).font(MEDIUM),
        text(
            "Les messages téléchargés et le mot de passe enregistré sur cet ordinateur seront effacés. \
             Rien n'est supprimé sur le serveur : vos e-mails restent dans votre boîte."
        )
        .size(13)
        .style(theme::muted),
        buttons(
            "Supprimer",
            Some(Message::Account(AccountMsg::ConfirmDelete)),
            theme::danger,
        ),
    ]
    .spacing(14);
    card("Supprimer le compte", body.into())
}

fn servers<'a>(app: &'a App, form: &'a ServersForm) -> Element<'a, Message> {
    let email = app
        .account(form.account)
        .map(|a| a.email.clone())
        .unwrap_or_default();
    let field = |label: &'static str, field: ServerField, value: &str, width: f32| {
        column![
            field_label(label),
            text_input("", value)
                .on_input(move |v| Message::Account(AccountMsg::ServerText(field, v)))
                .on_submit(Message::Account(AccountMsg::SaveServers))
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
            .on_press(Message::Account(AccountMsg::ServerSecurity(value)))
            .style(theme::chip(form.smtp_security == value))
    };
    let mut body = Column::new()
        .spacing(12)
        .push(
            text(format!("Serveurs utilisés par {email}."))
                .size(13)
                .style(theme::muted),
        )
        .push(text("Réception (IMAP, TLS)").size(13).font(SEMIBOLD))
        .push(
            row![
                field("Serveur", ServerField::ImapHost, &form.imap_host, 0.0),
                field("Port", ServerField::ImapPort, &form.imap_port, 80.0),
            ]
            .spacing(10),
        )
        .push(text("Envoi (SMTP)").size(13).font(SEMIBOLD))
        .push(
            row![
                field("Serveur", ServerField::SmtpHost, &form.smtp_host, 0.0),
                field("Port", ServerField::SmtpPort, &form.smtp_port, 80.0),
            ]
            .spacing(10),
        )
        .push(
            row![
                text("Chiffrement").size(12).style(theme::muted).width(Fill),
                security("TLS (465)", Security::Tls),
                security("STARTTLS (587)", Security::StartTls),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        );
    if let Some(error) = &form.error {
        body = body.push(error_line(error));
    }
    body = body.push(buttons(
        "Enregistrer",
        Some(Message::Account(AccountMsg::SaveServers)),
        theme::primary,
    ));
    card("Serveurs", body.into())
}

fn confirm_delete_forever<'a>(count: usize) -> Element<'a, Message> {
    let what = if count > 1 {
        "Ces messages sont déjà dans la corbeille."
    } else {
        "Ce message est déjà dans la corbeille."
    };
    let body = column![
        text(what).size(14).font(MEDIUM),
        text("Il sera supprimé définitivement du serveur. Cette action est irréversible.")
            .size(13)
            .style(theme::muted),
        buttons(
            "Supprimer définitivement",
            Some(Message::ConfirmDeleteForever),
            theme::danger,
        ),
    ]
    .spacing(14);
    card("Supprimer définitivement", body.into())
}

fn reconnect(app: &App, account: avero_core::AccountId) -> Element<'_, Message> {
    let email = app
        .account(account)
        .map(|a| a.email.clone())
        .unwrap_or_default();
    let body = column![
        spinner_line(app, "Autorisez Avero dans votre navigateur…"),
        text(format!(
            "Connectez-vous avec {email} dans la page qui s'est ouverte, puis revenez ici."
        ))
        .size(13)
        .style(theme::muted),
        row![
            space().width(Fill),
            button(text("Annuler").size(13).font(MEDIUM))
                .padding([8, 14])
                .on_press(Message::Account(AccountMsg::CloseDialog))
                .style(theme::secondary),
        ],
    ]
    .spacing(14);
    card("Reconnecter le compte", body.into())
}

fn signature<'a>(
    app: &'a App,
    account: avero_core::AccountId,
    content: &'a text_editor::Content,
) -> Element<'a, Message> {
    let email = app
        .account(account)
        .map(|a| a.email.clone())
        .unwrap_or_default();
    let editor = text_editor(content)
        .placeholder("Marie Durand\nPâtisserie Durand · 01 23 45 67 89")
        .on_action(|action| Message::Account(AccountMsg::SignatureEdit(action)))
        .size(14)
        .padding([10, 12])
        .height(140)
        .style(theme::editor);
    let body = column![
        text(format!(
            "Ajoutée sous les messages, réponses et transferts envoyés depuis {email}, \
             après une ligne « -- ». Laissez vide pour ne pas en mettre."
        ))
        .size(13)
        .style(theme::muted),
        column![field_label("Signature"), editor].spacing(6),
        buttons(
            "Enregistrer",
            Some(Message::Account(AccountMsg::SaveSignature)),
            theme::primary,
        ),
    ]
    .spacing(14);
    card_width("Signature", body.into(), 480.0)
}

fn shortcuts<'a>() -> Element<'a, Message> {
    let mut list = Column::new().spacing(8);
    for (keys, action) in crate::app::SHORTCUTS {
        list = list.push(
            row![
                container(text(keys).size(12).font(SEMIBOLD))
                    .padding([2, 8])
                    .width(170)
                    .style(theme::note),
                text(action).size(13).width(Fill),
            ]
            .spacing(14)
            .align_y(Alignment::Center),
        );
    }
    let body = column![
        list,
        text("Les raccourcis à une touche sont inactifs pendant la saisie de texte.")
            .size(12)
            .style(theme::muted),
    ]
    .spacing(16);
    card_width("Raccourcis clavier", body.into(), 540.0)
}
