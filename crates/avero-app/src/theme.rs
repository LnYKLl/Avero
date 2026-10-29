//! Avero's look: colour tokens for light and dark mode, the Inter fonts,
//! the iced `Theme` built from them and named style functions used by
//! every view.
//!
//! Colours follow the V2 design (spec item 19): a gradient window
//! ("Avero" violet, "Orange" or "Océan") with white text on it, and a
//! light or dark card on top.

use crate::settings::ColorTheme;
use iced::font::Weight;
use iced::overlay::menu;
use iced::theme::Palette;
use iced::widget::{button, container, pick_list, scrollable, text, text_editor, text_input};
use iced::{Background, Border, Color, Degrees, Font, Shadow, Theme, border, color, gradient};

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

/// The star of a starred message (same in both modes).
pub const STAR: Color = color!(0xE8A317);

/// Corner radii: the big card of the window, cards and controls.
pub const WINDOW_RADIUS: f32 = 14.0;
pub const CARD_RADIUS: f32 = 12.0;
pub const CONTROL_RADIUS: f32 = 10.0;

/// White over the gradient, by how strongly it shows: the active entry of
/// the side bar, the search field, an entry under the mouse. Kept low so
/// that white text stays readable on them (see the contrast tests).
pub const ON_FRAME_ACTIVE: f32 = 0.14;
pub const ON_FRAME_FIELD: f32 = 0.12;
pub const ON_FRAME_HOVER: f32 = 0.07;

/// The gradient behind the side bar and the card: an angle in degrees
/// (CSS-like: 0 points up, 90 right) and its colour stops.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub angle: f32,
    pub stops: &'static [(f32, Color)],
}

/// Every colour the interface uses, for one colour theme and one mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tokens {
    /// The window's gradient (white text is drawn on it).
    pub frame: Frame,
    /// The reading pane, a little off the card's colour.
    pub bg: Color,
    /// The card: list, settings, dialogs.
    pub surface: Color,
    /// Hovered lines, notes.
    pub surface2: Color,
    /// Thin separators.
    pub line: Color,
    /// Borders of cards and fields.
    pub border: Color,
    pub text: Color,
    pub muted: Color,
    /// Filled buttons ("Envoyer"), with white text.
    pub brand: Color,
    /// Accent text and icons (links, the focused field).
    pub accent: Color,
    /// Selected line, selected pill.
    pub accent_soft: Color,
    /// Text on `accent_soft`.
    pub on_soft: Color,
    /// The dot of an unread line.
    pub unread: Color,
    pub danger: Color,
    pub success: Color,
    /// Dark ink: the toast.
    pub ink: Color,
    /// Text and icons drawn on `brand` or on the gradient.
    pub on_accent: Color,
}

/// What changes from one colour theme to the other.
struct Hue {
    frame: Frame,
    brand: Color,
    soft: Color,
    soft_dark: Color,
    unread: Color,
    /// Accent text on the dark card (the brand is too dark there).
    accent_dark: Color,
}

// The gradients of the validated design, darkened where white text fell
// under 4.5:1 (on the colour itself or under the white of the active
// entry): Avero's stops were #4F3BD1, #5B4FE8, #7A5BE0 and #A35BD0,
// Orange's #CC4E14, #C25A10 and #B5690E, Océan's #0F6C8F, #12708A and
// #1C6B74. Same hue, lower lightness.
const AVERO: Hue = Hue {
    frame: Frame {
        angle: 155.0,
        stops: &[
            (0.0, color!(0x4F3BD1)),
            (0.4, color!(0x5346E7)),
            (0.75, color!(0x6743DC)),
            (1.0, color!(0x8D38C3)),
        ],
    },
    brand: color!(0x5B4FE8),
    soft: color!(0xEEEBFE),
    soft_dark: color!(0x28233F),
    unread: color!(0x6D5BF0),
    accent_dark: color!(0x8B80FF),
};

const ORANGE: Hue = Hue {
    frame: Frame {
        angle: 150.0,
        stops: &[
            (0.0, color!(0xA94111)),
            (0.45, color!(0x9C490D)),
            (1.0, color!(0x8D520B)),
        ],
    },
    brand: color!(0xC2410C),
    soft: color!(0xFDEEE6),
    soft_dark: color!(0x3A2418),
    unread: color!(0xE5620F),
    accent_dark: color!(0xF0884A),
};

const OCEAN: Hue = Hue {
    frame: Frame {
        angle: 150.0,
        stops: &[
            (0.0, color!(0x0E6788)),
            (0.5, color!(0x116981)),
            (1.0, color!(0x1C6972)),
        ],
    },
    brand: color!(0x0E7490),
    soft: color!(0xE2F3F7),
    soft_dark: color!(0x173039),
    unread: color!(0x1592B5),
    accent_dark: color!(0x4FB8D8),
};

const fn light(hue: Hue) -> Tokens {
    Tokens {
        frame: hue.frame,
        bg: color!(0xFBFAF9),
        surface: color!(0xFFFFFF),
        surface2: color!(0xF4F2F0),
        line: color!(0xEFEDEB),
        border: color!(0xE5E2DF),
        text: color!(0x1D1B22),
        muted: color!(0x625F6B),
        brand: hue.brand,
        accent: hue.brand,
        accent_soft: hue.soft,
        on_soft: hue.brand,
        unread: hue.unread,
        danger: color!(0xD93D42),
        success: color!(0x2F9E66),
        ink: color!(0x1D1B22),
        on_accent: color!(0xFFFFFF),
    }
}

const fn dark(hue: Hue) -> Tokens {
    Tokens {
        frame: hue.frame,
        bg: color!(0x19181E),
        surface: color!(0x1E1D24),
        surface2: color!(0x29282F),
        line: color!(0x2B2A32),
        border: color!(0x36343E),
        text: color!(0xEEECF2),
        muted: color!(0xA9A6B4),
        brand: hue.brand,
        accent: hue.accent_dark,
        accent_soft: hue.soft_dark,
        on_soft: color!(0xFFFFFF),
        unread: hue.accent_dark,
        danger: color!(0xF06A6E),
        success: color!(0x3DBE7E),
        ink: color!(0x0E0D12),
        on_accent: color!(0xFFFFFF),
    }
}

/// Avero's violet, light and dark.
pub const LIGHT: Tokens = light(AVERO);
pub const DARK: Tokens = dark(AVERO);
const ORANGE_LIGHT: Tokens = light(ORANGE);
const ORANGE_DARK: Tokens = dark(ORANGE);
const OCEAN_LIGHT: Tokens = light(OCEAN);
const OCEAN_DARK: Tokens = dark(OCEAN);

/// The tokens of a colour theme in light (`dark = false`) or dark mode.
pub fn tokens_for(color: ColorTheme, dark: bool) -> &'static Tokens {
    match (color, dark) {
        (ColorTheme::Avero, false) => &LIGHT,
        (ColorTheme::Avero, true) => &DARK,
        (ColorTheme::Orange, false) => &ORANGE_LIGHT,
        (ColorTheme::Orange, true) => &ORANGE_DARK,
        (ColorTheme::Ocean, false) => &OCEAN_LIGHT,
        (ColorTheme::Ocean, true) => &OCEAN_DARK,
    }
}

/// The tokens matching a theme built by [`build`]: its mode, and the
/// colour theme whose brand is its primary colour.
pub fn tokens(theme: &Theme) -> &'static Tokens {
    let dark = theme.extended_palette().is_dark;
    let primary = theme.palette().primary;
    ColorTheme::ALL
        .into_iter()
        .map(|color| tokens_for(color, dark))
        .find(|t| t.brand == primary)
        .unwrap_or(if dark { &DARK } else { &LIGHT })
}

/// The iced theme for a colour theme, light (`dark = false`) or dark.
pub fn build(color: ColorTheme, dark: bool) -> Theme {
    let t = tokens_for(color, dark);
    let palette = Palette {
        background: t.surface,
        text: t.text,
        primary: t.brand,
        success: t.success,
        warning: color!(0xF5A524),
        danger: t.danger,
    };
    let mode = if dark { "sombre" } else { "clair" };
    Theme::custom(format!("{} {mode}", color.label()), palette)
}

/// The window's gradient as an iced background.
pub fn frame_background(t: &Tokens) -> Background {
    Background::Gradient(frame_gradient(&t.frame).into())
}

/// The window's gradient.
pub fn frame_gradient(frame: &Frame) -> gradient::Linear {
    let mut linear = gradient::Linear::new(Degrees(frame.angle));
    for &(offset, color) in frame.stops {
        linear = linear.add_stop(offset, color);
    }
    linear
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
        button::Status::Hovered => mix(t.brand, Color::BLACK, 0.08),
        button::Status::Pressed => mix(t.brand, Color::BLACK, 0.16),
        button::Status::Disabled => alpha(t.brand, 0.45),
        button::Status::Active => t.brand,
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

/// Text that is a link only when hovered (a sender's name).
pub fn link_plain(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let text_color = match status {
        button::Status::Hovered | button::Status::Pressed => t.accent,
        _ => t.text,
    };
    plain_button(None, text_color, Border::default())
}

/// A pill used as a filter (accounts, folders).
pub fn chip(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = tokens(theme);
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let (background, text_color) = if selected {
            (Some(t.accent_soft), t.on_soft)
        } else if hovered {
            (Some(t.surface2), t.text)
        } else {
            (None, t.muted)
        };
        plain_button(background, text_color, rounded(999.0))
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

/// An entry of the side bar, on the gradient: white text, a translucent
/// white background when `active` (and a fainter one under the mouse).
pub fn side_entry(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let background = if active {
            Some(alpha(Color::WHITE, ON_FRAME_ACTIVE))
        } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
            Some(alpha(Color::WHITE, ON_FRAME_HOVER))
        } else {
            None
        };
        plain_button(background, Color::WHITE, rounded(CONTROL_RADIUS))
    }
}

/// "Écrire": white, with the brand colour for its text.
pub fn write_button(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => mix(Color::WHITE, t.brand, 0.06),
        _ => Color::WHITE,
    };
    button::Style {
        shadow: Shadow {
            color: Color::from_rgba(0.12, 0.04, 0.0, 0.18),
            offset: iced::Vector::new(0.0, 2.0),
            blur_radius: 8.0,
        },
        ..plain_button(Some(background), t.brand, rounded(CONTROL_RADIUS))
    }
}

/// A space of the bottom of the side bar: a pill of its colour, ringed
/// in white.
pub fn space_pill(color: Color) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let background = match status {
            button::Status::Hovered | button::Status::Pressed => mix(color, Color::WHITE, 0.2),
            _ => color,
        };
        plain_button(
            Some(background),
            Color::WHITE,
            Border {
                color: Color::WHITE,
                width: 2.0,
                radius: 999.0.into(),
            },
        )
    }
}

/// One line of the message list.
pub fn list_row(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = tokens(theme);
        let background = if selected {
            Some(t.accent_soft)
        } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
            Some(t.surface2)
        } else {
            None
        };
        plain_button(background, t.text, Border::default())
    }
}

/// A folded message of a conversation: a bordered card, one line.
pub fn folded_card(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => t.surface2,
        _ => t.surface,
    };
    plain_button(Some(background), t.text, outline(t.border, CARD_RADIUS))
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

/// The window around the card: white text on the gradient, which
/// `view::frame` paints.
pub fn on_frame(_theme: &Theme) -> container::Style {
    container::Style {
        text_color: Some(Color::WHITE),
        ..container::Style::default()
    }
}

/// The window: the theme's gradient, white text on it.
pub fn frame(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(frame_background(tokens(theme))),
        text_color: Some(Color::WHITE),
        ..container::Style::default()
    }
}

/// The big rounded card on the gradient (list and reader, settings…).
pub fn window_card(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.surface.into()),
        text_color: Some(t.text),
        border: rounded(WINDOW_RADIUS),
        ..container::Style::default()
    }
}

/// One ring of the card's shadow (`depth` 0 is the closest to the card).
/// A real blurred shadow is drawn pixel by pixel by the software
/// renderer on every redraw; a few faint rings look the same for free.
pub fn shadow_ring(depth: usize) -> impl Fn(&Theme) -> container::Style {
    move |_theme| container::Style {
        background: Some(shadow_color(depth).into()),
        border: rounded(WINDOW_RADIUS + 1.0 + depth as f32),
        ..container::Style::default()
    }
}

/// The colour of one ring of the card's shadow.
pub fn shadow_color(depth: usize) -> Color {
    Color::from_rgba(0.08, 0.02, 0.0, 0.07 - 0.015 * depth as f32)
}

/// The message list column (on the card).
pub fn list_pane(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: None,
        text_color: Some(t.text),
        ..container::Style::default()
    }
}

/// The reading pane and the composer: the right part of the card, a
/// little off its colour, its outer corners rounded like the card.
pub fn page(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.bg.into()),
        text_color: Some(t.text),
        border: Border {
            radius: border::Radius::default()
                .top_right(WINDOW_RADIUS)
                .bottom_right(WINDOW_RADIUS),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

/// A page that fills the card (settings, "Grand nettoyage"): the card
/// under it already has its colour, painting it twice would only slow
/// down every redraw.
pub fn page_on_card(theme: &Theme) -> container::Style {
    container::Style {
        background: None,
        ..paper(theme)
    }
}

/// The welcome screen's card.
pub fn paper(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.surface.into()),
        text_color: Some(t.text),
        border: rounded(WINDOW_RADIUS),
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
        border: outline(t.border, WINDOW_RADIUS),
        ..container::Style::default()
    }
}

/// A flat card on the surface (settings sections, messages of a
/// conversation).
pub fn panel(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.surface.into()),
        border: outline(t.border, CARD_RADIUS),
        ..container::Style::default()
    }
}

/// A small outlined pill ("3 messages", the count of a conversation).
pub fn count_pill(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        text_color: Some(t.muted),
        border: outline(t.border, 999.0),
        ..container::Style::default()
    }
}

/// The white tile of the logo, on the gradient.
pub fn logo_tile(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(Color::WHITE.into()),
        text_color: Some(t.brand),
        border: rounded(9.0),
        ..container::Style::default()
    }
}

/// An account's dot in the side bar: its colour, ringed in white.
pub fn ringed_disc(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_theme| container::Style {
        background: Some(color.into()),
        border: Border {
            color: Color::WHITE,
            width: 1.5,
            radius: 999.0.into(),
        },
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
        background: Some(t.ink.into()),
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
        background: Some(tokens(theme).line.into()),
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

/// The search field of the side bar: translucent white on the gradient,
/// white text, a white outline once focused.
pub fn search_input(_theme: &Theme, status: text_input::Status) -> text_input::Style {
    let border_color = match status {
        text_input::Status::Focused { .. } => alpha(Color::WHITE, 0.7),
        _ => Color::TRANSPARENT,
    };
    text_input::Style {
        background: alpha(Color::WHITE, ON_FRAME_FIELD).into(),
        border: outline(border_color, CONTROL_RADIUS),
        icon: Color::WHITE,
        placeholder: Color::WHITE,
        value: Color::WHITE,
        selection: alpha(Color::WHITE, 0.35),
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

/// A multi-line field with a border, like [`input`] (signature).
pub fn editor(theme: &Theme, status: text_editor::Status) -> text_editor::Style {
    let t = tokens(theme);
    let border_color = match status {
        text_editor::Status::Focused { .. } => t.accent,
        text_editor::Status::Hovered => mix(t.border, t.muted, 0.35),
        _ => t.border,
    };
    text_editor::Style {
        background: t.surface.into(),
        border: outline(border_color, CONTROL_RADIUS),
        placeholder: alpha(t.muted, 0.85),
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

    /// Relative luminance (WCAG 2) of an opaque colour.
    fn luminance(c: Color) -> f32 {
        let channel = |v: f32| {
            if v <= 0.039_28 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b)
    }

    /// WCAG contrast ratio of two opaque colours.
    fn contrast(a: Color, b: Color) -> f32 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    /// `base` under white of opacity `amount` (the side bar's overlays).
    fn under_white(base: Color, amount: f32) -> Color {
        mix(base, Color::WHITE, amount)
    }

    fn every_tokens() -> Vec<(String, &'static Tokens)> {
        let mut all = Vec::new();
        for color in ColorTheme::ALL {
            for dark in [false, true] {
                all.push((format!("{color:?} dark={dark}"), tokens_for(color, dark)));
            }
        }
        all
    }

    #[test]
    fn built_themes_map_back_to_their_tokens() {
        for color in ColorTheme::ALL {
            for dark in [false, true] {
                assert_eq!(
                    tokens(&build(color, dark)),
                    tokens_for(color, dark),
                    "{color:?} {dark}"
                );
            }
        }
        assert_eq!(tokens(&build(ColorTheme::Avero, false)), &LIGHT);
        assert_eq!(tokens(&build(ColorTheme::Avero, true)), &DARK);
    }

    #[test]
    fn design_colours_are_exact() {
        assert_eq!(LIGHT.brand, Color::from_rgb8(0x5B, 0x4F, 0xE8));
        assert_eq!(LIGHT.surface, Color::WHITE);
        assert_eq!(LIGHT.bg, Color::from_rgb8(0xFB, 0xFA, 0xF9));
        assert_eq!(DARK.surface, Color::from_rgb8(0x1E, 0x1D, 0x24));
        assert_eq!(DARK.bg, Color::from_rgb8(0x19, 0x18, 0x1E));
        let orange = tokens_for(ColorTheme::Orange, false);
        assert_eq!(orange.brand, Color::from_rgb8(0xC2, 0x41, 0x0C));
        let ocean = tokens_for(ColorTheme::Ocean, true);
        assert_eq!(ocean.accent_soft, Color::from_rgb8(0x17, 0x30, 0x39));
    }

    /// White text on the gradient stays readable everywhere: on each stop,
    /// under the active entry and in the search field. Checking the stops
    /// is enough: a mix of two colours is never lighter than the lighter
    /// one, and neither is the same mix under white.
    #[test]
    fn white_text_on_the_gradient_reaches_4_5() {
        for (name, t) in every_tokens() {
            assert!(t.frame.stops.len() >= 2, "{name}");
            for &(_, stop) in t.frame.stops {
                for overlay in [0.0, ON_FRAME_HOVER, ON_FRAME_FIELD, ON_FRAME_ACTIVE] {
                    let ratio = contrast(Color::WHITE, under_white(stop, overlay));
                    assert!(ratio >= 4.5, "{name} {stop:?} +{overlay}: {ratio:.2}");
                }
            }
        }
    }

    #[test]
    fn text_on_the_card_reaches_4_5() {
        for (name, t) in every_tokens() {
            for background in [t.surface, t.bg, t.surface2] {
                assert!(contrast(t.text, background) >= 4.5, "{name} text");
                assert!(contrast(t.muted, background) >= 4.5, "{name} muted");
                assert!(contrast(t.accent, background) >= 4.5, "{name} accent");
            }
            assert!(contrast(t.on_soft, t.accent_soft) >= 4.5, "{name} pill");
            assert!(contrast(t.muted, t.accent_soft) >= 4.5, "{name} selected");
            assert!(contrast(t.on_accent, t.brand) >= 4.5, "{name} button");
            // "Écrire": the brand on white.
            assert!(contrast(t.brand, Color::WHITE) >= 4.5, "{name} write");
        }
    }

    #[test]
    fn the_gradient_is_an_iced_gradient() {
        let Background::Gradient(gradient::Gradient::Linear(linear)) = frame_background(&LIGHT)
        else {
            panic!("not a gradient");
        };
        let stops: Vec<_> = linear.stops.iter().flatten().collect();
        assert_eq!(stops.len(), LIGHT.frame.stops.len());
        assert_eq!(stops[0].color, Color::from_rgb8(0x4F, 0x3B, 0xD1));
    }

    #[test]
    fn mix_goes_from_a_to_b() {
        assert_eq!(mix(Color::BLACK, Color::WHITE, 0.0), Color::BLACK);
        assert_eq!(mix(Color::BLACK, Color::WHITE, 1.0), Color::WHITE);
    }
}
