//! Vector icons (Lucide, ISC licence) and logos, embedded in the binary.
//!
//! Lucide icons are drawn with `currentColor`: iced recolours them with
//! the `color` of the svg style, so one file serves every theme.

use crate::theme::{self, Tokens};
use iced::widget::svg::{self, Handle};
use iced::{Color, Element, Theme};
use std::f32::consts::TAU;
use std::sync::{LazyLock, Mutex};

/// Every icon the app draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Archive,
    Check,
    ChevronDown,
    ChevronLeft,
    Alert,
    Download,
    EyeOff,
    FileText,
    Forward,
    Globe,
    Inbox,
    Key,
    Loader,
    LogOut,
    MailOpen,
    Mail,
    MailX,
    Moon,
    Paperclip,
    Pencil,
    Plus,
    Refresh,
    ReplyAll,
    Reply,
    Search,
    Send,
    Settings,
    Shield,
    Compose,
    Star,
    /// The star, filled: a starred message.
    StarFilled,
    Sun,
    Trash,
    User,
    Users,
    Close,
    Copy,
    ExternalLink,
    /// The Avero mark (monochrome, `currentColor`).
    Mark,
    /// The Avero app icon tile (keeps its own colours).
    Logo,
    Google,
    Microsoft,
}

/// All icons, in the order of `bytes`; used to build the handle cache.
const ALL: [Icon; 42] = [
    Icon::Archive,
    Icon::Check,
    Icon::ChevronDown,
    Icon::ChevronLeft,
    Icon::Alert,
    Icon::Download,
    Icon::EyeOff,
    Icon::FileText,
    Icon::Forward,
    Icon::Globe,
    Icon::Inbox,
    Icon::Key,
    Icon::Loader,
    Icon::LogOut,
    Icon::MailOpen,
    Icon::Mail,
    Icon::MailX,
    Icon::Moon,
    Icon::Paperclip,
    Icon::Pencil,
    Icon::Plus,
    Icon::Refresh,
    Icon::ReplyAll,
    Icon::Reply,
    Icon::Search,
    Icon::Send,
    Icon::Settings,
    Icon::Shield,
    Icon::Compose,
    Icon::Star,
    Icon::StarFilled,
    Icon::Sun,
    Icon::Trash,
    Icon::User,
    Icon::Users,
    Icon::Close,
    Icon::Copy,
    Icon::ExternalLink,
    Icon::Mark,
    Icon::Logo,
    Icon::Google,
    Icon::Microsoft,
];

/// The SVG source of an icon.
pub fn bytes(icon: Icon) -> &'static [u8] {
    match icon {
        Icon::Archive => include_bytes!("../assets/icons/archive.svg"),
        Icon::Check => include_bytes!("../assets/icons/check.svg"),
        Icon::ChevronDown => include_bytes!("../assets/icons/chevron-down.svg"),
        Icon::ChevronLeft => include_bytes!("../assets/icons/chevron-left.svg"),
        Icon::Alert => include_bytes!("../assets/icons/circle-alert.svg"),
        Icon::Download => include_bytes!("../assets/icons/download.svg"),
        Icon::EyeOff => include_bytes!("../assets/icons/eye-off.svg"),
        Icon::FileText => include_bytes!("../assets/icons/file-text.svg"),
        Icon::Forward => include_bytes!("../assets/icons/forward.svg"),
        Icon::Globe => include_bytes!("../assets/icons/globe.svg"),
        Icon::Inbox => include_bytes!("../assets/icons/inbox.svg"),
        Icon::Key => include_bytes!("../assets/icons/key-round.svg"),
        Icon::Loader => include_bytes!("../assets/icons/loader-circle.svg"),
        Icon::LogOut => include_bytes!("../assets/icons/log-out.svg"),
        Icon::MailOpen => include_bytes!("../assets/icons/mail-open.svg"),
        Icon::Mail => include_bytes!("../assets/icons/mail.svg"),
        Icon::MailX => include_bytes!("../assets/icons/mail-x.svg"),
        Icon::Moon => include_bytes!("../assets/icons/moon.svg"),
        Icon::Paperclip => include_bytes!("../assets/icons/paperclip.svg"),
        Icon::Pencil => include_bytes!("../assets/icons/pencil.svg"),
        Icon::Plus => include_bytes!("../assets/icons/plus.svg"),
        Icon::Refresh => include_bytes!("../assets/icons/refresh-cw.svg"),
        Icon::ReplyAll => include_bytes!("../assets/icons/reply-all.svg"),
        Icon::Reply => include_bytes!("../assets/icons/reply.svg"),
        Icon::Search => include_bytes!("../assets/icons/search.svg"),
        Icon::Send => include_bytes!("../assets/icons/send.svg"),
        Icon::Settings => include_bytes!("../assets/icons/settings.svg"),
        Icon::Shield => include_bytes!("../assets/icons/shield-alert.svg"),
        Icon::Compose => include_bytes!("../assets/icons/square-pen.svg"),
        Icon::Star => include_bytes!("../assets/icons/star.svg"),
        Icon::StarFilled => include_bytes!("../assets/icons/star-filled.svg"),
        Icon::Sun => include_bytes!("../assets/icons/sun.svg"),
        Icon::Trash => include_bytes!("../assets/icons/trash-2.svg"),
        Icon::User => include_bytes!("../assets/icons/user.svg"),
        Icon::Users => include_bytes!("../assets/icons/users.svg"),
        Icon::Close => include_bytes!("../assets/icons/x.svg"),
        Icon::Copy => include_bytes!("../assets/icons/copy.svg"),
        Icon::ExternalLink => include_bytes!("../assets/icons/external-link.svg"),
        Icon::Mark => include_bytes!("../assets/logo/avero-mark.svg"),
        Icon::Logo => include_bytes!("../assets/logo/avero-icon.svg"),
        Icon::Google => include_bytes!("../assets/logo/google.svg"),
        Icon::Microsoft => include_bytes!("../assets/logo/microsoft.svg"),
    }
}

/// One handle per icon, built once: the renderer caches rasterised SVGs
/// by handle, so reusing them avoids hashing the bytes on every frame.
static HANDLES: LazyLock<Vec<(Icon, Handle)>> = LazyLock::new(|| {
    ALL.iter()
        .map(|&icon| (icon, Handle::from_memory(bytes(icon))))
        .collect()
});

fn handle(icon: Icon) -> Handle {
    HANDLES
        .iter()
        .find(|(i, _)| *i == icon)
        .map(|(_, h)| h.clone())
        .unwrap_or_else(|| Handle::from_memory(bytes(icon)))
}

/// Which colour an icon takes, read from the current theme.
#[derive(Debug, Clone, Copy)]
pub enum Tone {
    Text,
    Muted,
    Accent,
    OnAccent,
    Rail,
    Danger,
    Fixed(Color),
    /// Keep the colours of the SVG file (logos).
    Original,
}

impl Tone {
    fn color(self, t: &Tokens) -> Option<Color> {
        match self {
            Tone::Text => Some(t.text),
            Tone::Muted => Some(t.muted),
            Tone::Accent => Some(t.accent),
            Tone::OnAccent => Some(t.on_accent),
            Tone::Rail => Some(t.rail_icon),
            Tone::Danger => Some(t.danger),
            Tone::Fixed(color) => Some(color),
            Tone::Original => None,
        }
    }
}

/// An icon of `size` pixels in the given tone.
pub fn icon<'a, Message: 'a>(icon: Icon, size: f32, tone: Tone) -> Element<'a, Message> {
    icon_rotated(icon, size, tone, 0.0)
}

/// Same as [`icon`], turned by `radians` (spinning refresh icon).
pub fn icon_rotated<'a, Message: 'a>(
    icon: Icon,
    size: f32,
    tone: Tone,
    radians: f32,
) -> Element<'a, Message> {
    svg(rotated_handle(icon, radians))
        .width(size)
        .height(size)
        .style(move |theme: &Theme, _status| svg::Style {
            color: tone.color(theme::tokens(theme)),
        })
        .into()
}

/// Positions of a spinning icon per turn.
const SPIN_FRAMES: usize = 12;

/// The icon turned by `radians`, rounded to a twelfth of a turn. The
/// rotation is written into the SVG: iced's software renderer leaves
/// streaks behind rotated SVG widgets.
fn rotated_handle(icon: Icon, radians: f32) -> Handle {
    static FRAMES: Mutex<Vec<((Icon, usize), Handle)>> = Mutex::new(Vec::new());
    let turn = radians.rem_euclid(TAU) / TAU;
    let frame = (turn * SPIN_FRAMES as f32).round() as usize % SPIN_FRAMES;
    if frame == 0 {
        return handle(icon);
    }
    let mut frames = FRAMES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, h)) = frames.iter().find(|(key, _)| *key == (icon, frame)) {
        return h.clone();
    }
    let degrees = frame as f32 * 360.0 / SPIN_FRAMES as f32;
    let h = Handle::from_memory(rotate_svg(bytes(icon), degrees).into_bytes());
    frames.push(((icon, frame), h.clone()));
    h
}

/// Wraps the content of a 24×24 icon in a rotation around its centre.
fn rotate_svg(svg: &[u8], degrees: f32) -> String {
    let text = String::from_utf8_lossy(svg);
    let open_end = text
        .find("<svg")
        .and_then(|start| text[start..].find('>').map(|end| start + end + 1));
    match (open_end, text.rfind("</svg>")) {
        (Some(open), Some(close)) if open <= close => format!(
            "{}<g transform=\"rotate({degrees} 12 12)\">{}</g>{}",
            &text[..open],
            &text[open..close],
            &text[close..]
        ),
        _ => text.into_owned(),
    }
}

fn svg<'a>(handle: Handle) -> iced::widget::Svg<'a> {
    iced::widget::svg(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_valid_svg() {
        for icon in ALL {
            let text = std::str::from_utf8(bytes(icon)).expect("utf-8");
            assert!(text.contains("<svg"), "{icon:?}");
        }
    }

    #[test]
    fn spinning_icons_are_rotated_inside_the_svg() {
        let text = rotate_svg(bytes(Icon::Refresh), 90.0);
        let open = text.find("<g transform=\"rotate(90 12 12)\">").unwrap();
        assert!(open > text.find("viewBox").unwrap(), "inside the <svg> tag");
        assert!(text.trim_end().ends_with("</g></svg>"));
        assert_eq!(text.matches("<path").count(), 4);
        // Rounded to twelfths of a turn; a full turn is the plain icon.
        assert_eq!(rotated_handle(Icon::Refresh, TAU), handle(Icon::Refresh));
        assert_eq!(
            rotated_handle(Icon::Loader, 0.5),
            rotated_handle(Icon::Loader, 0.52)
        );
    }

    #[test]
    fn lucide_icons_follow_current_color() {
        for icon in [Icon::Inbox, Icon::Send, Icon::Mark, Icon::ChevronDown] {
            let text = std::str::from_utf8(bytes(icon)).unwrap();
            assert!(text.contains("currentColor"), "{icon:?}");
        }
    }
}
