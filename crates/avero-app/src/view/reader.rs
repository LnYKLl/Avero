//! The reading pane: headers of the selected message.

use super::{bold, dot, find_account, format_full_date};
use crate::app::Message;
use avero_core::{Account, UnifiedItem};
use iced::widget::{Column, center, column, container, row, text};
use iced::{Center, Element, Fill, Theme};

pub fn view<'a>(
    selected: Option<&'a UnifiedItem>,
    accounts: &'a [Account],
) -> Element<'a, Message> {
    let Some(item) = selected else {
        return center(text("Sélectionne un message").size(14))
            .style(muted)
            .into();
    };

    // Which accounts received this message (several if it was merged).
    let mut received = Column::new().spacing(4);
    for id in &item.accounts {
        if let Some(account) = find_account(accounts, *id) {
            received = received.push(
                row![dot(account.color, 10.0), text(&account.email).size(13)]
                    .spacing(6)
                    .align_y(Center),
            );
        }
    }

    let headers = column![
        text(&item.subject).size(22).font(bold()),
        text(format!("De : {}", item.from_display)).size(14),
        text(format!("Date : {}", format_full_date(item.date))).size(14),
        row![text("Reçu sur :").size(13), received]
            .spacing(8)
            .align_y(Center),
        container(text("Le contenu du mail arrive dans la prochaine version").size(14))
            .padding([24, 0])
            .style(muted),
    ]
    .spacing(8);

    container(headers)
        .padding(20)
        .width(Fill)
        .height(Fill)
        .into()
}

/// Greyed-out text.
fn muted(theme: &Theme) -> container::Style {
    container::Style {
        text_color: Some(theme.extended_palette().background.strong.color),
        ..container::Style::default()
    }
}
