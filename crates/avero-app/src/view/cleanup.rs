//! "Grand nettoyage": the senders who fill the mailbox the most, with
//! what can be done about each of them.

use super::{avatar, dot, hr, tip, to_color};
use crate::app::{App, CleanupMsg, Message};
use crate::icons::{self, Icon, Tone};
use crate::theme::{self, MEDIUM, SEMIBOLD};
use avero_core::{AccountFilter, SenderStats};
use iced::widget::{Column, Row, button, column, container, row, scrollable, text, tooltip};
use iced::{Alignment, Element, Fill};

/// Width of the page.
const MAX_WIDTH: f32 = 860.0;

pub fn view(app: &App) -> Element<'_, Message> {
    let mut list = Column::new();
    for (index, sender) in app.cleanup.senders.iter().enumerate() {
        if index > 0 {
            list = list.push(hr());
        }
        list = list.push(sender_row(app, sender));
    }
    let body: Element<'_, Message> = if app.cleanup.senders.is_empty() {
        container(
            text("Aucun mail à trier ici : rien dans la réception ni dans les archives.")
                .size(13)
                .style(theme::muted),
        )
        .padding(18)
        .into()
    } else {
        list.into()
    };
    let mut page = column![
        text("Grand nettoyage").size(26).font(SEMIBOLD),
        text(
            "Les expéditeurs qui remplissent le plus vos boîtes. La corbeille, le spam \
             et vos propres envois ne sont pas comptés."
        )
        .size(13)
        .style(theme::muted),
        account_chips(app),
        container(body).width(Fill).style(theme::panel),
    ]
    .spacing(16)
    .max_width(MAX_WIDTH);
    if app.cleanup.more {
        page = page.push(
            container(
                button(text("Voir plus d'expéditeurs").size(13).font(MEDIUM))
                    .padding([8, 14])
                    .on_press(Message::Cleanup(CleanupMsg::More))
                    .style(theme::secondary),
            )
            .center_x(Fill),
        );
    }
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

/// "Tous" and one chip per account.
fn account_chips(app: &App) -> Element<'_, Message> {
    let chip = |label: String, color, filter: AccountFilter| {
        let mut content = Row::new().spacing(6).align_y(Alignment::Center);
        if let Some(color) = color {
            content = content.push(dot(color, 8.0));
        }
        button(content.push(text(label).size(13).font(MEDIUM)))
            .padding([5, 10])
            .on_press(Message::Cleanup(CleanupMsg::Filter(filter)))
            .style(theme::chip(app.cleanup.filter == filter))
    };
    let mut chips = Row::new()
        .spacing(5)
        .push(chip("Tous".to_string(), None, AccountFilter::All));
    for account in app.space_accounts() {
        chips = chips.push(chip(
            account.name.clone(),
            Some(to_color(account.color)),
            AccountFilter::Only(account.id),
        ));
    }
    chips.wrap().into()
}

fn sender_row<'a>(app: &'a App, sender: &'a SenderStats) -> Element<'a, Message> {
    let unread_share = (sender.unread * 100)
        .checked_div(sender.count)
        .unwrap_or_default();
    let mails = if sender.count > 1 {
        format!("{} mails", sender.count)
    } else {
        "1 mail".to_string()
    };
    let figures = format!(
        "{mails} · {unread_share} % jamais ouverts · {}",
        format_size(sender.size)
    );
    let details = column![
        text(sender.name.clone()).size(14).font(SEMIBOLD),
        text(sender.addr.clone()).size(12).style(theme::muted),
        text(figures).size(12),
    ]
    .spacing(2)
    .width(Fill);

    let addr = sender.addr.clone();
    // An icon and its name (the tooltip says more).
    let action =
        |icon, name: &'static str, label: &'static str, tone, message: Option<CleanupMsg>| {
            let mut name = text(name).size(12).font(MEDIUM);
            if matches!(tone, Tone::Danger) {
                name = name.style(theme::danger_text);
            }
            tip(
                button(
                    row![icons::icon(icon, 16.0, tone), name]
                        .spacing(6)
                        .align_y(Alignment::Center),
                )
                .padding([6, 8])
                .on_press_maybe(message.map(Message::Cleanup))
                .style(theme::ghost),
                label,
                tooltip::Position::Bottom,
            )
        };
    let requested = app.cleanup.requested.contains(&sender.addr);
    let unsubscribe: Element<'_, Message> = if requested {
        container(text("Désinscription demandée").size(11).font(MEDIUM))
            .padding([2, 8])
            .style(theme::note)
            .into()
    } else {
        action(
            Icon::MailX,
            "Se désinscrire",
            if sender.can_unsubscribe {
                "Se désinscrire"
            } else {
                "Pas de lien de désinscription dans ces mails"
            },
            Tone::Text,
            sender
                .can_unsubscribe
                .then(|| CleanupMsg::Unsubscribe(addr.clone())),
        )
    };
    let block: Element<'_, Message> = if app.is_blocked(&sender.addr) {
        container(text("Bloqué").size(11).font(MEDIUM))
            .padding([2, 8])
            .style(theme::note)
            .into()
    } else {
        action(
            Icon::EyeOff,
            "Bloquer",
            "Bloquer : ses prochains mails iront à la corbeille",
            Tone::Text,
            Some(CleanupMsg::Block(addr.clone())),
        )
    };
    row![
        avatar(&sender.name, super::avatar_color(&sender.addr), 34.0),
        details,
        unsubscribe,
        block,
        action(
            Icon::Trash,
            "Tout supprimer",
            "Tout mettre à la corbeille",
            Tone::Danger,
            Some(CleanupMsg::TrashAll(addr)),
        ),
    ]
    .spacing(8)
    .padding([12, 18])
    .align_y(Alignment::Center)
    .into()
}

/// A size in bytes, the French way: "850 o", "12 Ko", "3,4 Mo", "1,2 Go".
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 3] = ["Ko", "Mo", "Go"];
    if bytes < 1024 {
        return format!("{bytes} o");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let shown = if value < 10.0 && unit > 0 {
        format!("{value:.1}").replace('.', ",")
    } else {
        format!("{value:.0}")
    };
    format!("{shown} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_the_french_way() {
        assert_eq!(format_size(0), "0 o");
        assert_eq!(format_size(850), "850 o");
        assert_eq!(format_size(12 * 1024), "12 Ko");
        assert_eq!(format_size(3_565_158), "3,4 Mo");
        assert_eq!(format_size(5 * 1024 * 1024 * 1024), "5,0 Go");
    }
}
