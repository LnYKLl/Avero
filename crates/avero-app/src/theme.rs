//! Avero's look: colour tokens for light and dark mode, the Inter fonts,
//! the iced `Theme` built from them and named style functions used by
//! every view.
//!
//! Colours follow "Direction visuelle" in the V1 spec exactly.

use iced::font::Weight;
use iced::overlay::menu;
use iced::theme::Palette;
use iced::widget::{button, container, pick_list, scrollable, text, text_editor, text_input};
use iced::{Background, Border, Color, Font, Shadow, Theme, border, color};

/// Regular Inter (the default font of the app).
pub const INTER: Font = Font::with_name("Inter");
/// Inter Medium, for buttons and chips.
pub const MEDIUM: Font = Font {
    weight: Weight::Medium,
    ..INTER
};
/// Inter SemiBold, for titles and unread senders.
pub const SEMIBOLD: Font = Font {
    weight: Weight::Semibold,
    ..INTER
};
/// Inter Bold, for the wordmark.
pub const BOLD: Font = Font {
    weight: Weight::Bold,
    ..INTER
};

/// The font files embedded in the binary, loaded at startup.
pub const FONT_FILES: [&[u8]; 4] = [
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../assets/fonts/Inter-Bold.ttf"),
];

/// Corner radii: cards and controls.
pub const CARD_RADIUS: f32 = 10.0;
pub const CONTROL_RADIUS: f32 = 8.0;

/// Every colour the interface uses, for one mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tokens {
    pub bg: Color,
    pub surface: Color,
    pub surface2: Color,
    pub border: Color,
    pub text: Color,
    pub muted: Color,
    pub accent: Color,
    pub accent_soft: Color,
    pub danger: Color,
    pub success: Color,
    /// The ink rail on the left.
    pub rail: Color,
    /// Icons drawn on the rail.
    pub rail_icon: Color,
    /// Text and icons drawn on the accent colour.
    pub on_accent: Color,
}

pub const LIGHT: Tokens = Tokens {
    bg: color!(0xF6F5F2),
    surface: color!(0xFFFFFF),
    surface2: color!(0xEFEDE8),
    border: color!(0xE4E1DA),
    text: color!(0x1C1B22),
    muted: color!(0x6B6875),
    accent: color!(0x5B4FE8),
    accent_soft: color!(0xECEAFD),
    danger: color!(0xE5484D),
    success: color!(0x30A46C),
    rail: color!(0x1C1B22),
    rail_icon: color!(0xA6A3B3),
    on_accent: color!(0xFFFFFF),
};

pub const DARK: Tokens = Tokens {
    bg: color!(0x121118),
    surface: color!(0x1A1922),
    surface2: color!(0x23222D),
    border: color!(0x2E2C3A),
    text: color!(0xECEAF4),
    muted: color!(0x9A97A8),
    accent: color!(0x8B80FF),
    accent_soft: color!(0x2A2650),
    danger: color!(0xE5484D),
    success: color!(0x30A46C),
    rail: color!(0x0C0B10),
    rail_icon: color!(0x8D8A9C),
    on_accent: color!(0xFFFFFF),
};

/// The tokens matching a theme built by [`build`].
pub fn tokens(theme: &Theme) -> &'static Tokens {
    if theme.extended_palette().is_dark {
        &DARK
    } else {
        &LIGHT
    }
}

/// The iced theme for light (`dark = false`) or dark mode.
pub fn build(dark: bool) -> Theme {
    let t = if dark { &DARK } else { &LIGHT };
    let palette = Palette {
        background: t.bg,
        text: t.text,
        primary: t.accent,
        success: t.success,
        warning: color!(0xF5A524),
        danger: t.danger,
    };
    let name = if dark { "Avero sombre" } else { "Avero clair" };
    Theme::custom(name, palette)
}

/// `color` with its alpha multiplied by `alpha`.
pub fn alpha(color: Color, alpha: f32) -> Color {
    Color {
        a: color.a * alpha,
        ..color
    }
}

fn rounded(radius: f32) -> Border {
    border::rounded(radius)
}

fn outline(color: Color, radius: f32) -> Border {
    Border {
        color,
        width: 1.0,
        radius: radius.into(),
    }
}

fn plain_button(background: Option<Color>, text_color: Color, border: Border) -> button::Style {
    button::Style {
        background: background.map(Background::Color),
        text_color,
        border,
        shadow: Shadow::default(),
        snap: true,
    }
}

// ---------------------------------------------------------------- buttons

/// Filled accent button: the main action of a screen.
pub fn primary(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let background = match status {
        button::Status::Hovered => mix(t.accent, Color::BLACK, 0.08),
        button::Status::Pressed => mix(t.accent, Color::BLACK, 0.16),
        button::Status::Disabled => alpha(t.accent, 0.45),
        button::Status::Active => t.accent,
    };
    plain_button(Some(background), t.on_accent, rounded(CONTROL_RADIUS))
}

/// White (surface) button with a thin border.
pub fn secondary(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => t.surface2,
        _ => t.surface,
    };
    let text_color = if status == button::Status::Disabled {
        t.muted
    } else {
        t.text
    };
    plain_button(
        Some(background),
        text_color,
        outline(t.border, CONTROL_RADIUS),
    )
}

/// Destructive action.
pub fn danger(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => mix(t.danger, Color::BLACK, 0.1),
        button::Status::Disabled => alpha(t.danger, 0.45),
        button::Status::Active => t.danger,
    };
    plain_button(Some(background), Color::WHITE, rounded(CONTROL_RADIUS))
}

/// No background until hovered.
pub fn ghost(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let (background, text_color) = match status {
        button::Status::Hovered | button::Status::Pressed => (Some(t.surface2), t.text),
        button::Status::Disabled => (None, alpha(t.muted, 0.6)),
        button::Status::Active => (None, t.text),
    };
    plain_button(background, text_color, rounded(CONTROL_RADIUS))
}

/// A text link in the accent colour.
pub fn link(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let text_color = match status {
        button::Status::Hovered | button::Status::Pressed => mix(t.accent, t.text, 0.25),
        _ => t.accent,
    };
    plain_button(None, text_color, Border::default())
}

/// A pill used as a filter (accounts, folders).
pub fn chip(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = tokens(theme);
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let (background, border_color, text_color) = if selected {
            (t.accent_soft, t.accent_soft, t.text)
        } else if hovered {
            (t.surface2, t.border, t.text)
        } else {
            (t.surface, t.border, t.muted)
        };
        plain_button(Some(background), text_color, outline(border_color, 999.0))
    }
}

/// A large selectable card (theme choice).
pub fn choice(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = tokens(theme);
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let (background, border_color) = if selected {
            (t.accent_soft, t.accent)
        } else if hovered {
            (t.surface2, t.border)
        } else {
            (t.surface, t.border)
        };
        plain_button(Some(background), t.text, outline(border_color, CARD_RADIUS))
    }
}

/// A rail button; `active` marks the current screen.
pub fn rail_button(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = tokens(theme);
        let background = if active {
            Some(alpha(Color::WHITE, 0.12))
        } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
            Some(alpha(Color::WHITE, 0.07))
        } else {
            None
        };
        plain_button(background, t.rail_icon, rounded(CONTROL_RADIUS))
    }
}

/// One line of the message list.
pub fn list_row(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = tokens(theme);
        let background = if selected {
            Some(t.accent_soft)
        } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
            Some(alpha(t.surface2, 0.7))
        } else {
            None
        };
        plain_button(background, t.text, rounded(CONTROL_RADIUS))
    }
}

/// A big sign-in button ("Continuer avec Google").
pub fn provider_button(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => t.surface2,
        _ => t.surface,
    };
    plain_button(Some(background), t.text, outline(t.border, CARD_RADIUS))
}

// ------------------------------------------------------------- containers

/// The ink rail.
pub fn rail(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.rail.into()),
        text_color: Some(t.rail_icon),
        ..container::Style::default()
    }
}

/// The message list column.
pub fn list_pane(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.bg.into()),
        text_color: Some(t.text),
        ..container::Style::default()
    }
}

/// The reading pane, composer and settings page.
pub fn page(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.surface.into()),
        text_color: Some(t.text),
        ..container::Style::default()
    }
}

/// The window background (welcome screen).
pub fn paper(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.bg.into()),
        text_color: Some(t.text),
        ..container::Style::default()
    }
}

/// A raised card with a thin border.
pub fn card(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.surface.into()),
        text_color: Some(t.text),
        // No shadow: tiny-skia's partial redraws can leave shadow ghosts
        // behind when a card changes size; a border is enough.
        border: outline(t.border, CARD_RADIUS),
        ..container::Style::default()
    }
}

/// A flat card on the surface (settings sections).
pub fn panel(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.surface.into()),
        border: outline(t.border, CARD_RADIUS),
        ..container::Style::default()
    }
}

/// A soft grey block (help text, notes).
pub fn note(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.surface2.into()),
        text_color: Some(t.text),
        border: rounded(CONTROL_RADIUS),
        ..container::Style::default()
    }
}

/// The round number of an assistant step.
pub fn step_badge(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.accent_soft.into()),
        text_color: Some(t.accent),
        border: rounded(999.0),
        ..container::Style::default()
    }
}

/// An error banner.
pub fn error_banner(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(alpha(t.danger, 0.10).into()),
        text_color: Some(t.text),
        border: outline(alpha(t.danger, 0.35), CONTROL_RADIUS),
        ..container::Style::default()
    }
}

/// Dims the window behind a dialog.
pub fn backdrop(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(alpha(Color::BLACK, 0.38).into()),
        ..container::Style::default()
    }
}

/// The short notice shown at the bottom ("Message envoyé").
pub fn toast(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.rail.into()),
        text_color: Some(Color::WHITE),
        border: rounded(CONTROL_RADIUS),
        ..container::Style::default()
    }
}

/// Tooltip bubble.
pub fn tooltip(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.text.into()),
        text_color: Some(t.surface),
        border: rounded(6.0),
        ..container::Style::default()
    }
}

/// A thin separator line.
pub fn divider(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(tokens(theme).border.into()),
        ..container::Style::default()
    }
}

/// A filled circle of `color` (avatars, account dots).
pub fn disc(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_theme| container::Style {
        background: Some(color.into()),
        text_color: Some(Color::WHITE),
        border: rounded(999.0),
        ..container::Style::default()
    }
}

/// The accent-filled count badge.
pub fn badge(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.accent.into()),
        text_color: Some(t.on_accent),
        border: rounded(999.0),
        ..container::Style::default()
    }
}

/// An attachment chip.
pub fn attachment(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.surface.into()),
        text_color: Some(t.text),
        border: outline(t.border, CONTROL_RADIUS),
        ..container::Style::default()
    }
}

// ------------------------------------------------------------------- text

/// Secondary text (dates, snippets, labels).
pub fn muted(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(tokens(theme).muted),
    }
}

/// Text in the accent colour.
pub fn accent(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(tokens(theme).accent),
    }
}

/// Confirmation text ("ID client valide").
pub fn success_text(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(tokens(theme).success),
    }
}

/// Error text.
pub fn danger_text(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(tokens(theme).danger),
    }
}

// ----------------------------------------------------------------- inputs

/// A text field.
pub fn input(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let t = tokens(theme);
    let border_color = match status {
        text_input::Status::Focused { .. } => t.accent,
        text_input::Status::Hovered => mix(t.border, t.muted, 0.35),
        _ => t.border,
    };
    text_input::Style {
        background: t.surface.into(),
        border: outline(border_color, CONTROL_RADIUS),
        icon: t.muted,
        placeholder: alpha(t.muted, 0.85),
        value: t.text,
        selection: alpha(t.accent, 0.3),
    }
}

/// The search field of the list (filled, no border until focused).
pub fn search_input(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let t = tokens(theme);
    let (background, border_color) = match status {
        text_input::Status::Focused { .. } => (t.surface, t.accent),
        _ => (t.surface2, t.surface2),
    };
    text_input::Style {
        background: background.into(),
        border: outline(border_color, CONTROL_RADIUS),
        icon: t.muted,
        placeholder: t.muted,
        value: t.text,
        selection: alpha(t.accent, 0.3),
    }
}

/// A borderless field (composer header lines).
pub fn bare_input(theme: &Theme, _status: text_input::Status) -> text_input::Style {
    let t = tokens(theme);
    text_input::Style {
        background: Color::TRANSPARENT.into(),
        border: Border::default(),
        icon: t.muted,
        placeholder: alpha(t.muted, 0.8),
        value: t.text,
        selection: alpha(t.accent, 0.3),
    }
}

/// A borderless multi-line editor (composer body, message body).
pub fn bare_editor(theme: &Theme, _status: text_editor::Status) -> text_editor::Style {
    let t = tokens(theme);
    text_editor::Style {
        background: Color::TRANSPARENT.into(),
        border: Border::default(),
        placeholder: alpha(t.muted, 0.8),
        value: t.text,
        selection: alpha(t.accent, 0.3),
    }
}

/// A borderless drop-down (composer "De").
pub fn bare_pick_list(theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let t = tokens(theme);
    let background = match status {
        pick_list::Status::Hovered | pick_list::Status::Opened { .. } => t.surface2,
        pick_list::Status::Active => Color::TRANSPARENT,
    };
    pick_list::Style {
        text_color: t.text,
        placeholder_color: t.muted,
        handle_color: t.muted,
        background: background.into(),
        border: rounded(CONTROL_RADIUS),
    }
}

/// The open list of a drop-down.
pub fn menu(theme: &Theme) -> menu::Style {
    let t = tokens(theme);
    menu::Style {
        background: t.surface.into(),
        border: outline(t.border, CONTROL_RADIUS),
        text_color: t.text,
        selected_text_color: t.text,
        selected_background: t.accent_soft.into(),
        shadow: Shadow::default(),
    }
}

/// Thin, discreet scrollbars.
pub fn scrollbar(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let t = tokens(theme);
    let hovered = matches!(status, scrollable::Status::Hovered { .. })
        || matches!(status, scrollable::Status::Dragged { .. });
    let scroller_color = if hovered {
        alpha(t.muted, 0.55)
    } else {
        alpha(t.muted, 0.28)
    };
    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: scroller_color.into(),
            border: rounded(999.0),
        },
    };
    let mut style = scrollable::default(theme, status);
    style.vertical_rail = rail;
    style.horizontal_rail = rail;
    style
}

/// Linear mix of two colours (`amount` of `b`).
pub fn mix(a: Color, b: Color, amount: f32) -> Color {
    Color {
        r: a.r + (b.r - a.r) * amount,
        g: a.g + (b.g - a.g) * amount,
        b: a.b + (b.b - a.b) * amount,
        a: a.a + (b.a - a.a) * amount,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_themes_map_back_to_their_tokens() {
        assert_eq!(tokens(&build(false)), &LIGHT);
        assert_eq!(tokens(&build(true)), &DARK);
    }

    #[test]
    fn spec_colours_are_exact() {
        assert_eq!(LIGHT.accent, Color::from_rgb8(0x5B, 0x4F, 0xE8));
        assert_eq!(DARK.accent, Color::from_rgb8(0x8B, 0x80, 0xFF));
        assert_eq!(LIGHT.bg, Color::from_rgb8(0xF6, 0xF5, 0xF2));
        assert_eq!(DARK.rail, Color::from_rgb8(0x0C, 0x0B, 0x10));
    }

    #[test]
    fn mix_goes_from_a_to_b() {
        assert_eq!(mix(Color::BLACK, Color::WHITE, 0.0), Color::BLACK);
        assert_eq!(mix(Color::BLACK, Color::WHITE, 1.0), Color::WHITE);
    }
}
