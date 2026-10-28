//! The settings page: accounts, spaces, notifications, reading, rules,
//! the spam filter, appearance, about.

use super::{dot, hr, tip, to_color};
use crate::app::{AccountMsg, AddMsg, App, Message, RuleMsg, SpaceMsg, move_name};
use crate::icons::{self, Icon, Tone};
use crate::settings::{ColorTheme, ThemeChoice};
use crate::theme::{self, MEDIUM, SEMIBOLD};
use avero_core::rules::Rule;
use avero_core::{Account, AuthKind, Space};
use avero_oauth::Provider;
use iced::widget::{
    Column, button, column, container, row, scrollable, space, text, toggler, tooltip,
};
use iced::{Alignment, Element, Fill};

/// Width of the settings column.
const MAX_WIDTH: f32 = 720.0;

/// Project page.
pub const REPOSITORY: &str = "https://github.com/LnYKLl/Avero";

pub fn view(app: &App) -> Element<'_, Message> {
    let page = column![
        text("Réglages").size(26).font(SEMIBOLD),
        section("Comptes", accounts(app)),
        section("Espaces", spaces(app)),
        section("Connexion Google / Microsoft", oauth_clients(app)),
        section("Notifications", notifications(app)),
        section("Lecture", reading(app)),
        section("Règles", rules(app)),
        section("Anti-spam", spam_filter(app)),
        section("Apparence", appearance(app)),
        section("À propos", about()),
    ]
    .spacing(22)
    .max_width(MAX_WIDTH);
    container(
        scrollable(container(page).padding([28, 40]).center_x(Fill))
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

/// The spaces, in order, each with what can be done to it, then "Nouvel
/// espace".
fn spaces(app: &App) -> Element<'_, Message> {
    let mut list = Column::new();
    let last = app.spaces.len().saturating_sub(1);
    for (index, space) in app.spaces.iter().enumerate() {
        list = list.push(space_row(app, space, index == 0, index == last));
        list = list.push(hr());
    }
    list.push(
        row![
            button(
                row![
                    icons::icon(Icon::Plus, 16.0, Tone::Accent),
                    text("Nouvel espace").size(13).font(MEDIUM),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .padding([6, 8])
            .on_press(Message::Space(SpaceMsg::New))
            .style(theme::link),
            text(
                "Un espace regroupe des comptes (Perso, Pro…) : choisi au-dessus de la liste \
                 (Ctrl+1, Ctrl+2…), il n'en montre que les mails."
            )
            .size(12)
            .style(theme::muted)
            .width(Fill),
        ]
        .spacing(12)
        .padding([8, 12])
        .align_y(Alignment::Center),
    )
    .into()
}

fn space_row<'a>(app: &'a App, space: &'a Space, first: bool, last: bool) -> Element<'a, Message> {
    let id = space.id;
    let color = super::list::space_color(space);
    let names: Vec<&str> = app
        .accounts
        .iter()
        .filter(|a| space.contains(a.id))
        .map(|a| a.name.as_str())
        .collect();
    let accounts = if names.is_empty() {
        "Aucun compte".to_string()
    } else {
        names.join(", ")
    };
    let action = |icon, label: &'a str, message: Option<SpaceMsg>, tone| {
        tip(
            button(icons::icon(icon, 16.0, tone))
                .padding(7)
                .on_press_maybe(message.map(Message::Space))
                .style(theme::ghost),
            label,
            tooltip::Position::Bottom,
        )
    };
    row![
        container(icons::icon(
            icons::space_icon(&space.icon),
            16.0,
            Tone::Fixed(iced::Color::WHITE)
        ))
        .center(30)
        .style(theme::disc(color)),
        column![
            text(space.name.clone()).size(14).font(MEDIUM),
            text(accounts).size(12).style(theme::muted),
        ]
        .spacing(3)
        .width(Fill),
        action(
            Icon::ChevronUp,
            "Monter",
            (!first).then_some(SpaceMsg::Move(id, -1)),
            Tone::Text
        ),
        action(
            Icon::ChevronDown,
            "Descendre",
            (!last).then_some(SpaceMsg::Move(id, 1)),
            Tone::Text
        ),
        action(
            Icon::Pencil,
            "Modifier",
            Some(SpaceMsg::Edit(id)),
            Tone::Text
        ),
        action(
            Icon::Trash,
            "Supprimer",
            Some(SpaceMsg::AskDelete(id)),
            Tone::Danger
        ),
    ]
    .spacing(10)
    .padding([12, 18])
    .align_y(Alignment::Center)
    .into()
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
    if account.smtp.host.trim().is_empty() {
        details = details.push(
            text("Serveur d'envoi à configurer (bouton Serveurs)")
                .size(12)
                .style(theme::danger_text),
        );
    }
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
        action(
            Icon::FileText,
            "Signature",
            AccountMsg::EditSignature(id),
            Tone::Text
        ),
        action(
            Icon::Globe,
            "Serveurs (réception et envoi)",
            AccountMsg::OpenServers(id),
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

/// The OAuth clients behind "Continuer avec Google / Microsoft".
fn oauth_clients(app: &App) -> Element<'_, Message> {
    let mut list = Column::new();
    for (index, provider) in [Provider::Google, Provider::Microsoft]
        .into_iter()
        .enumerate()
    {
        if index > 0 {
            list = list.push(hr());
        }
        list = list.push(client_row(app, provider));
    }
    list.push(hr())
        .push(
            container(
                text(
                    "Google et Microsoft demandent que chaque application soit enregistrée par \
                     son propriétaire. L'assistant vous guide une seule fois ; ensuite chaque \
                     compte se connecte en un clic.",
                )
                .size(12)
                .style(theme::muted),
            )
            .padding([12, 18]),
        )
        .into()
}

fn client_row(app: &App, provider: Provider) -> Element<'_, Message> {
    let name = crate::auth::provider_name(provider);
    let logo = match provider {
        Provider::Google => Icon::Google,
        Provider::Microsoft => Icon::Microsoft,
    };
    let info = app.oauth_clients.get(&provider);
    let status: Element<'_, Message> = match info {
        Some(info) => row![
            icons::icon(Icon::Check, 13.0, Tone::Fixed(theme::LIGHT.success)),
            text(if info.saved {
                format!("Configurée · {}", info.masked_id)
            } else {
                format!("Intégrée à cette version · {}", info.masked_id)
            })
            .size(12)
            .style(theme::muted),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .into(),
        None => text(format!(
            "Non configurée : nécessaire pour « Continuer avec {name} »"
        ))
        .size(12)
        .style(theme::muted)
        .into(),
    };
    let edit_label = if info.is_some() {
        "Modifier"
    } else {
        "Configurer"
    };
    let mut actions = row![
        button(text(edit_label).size(13).font(MEDIUM))
            .padding([6, 12])
            .on_press(Message::Add(AddMsg::EditClient(provider)))
            .style(theme::secondary),
    ]
    .spacing(4)
    .align_y(Alignment::Center);
    if info.is_some_and(|i| i.saved) {
        actions = actions.push(tip(
            button(icons::icon(Icon::Trash, 17.0, Tone::Danger))
                .padding(8)
                .on_press(Message::Add(AddMsg::RemoveClient(provider)))
                .style(theme::ghost),
            "Retirer l'identifiant",
            tooltip::Position::Bottom,
        ));
    }
    row![
        icons::icon(logo, 20.0, Tone::Original),
        column![
            text(format!("Connexion {name}")).size(14).font(SEMIBOLD),
            status,
        ]
        .spacing(3)
        .width(Fill),
        actions,
    ]
    .spacing(14)
    .padding([14, 18])
    .align_y(Alignment::Center)
    .into()
}

fn notifications(app: &App) -> Element<'_, Message> {
    row![
        column![
            text("Notifications de nouveaux messages")
                .size(14)
                .font(MEDIUM),
            text("Expéditeur et objet des nouveaux mails non lus de la Réception.")
                .size(12)
                .style(theme::muted),
        ]
        .spacing(3)
        .width(Fill),
        toggler(app.settings.notifications).on_toggle(Message::NotificationsToggled),
    ]
    .spacing(12)
    .padding(18)
    .align_y(Alignment::Center)
    .into()
}

fn reading(app: &App) -> Element<'_, Message> {
    let threads = row![
        column![
            text("Regrouper les conversations").size(14).font(MEDIUM),
            text("Une ligne par échange, avec les réponses ; désactivé, une ligne par message.")
                .size(12)
                .style(theme::muted),
        ]
        .spacing(3)
        .width(Fill),
        toggler(app.settings.group_threads).on_toggle(Message::ThreadsToggled),
    ]
    .spacing(12)
    .padding(18)
    .align_y(Alignment::Center);
    let tabs = row![
        column![
            text("Trier la réception en onglets").size(14).font(MEDIUM),
            text(
                "Important, Newsletters et Notifications, d'après les en-têtes des messages \
                 (tri local, rien n'est envoyé)."
            )
            .size(12)
            .style(theme::muted),
        ]
        .spacing(3)
        .width(Fill),
        toggler(app.settings.inbox_tabs).on_toggle(Message::TabsToggled),
    ]
    .spacing(12)
    .padding(18)
    .align_y(Alignment::Center);
    column![threads, hr(), tabs].into()
}

/// The rules, in the order they apply, and "Nouvelle règle".
fn rules(app: &App) -> Element<'_, Message> {
    let mut list = Column::new();
    let last = app.rules.len().saturating_sub(1);
    for (index, rule) in app.rules.iter().enumerate() {
        list = list.push(rule_row(app, rule, index == 0, index == last));
        list = list.push(hr());
    }
    list.push(
        row![
            button(
                row![
                    icons::icon(Icon::Plus, 16.0, Tone::Accent),
                    text("Nouvelle règle").size(13).font(MEDIUM),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .padding([6, 8])
            .on_press(Message::Rule(RuleMsg::New))
            .style(theme::link),
            text(
                "Les règles s'appliquent aux nouveaux mails de la Réception, quand Avero est ouvert."
            )
            .size(12)
            .style(theme::muted)
            .width(Fill),
        ]
        .spacing(12)
        .padding([8, 12])
        .align_y(Alignment::Center),
    )
    .into()
}

/// What a rule does, in a few words: "Si l'expéditeur contient
/// « promo » : archiver, marquer comme lu · Perso".
fn rule_summary(app: &App, rule: &Rule) -> String {
    use avero_core::rules::RuleField;
    let conditions: Vec<String> = rule
        .def
        .conditions
        .iter()
        .map(|c| {
            let field = match c.field {
                RuleField::From => "l'expéditeur",
                RuleField::To => "le destinataire",
                RuleField::Subject => "l'objet",
            };
            format!("{field} contient « {} »", c.value)
        })
        .collect();
    let joiner = if rule.def.match_all { " et " } else { " ou " };
    let mut actions = Vec::new();
    if rule.def.move_to.is_some() {
        actions.push(move_name(rule.def.move_to).to_lowercase());
    }
    if rule.def.mark_read {
        actions.push("marquer comme lu".to_string());
    }
    let scope = match rule.def.account.and_then(|id| app.account(id)) {
        Some(account) => account.name.clone(),
        None => "tous les comptes".to_string(),
    };
    format!(
        "Si {} : {} · {scope}",
        conditions.join(joiner),
        actions.join(", ")
    )
}

fn rule_row<'a>(app: &'a App, rule: &'a Rule, first: bool, last: bool) -> Element<'a, Message> {
    let id = rule.id;
    let action = |icon, label: &'a str, message: Option<RuleMsg>, tone| {
        tip(
            button(icons::icon(icon, 16.0, tone))
                .padding(7)
                .on_press_maybe(message.map(Message::Rule))
                .style(theme::ghost),
            label,
            tooltip::Position::Bottom,
        )
    };
    row![
        toggler(rule.enabled).on_toggle(move |on| Message::Rule(RuleMsg::Toggle(id, on))),
        column![
            text(rule.def.name.clone()).size(14).font(MEDIUM),
            text(rule_summary(app, rule)).size(12).style(theme::muted),
        ]
        .spacing(3)
        .width(Fill),
        button(text("Appliquer maintenant").size(12).font(MEDIUM))
            .padding([5, 10])
            .on_press(Message::Rule(RuleMsg::Apply(id)))
            .style(theme::secondary),
        action(
            Icon::ChevronUp,
            "Monter",
            (!first).then_some(RuleMsg::Shift(id, true)),
            Tone::Text
        ),
        action(
            Icon::ChevronDown,
            "Descendre",
            (!last).then_some(RuleMsg::Shift(id, false)),
            Tone::Text
        ),
        action(
            Icon::Pencil,
            "Modifier",
            Some(RuleMsg::Edit(id)),
            Tone::Text
        ),
        action(
            Icon::Trash,
            "Supprimer",
            Some(RuleMsg::Delete(id)),
            Tone::Danger
        ),
    ]
    .spacing(8)
    .padding([12, 18])
    .align_y(Alignment::Center)
    .into()
}

fn spam_filter(app: &App) -> Element<'_, Message> {
    row![
        column![
            text("Filtre anti-spam").size(14).font(MEDIUM),
            text(
                "Apprend de vos boutons « Spam » et « Pas spam » et place dans le spam les \
                 nouveaux mails dont il est sûr. Jamais pour quelqu'un à qui vous avez écrit. \
                 Tout reste sur cet ordinateur."
            )
            .size(12)
            .style(theme::muted),
        ]
        .spacing(3)
        .width(Fill),
        toggler(app.settings.spam_filter).on_toggle(Message::SpamFilterToggled),
    ]
    .spacing(12)
    .padding(18)
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
    let dark = app.tokens().surface == theme::DARK.surface;
    let colour = |color: ColorTheme| {
        let selected = app.settings.color == color;
        let tokens = theme::tokens_for(color, dark);
        let swatch = container(
            text("Aa")
                .size(15)
                .font(theme::BOLD)
                .color(iced::Color::WHITE),
        )
        .center_x(Fill)
        .height(44)
        .center_y(44)
        .style(move |_theme: &iced::Theme| container::Style {
            background: Some(theme::frame_background(tokens)),
            border: iced::border::rounded(theme::CONTROL_RADIUS),
            ..container::Style::default()
        });
        button(
            column![
                swatch,
                container(text(color.label()).size(13).font(MEDIUM)).center_x(Fill),
            ]
            .spacing(8),
        )
        .width(Fill)
        .padding(8)
        .on_press(Message::ColorChosen(color))
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
        text("Couleurs").size(14).font(MEDIUM),
        row(ColorTheme::ALL.map(|color| colour(color).into())).spacing(10),
        text("Le dégradé de la fenêtre et la couleur des boutons, en clair comme en sombre.")
            .size(12)
            .style(theme::muted),
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
