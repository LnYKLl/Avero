//! Drawing the window: the welcome screen or the three panes, dialogs
//! and the toast, plus small pieces shared by every view.

pub mod add_account;
pub mod cleanup;
pub mod composer;
pub mod dialogs;
pub mod list;
pub mod oauth_setup;
pub mod rail;
pub mod reader;
pub mod settings;
pub mod welcome;

use crate::app::{App, Message, Screen};
use crate::icons::{self, Icon, Tone};
use crate::theme::{self, SEMIBOLD};
use avero_core::{Account, AccountColor, AccountId};
use chrono::{DateTime, Datelike, Local, TimeZone};
use iced::widget::{
    Space, button, center, container, mouse_area, opaque, row, space, stack, text, tooltip,
};
use iced::{Alignment, Color, Element, Fill, Padding};

/// French short month names, January first.
const MONTHS: [&str; 12] = [
    "janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.",
    "déc.",
];

/// French short weekday names, Monday first.
const WEEKDAYS: [&str; 7] = ["lun.", "mar.", "mer.", "jeu.", "ven.", "sam.", "dim."];

/// The whole window.
pub fn app(app: &App) -> Element<'_, Message> {
    let base: Element<'_, Message> = if app.accounts.is_empty() {
        welcome::view(app)
    } else {
        let main: Element<'_, Message> = match app.screen {
            Screen::Mail => {
                let right = match &app.composer {
                    Some(composer) => composer::view(app, composer),
                    None => reader::view(app),
                };
                row![list::view(app), right].height(Fill).into()
            }
            Screen::Settings => settings::view(app),
            Screen::Cleanup => cleanup::view(app),
        };
        row![rail::view(app), main].height(Fill).into()
    };

    let mut layers = vec![base];
    if let Some(overlay) = &app.overlay {
        layers.push(modal(dialogs::view(app, overlay)));
    }
    if let Some(toast) = &app.toast {
        layers.push(toast_layer(&toast.text, toast.error));
    }
    stack(layers).width(Fill).height(Fill).into()
}

/// A dialog card centred over a dimmed window.
fn modal(card: Element<'_, Message>) -> Element<'_, Message> {
    opaque(mouse_area(center(card).style(theme::backdrop).padding(24)))
}

fn toast_layer(message: &str, error: bool) -> Element<'_, Message> {
    let tone = if error {
        Tone::Fixed(theme::LIGHT.danger)
    } else {
        Tone::Fixed(theme::LIGHT.success)
    };
    let icon = if error { Icon::Alert } else { Icon::Check };
    let bubble = container(
        row![icons::icon(icon, 16.0, tone), text(message).size(13)]
            .spacing(10)
            .align_y(Alignment::Center),
    )
    .padding([10, 16])
    .max_width(560)
    .style(theme::toast);
    container(bubble)
        .width(Fill)
        .height(Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::End)
        .padding(Padding::new(0.0).bottom(28))
        .into()
}

// ------------------------------------------------------------ small pieces

/// Converts an account colour to an iced colour.
pub fn to_color(color: AccountColor) -> Color {
    Color::from_rgb8(color.r, color.g, color.b)
}

/// Finds an account by id.
pub fn find_account(accounts: &[Account], id: AccountId) -> Option<&Account> {
    accounts.iter().find(|a| a.id == id)
}

/// The capital first letter of a name (or "?").
pub fn initial(name: &str) -> String {
    name.trim()
        .trim_start_matches(['"', '\'', '<', '('])
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "?".to_string())
}

/// A round avatar with an initial.
pub fn avatar<'a>(name: &str, color: Color, size: f32) -> Element<'a, Message> {
    container(
        text(initial(name))
            .size(size * 0.42)
            .font(SEMIBOLD)
            .color(Color::WHITE),
    )
    .center(size)
    .style(theme::disc(color))
    .into()
}

/// An avatar for a message received by several accounts: the first
/// account's colour, with a small dot of the second one.
pub fn shared_avatar<'a>(name: &str, colors: &[Color], size: f32) -> Element<'a, Message> {
    let first = colors.first().copied().unwrap_or(theme::LIGHT.muted);
    let Some(&second) = colors.get(1) else {
        return avatar(name, first, size);
    };
    let badge = container(dot(second, 10.0))
        .padding(2)
        .style(|theme: &iced::Theme| iced::widget::container::Style {
            background: Some(theme::tokens(theme).bg.into()),
            border: iced::border::rounded(999),
            ..Default::default()
        });
    stack![
        avatar(name, first, size),
        container(badge)
            .width(size + 3.0)
            .height(size + 3.0)
            .align_x(Alignment::End)
            .align_y(Alignment::End),
    ]
    .into()
}

/// A round dot.
pub fn dot<'a>(color: Color, size: f32) -> Element<'a, Message> {
    container(Space::new().width(size).height(size))
        .style(theme::disc(color))
        .into()
}

/// Wraps `content` in a tooltip.
pub fn tip<'a>(
    content: impl Into<Element<'a, Message>>,
    label: impl Into<String>,
    position: tooltip::Position,
) -> Element<'a, Message> {
    tooltip(
        content,
        container(text(label.into()).size(12))
            .padding([5, 9])
            .style(theme::tooltip),
        position,
    )
    .gap(6)
    .into()
}

/// A square icon button with a tooltip; `None` draws it disabled.
pub fn icon_button<'a>(
    icon: Icon,
    label: impl Into<String>,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    let enabled = on_press.is_some();
    let tone = if enabled {
        Tone::Text
    } else {
        Tone::Fixed(theme::alpha(theme::LIGHT.muted, 0.5))
    };
    let button = button(icons::icon(icon, 18.0, tone))
        .padding(8)
        .on_press_maybe(on_press)
        .style(theme::ghost);
    tip(button, label, tooltip::Position::Bottom)
}

/// A button with an icon and a label.
pub fn icon_label_button<'a>(
    icon: Icon,
    label: &'a str,
    tone: Tone,
    on_press: Option<Message>,
    style: impl Fn(&iced::Theme, button::Status) -> button::Style + 'a,
) -> button::Button<'a, Message> {
    button(
        row![
            icons::icon(icon, 16.0, tone),
            text(label).size(13).font(theme::MEDIUM)
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding([8, 14])
    .on_press_maybe(on_press)
    .style(style)
}

/// A one-pixel horizontal line.
pub fn hr<'a>() -> Element<'a, Message> {
    container(space().height(1))
        .width(Fill)
        .height(1)
        .style(theme::divider)
        .into()
}

/// A one-pixel vertical line.
pub fn vr<'a>() -> Element<'a, Message> {
    container(space().width(1))
        .width(1)
        .height(Fill)
        .style(theme::divider)
        .into()
}

/// A field label above an input.
pub fn field_label<'a>(label: &'a str) -> Element<'a, Message> {
    text(label)
        .size(12)
        .font(theme::MEDIUM)
        .style(theme::muted)
        .into()
}

/// Cuts a text to `max` characters, adding "…".
pub fn ellipsize(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or_default();
    if line.chars().count() <= max && !text.contains('\n') {
        return line.to_string();
    }
    let cut: String = line.chars().take(max).collect();
    format!("{}…", cut.trim_end())
}

/// Date shown in the message list: "14:32" today, "lun." this week,
/// "27 oct." otherwise. `ts` is a unix time in seconds.
pub fn format_date(ts: i64, now: DateTime<Local>) -> String {
    let Some(date) = Local.timestamp_opt(ts, 0).single() else {
        return String::new();
    };
    let days = (now.date_naive() - date.date_naive()).num_days();
    if days == 0 {
        date.format("%H:%M").to_string()
    } else if (1..6).contains(&days) {
        WEEKDAYS[date.weekday().num_days_from_monday() as usize].to_string()
    } else if date.year() == now.year() {
        let month = MONTHS[date.month0() as usize];
        format!("{} {month}", date.day())
    } else {
        format!(
            "{} {} {}",
            date.day(),
            MONTHS[date.month0() as usize],
            date.year()
        )
    }
}

/// Full date for the reader, e.g. "mar. 27 oct. 2026, 14:32".
pub fn format_full_date(ts: i64) -> String {
    match Local.timestamp_opt(ts, 0).single() {
        Some(date) if ts > 0 => format!(
            "{} {} {} {}, {}",
            WEEKDAYS[date.weekday().num_days_from_monday() as usize],
            date.day(),
            MONTHS[date.month0() as usize],
            date.year(),
            date.format("%H:%M")
        ),
        _ => String::new(),
    }
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
    fn this_week_shows_weekday() {
        let now = local(2026, 10, 28, 18, 0); // a Wednesday
        let ts = local(2026, 10, 27, 23, 59).timestamp();
        assert_eq!(format_date(ts, now), "mar.");
    }

    #[test]
    fn older_days_show_day_and_short_month() {
        let now = local(2026, 10, 28, 18, 0);
        let ts = local(2026, 10, 2, 9, 0).timestamp();
        assert_eq!(format_date(ts, now), "2 oct.");
        let ts = local(2026, 2, 3, 9, 0).timestamp();
        assert_eq!(format_date(ts, now), "3 févr.");
        let ts = local(2025, 2, 3, 9, 0).timestamp();
        assert_eq!(format_date(ts, now), "3 févr. 2025");
    }

    #[test]
    fn full_date_is_french() {
        let ts = local(2026, 10, 27, 14, 32).timestamp();
        assert_eq!(format_full_date(ts), "mar. 27 oct. 2026, 14:32");
        assert_eq!(format_full_date(0), "");
    }

    #[test]
    fn initials_skip_punctuation() {
        assert_eq!(initial("\"marie\" <m@x>"), "M");
        assert_eq!(initial("élodie"), "É");
        assert_eq!(initial("  "), "?");
    }

    #[test]
    fn ellipsize_cuts_long_lines() {
        assert_eq!(ellipsize("court", 10), "court");
        assert_eq!(ellipsize("un texte assez long", 8), "un texte…");
        assert_eq!(ellipsize("ligne 1\nligne 2", 50), "ligne 1…");
    }
}
