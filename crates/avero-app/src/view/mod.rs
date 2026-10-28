//! The three panes of the main window, and small pieces they share.

pub mod list;
pub mod rail;
pub mod reader;

use avero_core::{Account, AccountColor, AccountId};
use chrono::{DateTime, Datelike, Local, TimeZone};
use iced::widget::{container, space};
use iced::{Color, Element, Font, border, font};

/// French short month names, January first.
const MONTHS: [&str; 12] = [
    "janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.",
    "déc.",
];

/// Date shown in the message list: "14:32" if the message is from
/// today, "27 oct." otherwise. `ts` is a unix time in seconds.
pub fn format_date(ts: i64, now: DateTime<Local>) -> String {
    let Some(date) = Local.timestamp_opt(ts, 0).single() else {
        return String::new();
    };
    if date.date_naive() == now.date_naive() {
        date.format("%H:%M").to_string()
    } else {
        let month = MONTHS[date.month0() as usize];
        format!("{} {month}", date.day())
    }
}

/// Full date for the reader, e.g. "27/10/2026 14:32".
pub fn format_full_date(ts: i64) -> String {
    match Local.timestamp_opt(ts, 0).single() {
        Some(date) => date.format("%d/%m/%Y %H:%M").to_string(),
        None => String::new(),
    }
}

/// The default font, in bold.
pub fn bold() -> Font {
    Font {
        weight: font::Weight::Bold,
        ..Font::DEFAULT
    }
}

/// Converts an account colour to an iced colour.
pub fn to_color(color: AccountColor) -> Color {
    Color::from_rgb8(color.r, color.g, color.b)
}

/// A round dot of the account's colour.
pub fn dot<'a, Message: 'a>(color: AccountColor, size: f32) -> Element<'a, Message> {
    container(space().width(size).height(size))
        .style(move |_theme| container::Style {
            background: Some(to_color(color).into()),
            border: border::rounded(size / 2.0),
            ..container::Style::default()
        })
        .into()
}

/// Finds an account by id.
pub fn find_account(accounts: &[Account], id: AccountId) -> Option<&Account> {
    accounts.iter().find(|a| a.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, h, min, 0).single().unwrap()
    }

    #[test]
    fn same_day_shows_hour_and_minute() {
        let now = local(2026, 10, 28, 18, 0);
        let ts = local(2026, 10, 28, 14, 32).timestamp();
        assert_eq!(format_date(ts, now), "14:32");
    }

    #[test]
    fn other_day_shows_day_and_short_month() {
        let now = local(2026, 10, 28, 18, 0);
        let ts = local(2026, 10, 27, 23, 59).timestamp();
        assert_eq!(format_date(ts, now), "27 oct.");
        let ts = local(2026, 2, 3, 9, 0).timestamp();
        assert_eq!(format_date(ts, now), "3 févr.");
    }
}
