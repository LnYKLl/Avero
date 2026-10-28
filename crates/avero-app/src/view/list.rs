//! The message list: account filters, category tabs, then the messages.

use super::{bold, dot, find_account, format_date};
use crate::app::{Message, PAGE};
use avero_core::{Account, AccountFilter, AccountId, UnifiedItem};
use chrono::{DateTime, Local};
use iced::widget::{
    Column, Row, button, center, column, container, row, scrollable, space, stack, text, tooltip,
};
use iced::{Center, Element, Fill, Theme, border};
use std::collections::HashMap;

/// Width of the list, in pixels.
const WIDTH: f32 = 360.0;

pub fn view<'a>(
    accounts: &'a [Account],
    filter: AccountFilter,
    items: &'a [UnifiedItem],
    selected: Option<usize>,
    errors: &'a HashMap<AccountId, String>,
    syncing: bool,
) -> Element<'a, Message> {
    let now = Local::now();
    let mut lines = Column::new();
    for (index, item) in items.iter().enumerate() {
        lines = lines.push(message_line(
            index,
            item,
            accounts,
            selected == Some(index),
            now,
        ));
    }
    if items.is_empty() {
        lines = lines.push(container(text("Aucun message").size(14)).padding(16));
    }
    // A full last page means there may be more.
    if !items.is_empty() && items.len().is_multiple_of(PAGE) {
        lines = lines.push(
            container(button(text("Charger plus").size(13)).on_press(Message::LoadMore))
                .padding(10)
                .center_x(Fill),
        );
    }

    let content = column![
        filters(accounts, filter, errors, syncing),
        tabs(),
        scrollable(lines).height(Fill),
    ];

    container(content)
        .width(WIDTH)
        .height(Fill)
        .style(|theme: &Theme| container::Style {
            border: border::width(1).color(theme.extended_palette().background.strong.color),
            ..container::Style::default()
        })
        .into()
}

/// "Tous" then one dot per account; the active filter is highlighted.
fn filters<'a>(
    accounts: &'a [Account],
    filter: AccountFilter,
    errors: &'a HashMap<AccountId, String>,
    syncing: bool,
) -> Element<'a, Message> {
    let all = filter_button(
        text("Tous").size(13).into(),
        filter == AccountFilter::All,
        AccountFilter::All,
    );
    let mut filters = Row::new().push(all).spacing(4).align_y(Center);

    for account in accounts {
        let only = AccountFilter::Only(account.id);
        let error = errors.get(&account.id);
        let content: Element<'a, Message> = match error {
            // A barred dot: something went wrong with this account.
            Some(_) => stack![
                center(dot(account.color, 12.0)),
                center(text("/").size(16).font(bold())),
            ]
            .width(18)
            .height(18)
            .into(),
            None => center(dot(account.color, 12.0)).width(18).height(18).into(),
        };
        let tip = match error {
            Some(message) => format!("{} — {message}", account.email),
            None => account.email.clone(),
        };
        filters = filters.push(tooltip(
            filter_button(content, filter == only, only),
            container(text(tip).size(13))
                .padding(6)
                .style(container::rounded_box),
            tooltip::Position::Bottom,
        ));
    }

    filters = filters.push(tooltip(
        button(text("+").size(14))
            .padding([2, 8])
            .on_press(Message::AddAccountOpened)
            .style(button::secondary),
        container(text("Ajouter un compte").size(13))
            .padding(6)
            .style(container::rounded_box),
        tooltip::Position::Bottom,
    ));
    if accounts.is_empty() {
        filters = filters.push(text("← Ajoute un compte").size(12));
    }

    if syncing {
        filters = filters
            .push(space().width(Fill))
            .push(text("Synchronisation…").size(12));
    }

    container(filters).padding([6, 8]).into()
}

fn filter_button<'a>(
    content: Element<'a, Message>,
    active: bool,
    filter: AccountFilter,
) -> Element<'a, Message> {
    button(content)
        .padding([3, 6])
        .on_press(Message::FilterSelected(filter))
        .style(move |theme: &Theme, status| {
            let palette = theme.extended_palette();
            let background = if active {
                Some(palette.primary.weak.color.into())
            } else if status == button::Status::Hovered {
                Some(palette.background.weak.color.into())
            } else {
                None
            };
            button::Style {
                background,
                text_color: palette.background.base.text,
                border: border::rounded(4),
                ..button::Style::default()
            }
        })
        .into()
}

/// Category tabs, shown but not active yet.
fn tabs<'a>() -> Element<'a, Message> {
    let tab = |label: &'a str| text(label).size(13).font(bold());
    container(
        row![tab("Important"), tab("Newsletters"), tab("Notifications")]
            .spacing(14)
            .padding([6, 8]),
    )
    .width(Fill)
    .style(|theme: &Theme| {
        let palette = theme.extended_palette();
        container::Style {
            text_color: Some(palette.background.strong.color),
            ..container::Style::default()
        }
    })
    .into()
}

/// One message: account dots, sender (bold if unread), date, subject.
fn message_line<'a>(
    index: usize,
    item: &'a UnifiedItem,
    accounts: &'a [Account],
    is_selected: bool,
    now: DateTime<Local>,
) -> Element<'a, Message> {
    let mut dots = Column::new().spacing(3).padding([4, 0]);
    for id in &item.accounts {
        if let Some(account) = find_account(accounts, *id) {
            dots = dots.push(dot(account.color, 8.0));
        }
    }

    let mut from = text(&item.from_display).size(14);
    if !item.seen {
        from = from.font(bold());
    }
    let first_line = row![
        container(from).width(Fill).clip(true),
        text(format_date(item.date, now)).size(12),
    ]
    .spacing(6);
    let subject = container(text(&item.subject).size(13)).clip(true);

    button(row![dots, column![first_line, subject].spacing(2)].spacing(8))
        .width(Fill)
        .padding([6, 10])
        .on_press(Message::MessageSelected(index))
        .style(move |theme: &Theme, status| {
            let palette = theme.extended_palette();
            let background = if is_selected {
                Some(palette.primary.weak.color.into())
            } else if status == button::Status::Hovered {
                Some(palette.background.weak.color.into())
            } else {
                None
            };
            button::Style {
                background,
                text_color: palette.background.base.text,
                ..button::Style::default()
            }
        })
        .into()
}
