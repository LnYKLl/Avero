//! The thin icon bar on the left of the window.

use crate::app::Message;
use iced::widget::{button, center, column, container, space, text, tooltip};
use iced::{Element, Fill, Theme, border};

/// Width of the rail, in pixels.
const WIDTH: f32 = 48.0;

/// Mails (the current screen), Calendar and Big clean-up (not there yet),
/// and Settings at the bottom.
pub fn view<'a>() -> Element<'a, Message> {
    let rail = column![
        icon("✉", "Mails", true),
        icon("▦", "Calendrier — Bientôt", false),
        icon("♻", "Grand nettoyage — Bientôt", false),
        space().height(Fill),
        icon("⚙", "Réglages — Bientôt", false),
    ]
    .spacing(8)
    .padding([10, 0])
    .align_x(iced::Center);

    container(rail)
        .width(WIDTH)
        .height(Fill)
        .style(|theme: &Theme| {
            let palette = theme.extended_palette();
            container::Style {
                background: Some(palette.background.weak.color.into()),
                ..container::Style::default()
            }
        })
        .into()
}

/// One rail button. None of them sends a message yet: Mails is the only
/// screen, so it is simply drawn as active.
fn icon<'a>(glyph: &'a str, label: &'a str, active: bool) -> Element<'a, Message> {
    let button = button(center(text(glyph).size(18)))
        .width(34)
        .height(34)
        .padding(0)
        .style(move |theme: &Theme, _status| {
            let palette = theme.extended_palette();
            let (background, text_color) = if active {
                (
                    Some(palette.primary.base.color.into()),
                    palette.primary.base.text,
                )
            } else {
                (None, palette.background.strong.color)
            };
            button::Style {
                background,
                text_color,
                border: border::rounded(8),
                ..button::Style::default()
            }
        });

    tooltip(
        button,
        container(text(label).size(13))
            .padding(6)
            .style(container::rounded_box),
        tooltip::Position::Right,
    )
    .into()
}
