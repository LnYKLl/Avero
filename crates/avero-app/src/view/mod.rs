//! Drawing the window: the gradient, the side bar and the big card on it
//! (list and reader, settings…) or the welcome screen, dialogs and the
//! toast, plus small pieces shared by every view.

pub mod add_account;
pub mod cleanup;
pub mod composer;
pub mod dialogs;
mod frame;
pub mod list;
pub mod oauth_setup;
pub mod reader;
pub mod settings;
pub mod sidebar;
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

/// Space around the card, in pixels.
pub const MARGIN: f32 = 16.0;
/// Rings of the card's shadow (see `theme::shadow_ring`).
const SHADOW_RINGS: usize = 3;

/// The whole window.
pub fn app(app: &App) -> Element<'_, Message> {
    let base: Element<'_, Message> = if app.accounts.is_empty() {
        let card = container(welcome::view(app)).style(theme::paper);
        container(
            container(shadowed(card.into()))
                .padding(MARGIN)
                .center(Fill),
        )
        .style(theme::frame)
        .into()
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
        let card = container(main)
            .width(Fill)
            .height(Fill)
            .style(theme::window_card);
        container(frame::frame(sidebar::view(app), card.into()))
            .width(Fill)
            .height(Fill)
            .style(theme::on_frame)
            .into()
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

/// `card` with a soft shadow below it (the welcome screen; `frame` draws
/// the main card's): faint rings, one pixel wider on the
/// sides and two lower at each step.
pub fn shadowed(card: Element<'_, Message>) -> Element<'_, Message> {
    let mut element = card;
    for depth in 0..SHADOW_RINGS {
        element = container(element)
            .padding(Padding {
                top: 0.0,
                right: 1.0,
                bottom: 2.0,
                left: 1.0,
            })
            .style(theme::shadow_ring(depth))
            .into();
    }
    element
}

/// The width of the card in this window.
pub fn card_width(app: &App) -> f32 {
    (app.window_width - sidebar::width(app) - MARGIN).max(0.0)
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

/// Colours of the avatars: deep enough for white initials (≥ 4.5:1).
const AVATAR_COLORS: [Color; 8] = [
    iced::color!(0xB83A6B),
    iced::color!(0x6D4FD8),
    iced::color!(0x2F6FC0),
    iced::color!(0x0F7A6C),
    iced::color!(0x9A5B0E),
    iced::color!(0xB4441F),
    iced::color!(0x55667A),
    iced::color!(0x7A3FA0),
];

/// The colour of a sender's avatar, always the same for the same address
/// (or name): a hash of it, lower case, picks one of `AVATAR_COLORS`.
pub fn avatar_color(key: &str) -> Color {
    // FNV-1a: stable from one run (and one version of Rust) to the next.
    let mut hash: u32 = 0x811c_9dc5;
    for byte in key.trim().to_lowercase().bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    AVATAR_COLORS[hash as usize % AVATAR_COLORS.len()]
}

/// "LC" for "Lenny Conoway", "M" for "Maman", "P" for "paul@ex.fr".
pub fn initials(name: &str) -> String {
    let clean = name.trim().trim_matches(['"', '\'', '<', '(', ')', '>']);
    if clean.contains('@') && !clean.contains(' ') {
        return initial(clean);
    }
    let words: Vec<&str> = clean
        .split_whitespace()
        .filter(|w| w.chars().next().is_some_and(char::is_alphanumeric))
        .collect();
    match words.as_slice() {
        [first, .., last] => format!("{}{}", initial(first), initial(last)),
        _ => initial(clean),
    }
}

/// A round avatar with an initial.
pub fn avatar<'a>(name: &str, color: Color, size: f32) -> Element<'a, Message> {
    let letters = initials(name);
    let scale = if letters.chars().count() > 1 {
        0.36
    } else {
        0.42
    };
    container(
        text(letters)
            .size(size * scale)
            .font(SEMIBOLD)
            .color(Color::WHITE),
    )
    .center(size)
    .style(theme::disc(color))
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

/// When a message of the reader was written: "09:00" today, "lun.
/// 09:00" this week, "27 oct., 09:00" otherwise (with the year if it is
/// not this one).
pub fn format_when(ts: i64, now: DateTime<Local>) -> String {
    let Some(date) = Local.timestamp_opt(ts, 0).single() else {
        return String::new();
    };
    let time = date.format("%H:%M");
    let days = (now.date_naive() - date.date_naive()).num_days();
    if days == 0 {
        time.to_string()
    } else if (1..6).contains(&days) {
        format!(
            "{} {time}",
            WEEKDAYS[date.weekday().num_days_from_monday() as usize]
        )
    } else {
        format!("{}, {time}", format_date(ts, now))
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
    fn reader_times_are_short() {
        let now = local(2026, 10, 28, 18, 0);
        let today = local(2026, 10, 28, 9, 0).timestamp();
        assert_eq!(format_when(today, now), "09:00");
        let monday = local(2026, 10, 26, 9, 5).timestamp();
        assert_eq!(format_when(monday, now), "lun. 09:05");
        let older = local(2026, 10, 2, 14, 32).timestamp();
        assert_eq!(format_when(older, now), "2 oct., 14:32");
    }

    #[test]
    fn avatars_have_two_initials_and_a_stable_colour() {
        assert_eq!(initials("Lenny Conoway"), "LC");
        assert_eq!(initials("Maman"), "M");
        assert_eq!(initials("\"Marie Anne Durand\""), "MD");
        assert_eq!(initials("paul@ex.fr"), "P");
        assert_eq!(initials(""), "?");
        let first = avatar_color("marie@ex.fr");
        assert_eq!(first, avatar_color(" Marie@Ex.fr"));
        // Different senders get different colours (here, at least).
        let colours: std::collections::HashSet<String> = ["a@x", "b@x", "c@x", "d@x", "e@x"]
            .iter()
            .map(|k| format!("{:?}", avatar_color(k)))
            .collect();
        assert!(colours.len() >= 3, "{colours:?}");
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
