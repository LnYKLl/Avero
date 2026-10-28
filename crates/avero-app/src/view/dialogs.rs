//! Small dialogs: password, rename, signature, delete confirmation,
//! reconnection, unsubscribe confirmation, keyboard shortcuts, the rule
//! editor, the confirmations of "Appliquer maintenant" and "Tout
//! mettre à la corbeille", the times of "Plus tard" and "Envoyer plus
//! tard", and the space editor.

use super::add_account::{error_line, spinner_line};
use super::{field_label, icon_button};
use crate::app::{
    AccountMsg, App, CleanupMsg, LaterMsg, LaterPicker, LaterPurpose, Message, Overlay,
    PasswordDialog, RuleForm, RuleMsg, SPACE_ICONS, ServerField, ServersForm, SpaceForm, SpaceMsg,
    UnsubscribeOffer, field_name, field_verb, move_name,
};
use crate::icons::{self, Icon, Tone};
use crate::later;
use crate::theme::{self, MEDIUM, SEMIBOLD};
use avero_core::rules::{RuleField, RuleMove};
use avero_core::{AccountColor, Security};
use iced::widget::{
    Column, button, checkbox, column, container, row, space, text, text_editor, text_input,
};
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
        Overlay::ConfirmUnsubscribe(offer) => confirm_unsubscribe(app, offer),
        Overlay::RuleEditor(form) => rule_editor(app, form),
        Overlay::ConfirmApplyRule { name, count, .. } => confirm_apply_rule(name, *count),
        Overlay::ConfirmTrashSender { name, rows, .. } => confirm_trash_sender(name, rows.len()),
        Overlay::Later(picker) => later_picker(app, picker),
        Overlay::SpaceEditor(form) => space_editor(app, form),
        Overlay::ConfirmDeleteSpace { name, .. } => confirm_delete_space(name),
    }
}

/// A space: its name, icon, colour and accounts.
fn space_editor<'a>(app: &'a App, form: &'a SpaceForm) -> Element<'a, Message> {
    let color = super::to_color(AccountColor::palette(form.color));
    let name = text_input("Par exemple : Perso, Pro", &form.name)
        .on_input(|v| Message::Space(SpaceMsg::Name(v)))
        .on_submit(Message::Space(SpaceMsg::Save))
        .padding([8, 10])
        .size(13)
        .style(theme::input);

    let mut icons_row = row![].spacing(4).align_y(Alignment::Center);
    for name in SPACE_ICONS {
        let selected = form.icon == name;
        let tone = if selected {
            Tone::Fixed(color)
        } else {
            Tone::Muted
        };
        icons_row = icons_row.push(
            button(icons::icon(icons::space_icon(name), 18.0, tone))
                .padding(7)
                .on_press(Message::Space(SpaceMsg::Icon(name.to_string())))
                .style(theme::chip(selected)),
        );
    }

    let mut colors = row![].spacing(6).align_y(Alignment::Center);
    for index in 0..AccountColor::COUNT {
        let selected = form.color == index;
        let swatch = super::to_color(AccountColor::palette(index));
        let mark: Element<'a, Message> = if selected {
            icons::icon(Icon::Check, 14.0, Tone::Fixed(iced::Color::WHITE))
        } else {
            space().into()
        };
        colors = colors.push(
            button(container(mark).center(22).style(theme::disc(swatch)))
                .padding(2)
                .on_press(Message::Space(SpaceMsg::Color(index)))
                .style(theme::ghost),
        );
    }

    let mut accounts = Column::new().spacing(8);
    for account in &app.accounts {
        let id = account.id;
        accounts = accounts.push(
            row![
                checkbox(form.accounts.contains(&id))
                    .on_toggle(move |on| Message::Space(SpaceMsg::Account(id, on)))
                    .size(15),
                super::dot(super::to_color(account.color), 8.0),
                text(account.name.clone()).size(13).font(MEDIUM),
                text(account.email.clone()).size(12).style(theme::muted),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }

    let mut body = Column::new()
        .spacing(14)
        .push(column![field_label("Nom"), name].spacing(6))
        .push(column![field_label("Icône"), icons_row.wrap()].spacing(6))
        .push(column![field_label("Couleur"), colors.wrap()].spacing(6))
        .push(column![field_label("Comptes"), accounts].spacing(8))
        .push(
            text(
                "Un compte peut faire partie de plusieurs espaces. L'espace choisi limite la \
                 liste, les compteurs et la recherche à ses comptes.",
            )
            .size(12)
            .style(theme::muted),
        );
    if let Some(error) = &form.error {
        body = body.push(error_line(error));
    }
    body = body.push(buttons(
        "Enregistrer",
        Some(Message::Space(SpaceMsg::Save)),
        theme::primary,
    ));
    let title = if form.id.is_some() {
        "Modifier l'espace"
    } else {
        "Nouvel espace"
    };
    card_width(title, body.into(), 520.0)
}

fn confirm_delete_space<'a>(name: &'a str) -> Element<'a, Message> {
    let body = column![
        text(format!("Supprimer l'espace « {name} » ?"))
            .size(14)
            .font(MEDIUM),
        text("Ses comptes et leurs mails restent dans Avero ; seul le regroupement disparaît.")
            .size(13)
            .style(theme::muted),
        buttons(
            "Supprimer",
            Some(Message::Space(SpaceMsg::ConfirmDelete)),
            theme::danger,
        ),
    ]
    .spacing(14);
    card("Supprimer l'espace", body.into())
}

/// "Plus tard" and "Envoyer plus tard": the ready-made times, each with
/// the date it stands for, then a date typed by hand.
fn later_picker<'a>(app: &'a App, picker: &'a LaterPicker) -> Element<'a, Message> {
    let now = app.clock.local();
    let (title, intro) = match picker.purpose {
        LaterPurpose::Snooze(_) => (
            "Plus tard",
            "La conversation quitte vos dossiers et revient en haut, non lue, au moment choisi. \
             Avero doit être ouvert à ce moment-là (sinon, elle revient au lancement suivant).",
        ),
        LaterPurpose::Send => (
            "Envoyer plus tard",
            "Le message reste sur cet ordinateur jusqu'à l'heure choisie, puis Avero l'envoie. \
             S'il est fermé à ce moment-là, l'envoi se fait au lancement suivant.",
        ),
    };
    let mut choices = Column::new().spacing(6);
    for &when in picker.choices() {
        let at = later::resolve(when, &now);
        choices = choices.push(
            button(
                row![
                    text(later::label(when, &now))
                        .size(14)
                        .font(MEDIUM)
                        .width(Fill),
                    text(later::describe(&at, &now))
                        .size(12)
                        .style(theme::muted),
                ]
                .spacing(12)
                .align_y(Alignment::Center),
            )
            .width(Fill)
            .padding([10, 12])
            .on_press(Message::Later(LaterMsg::Pick(when)))
            .style(theme::secondary),
        );
    }
    let input = text_input("31/12/2026 18:30", &picker.custom)
        .on_input(|v| Message::Later(LaterMsg::CustomText(v)))
        .on_submit(Message::Later(LaterMsg::PickCustom))
        .padding([10, 12])
        .size(14)
        .style(theme::input);
    let custom = row![
        input,
        button(text("Valider").size(13).font(MEDIUM))
            .padding([10, 14])
            .on_press_maybe(
                (!picker.custom.trim().is_empty()).then_some(Message::Later(LaterMsg::PickCustom))
            )
            .style(theme::primary),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let mut body = column![
        text(intro).size(13).style(theme::muted),
        choices,
        column![field_label("Date et heure choisies"), custom].spacing(6),
    ]
    .spacing(14);
    if let Some(error) = &picker.error {
        body = body.push(error_line(error));
    }
    card(title, body.into())
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

/// "Se désinscrire": says what will happen (and to whom) before it does.
fn confirm_unsubscribe<'a>(app: &'a App, offer: &'a UnsubscribeOffer) -> Element<'a, Message> {
    use crate::unsubscribe::Method;
    let host = |url: &reqwest::Url| url.host_str().unwrap_or_default().to_string();
    let from = app
        .account(offer.account)
        .map(|a| a.email.clone())
        .unwrap_or_default();
    let (what, detail, confirm, style): (String, String, &str, ButtonStyle) = match &offer.method {
        Method::OneClick(url) => (
            format!("Avero va demander à {} de ne plus vous écrire.", host(url)),
            "Une seule requête chiffrée (HTTPS) est envoyée à l'adresse de désinscription, \
             sans rien d'autre de ce message."
                .to_string(),
            "Se désinscrire",
            theme::primary,
        ),
        Method::Mail { to, subject, .. } => (
            format!("Avero va envoyer un mail de désinscription à {to}."),
            format!(
                "Envoyé depuis {from}, avec l'objet « {subject} » ; rien d'autre de ce message."
            ),
            "Envoyer",
            theme::primary,
        ),
        Method::Web(url) => (
            format!("La désinscription se fait sur le site {}.", host(url)),
            "La page s'ouvrira dans votre navigateur ; suivez ses instructions.".to_string(),
            "Ouvrir la page",
            theme::primary,
        ),
        Method::InsecureWeb(url) => (
            format!("La désinscription se fait sur le site {}.", host(url)),
            "Attention : ce lien n'est pas chiffré (http). Ce que vous enverrez sur cette page \
             pourra être lu en chemin."
                .to_string(),
            "Ouvrir quand même",
            theme::danger,
        ),
    };
    let body = column![
        text(format!("Expéditeur : {}", offer.sender_name))
            .size(13)
            .style(theme::muted),
        text(what).size(14).font(MEDIUM),
        text(detail).size(13).style(theme::muted),
        buttons(confirm, Some(Message::ConfirmUnsubscribe), style),
    ]
    .spacing(14);
    card("Se désinscrire", body.into())
}

/// The style of a dialog's confirm button.
type ButtonStyle = fn(&iced::Theme, button::Status) -> button::Style;

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

/// A small pill that is either picked or not.
fn pill<'a>(label: &'a str, selected: bool, message: RuleMsg) -> Element<'a, Message> {
    button(text(label).size(12).font(MEDIUM))
        .padding([5, 10])
        .on_press(Message::Rule(message))
        .style(theme::chip(selected))
        .into()
}

/// Writing or changing a rule: conditions, actions, accounts.
fn rule_editor<'a>(app: &'a App, form: &'a RuleForm) -> Element<'a, Message> {
    let name = text_input("Par exemple : Factures", &form.name)
        .on_input(|v| Message::Rule(RuleMsg::Name(v)))
        .padding([8, 10])
        .size(13)
        .style(theme::input);

    let mut conditions = Column::new().spacing(8);
    for (index, condition) in form.conditions.iter().enumerate() {
        let mut fields = row![].spacing(4).align_y(Alignment::Center);
        for field in RuleField::ALL {
            fields = fields.push(pill(
                field_name(field),
                condition.field == field,
                RuleMsg::Field(index, field),
            ));
        }
        let mut line = row![
            fields,
            text(field_verb(condition.field))
                .size(12)
                .style(theme::muted),
            text_input("texte", &condition.value)
                .on_input(move |v| Message::Rule(RuleMsg::Value(index, v)))
                .padding([6, 8])
                .size(13)
                .style(theme::input),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if form.conditions.len() > 1 {
            line = line.push(icon_button(
                Icon::Close,
                "Retirer cette condition",
                Some(Message::Rule(RuleMsg::RemoveCondition(index))),
            ));
        }
        conditions = conditions.push(line);
    }
    conditions = conditions.push(
        button(text("+ Ajouter une condition").size(12).font(MEDIUM))
            .padding([4, 0])
            .on_press(Message::Rule(RuleMsg::AddCondition))
            .style(theme::link),
    );

    let how = row![
        text("Les mails qui remplissent")
            .size(12)
            .style(theme::muted),
        pill(
            "toutes les conditions",
            form.match_all,
            RuleMsg::MatchAll(true)
        ),
        pill("au moins une", !form.match_all, RuleMsg::MatchAll(false)),
    ]
    .spacing(6)
    .align_y(Alignment::Center);

    let mut moves = row![].spacing(4).align_y(Alignment::Center);
    for to in [
        None,
        Some(RuleMove::Archive),
        Some(RuleMove::Trash),
        Some(RuleMove::Junk),
    ] {
        moves = moves.push(pill(move_name(to), form.move_to == to, RuleMsg::MoveTo(to)));
    }
    let read = checkbox(form.mark_read)
        .label("Marquer comme lu")
        .on_toggle(|on| Message::Rule(RuleMsg::MarkRead(on)))
        .size(15)
        .text_size(13)
        .spacing(8);

    let mut scope = row![pill(
        "Tous les comptes",
        form.account.is_none(),
        RuleMsg::Scope(None)
    )]
    .spacing(4)
    .align_y(Alignment::Center);
    for account in &app.accounts {
        scope = scope.push(pill(
            &account.name,
            form.account == Some(account.id),
            RuleMsg::Scope(Some(account.id)),
        ));
    }

    let mut body = Column::new()
        .spacing(14)
        .push(column![field_label("Nom"), name].spacing(6))
        .push(column![field_label("Si"), conditions].spacing(6));
    if form.conditions.len() > 1 {
        body = body.push(how);
    }
    body = body
        .push(column![field_label("Alors"), moves.wrap(), read].spacing(8))
        .push(column![field_label("Comptes"), scope.wrap()].spacing(6))
        .push(
            text(
                "Les règles s'appliquent aux nouveaux mails de la Réception, quand Avero est \
                 ouvert ; les déplacements se font sur le serveur.",
            )
            .size(12)
            .style(theme::muted),
        );
    if let Some(error) = &form.error {
        body = body.push(error_line(error));
    }
    body = body.push(buttons(
        "Enregistrer",
        Some(Message::Rule(RuleMsg::Save)),
        theme::primary,
    ));
    let title = if form.editing.is_some() {
        "Modifier la règle"
    } else {
        "Nouvelle règle"
    };
    card_width(title, body.into(), 620.0)
}

fn confirm_apply_rule<'a>(name: &'a str, count: usize) -> Element<'a, Message> {
    let what = if count > 1 {
        format!("{count} mails de la Réception sont concernés.")
    } else {
        "1 mail de la Réception est concerné.".to_string()
    };
    let body = column![
        text(format!("Règle « {name} »"))
            .size(13)
            .style(theme::muted),
        text(what).size(14).font(MEDIUM),
        text("Les déplacements se font sur le serveur.")
            .size(13)
            .style(theme::muted),
        buttons(
            "Appliquer",
            Some(Message::Rule(RuleMsg::ConfirmApply)),
            theme::primary,
        ),
    ]
    .spacing(14);
    card("Appliquer maintenant", body.into())
}

fn confirm_trash_sender<'a>(name: &'a str, count: usize) -> Element<'a, Message> {
    let what = if count > 1 {
        format!("Mettre les {count} mails de {name} à la corbeille ?")
    } else {
        format!("Mettre le mail de {name} à la corbeille ?")
    };
    let body = column![
        text(what).size(14).font(MEDIUM),
        text(
            "Ils sont déplacés dans la corbeille de chaque compte, sur le serveur ; \
                 vous pourrez les y retrouver."
        )
        .size(13)
        .style(theme::muted),
        buttons(
            "Mettre à la corbeille",
            Some(Message::Cleanup(CleanupMsg::ConfirmTrash)),
            theme::danger,
        ),
    ]
    .spacing(14);
    card("Tout mettre à la corbeille", body.into())
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
