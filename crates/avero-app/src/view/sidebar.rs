//! The side bar, on the window's gradient with white text: logo, search
//! (Ctrl+K), "Écrire", the space picker, the places of the mail with
//! their counters, the accounts, then the spaces and the settings at the
//! bottom. In a narrow window it folds to its icons.

use super::tip;
use crate::app::{AddMsg, App, ComposeMsg, LaterMsg, Message, SEARCH_ID, Screen, SpaceMsg};
use crate::icons::{self, Icon, Tone};
use crate::theme::{self, BOLD, MEDIUM, SEMIBOLD};
use avero_core::{AccountFilter, FolderRole};
use iced::widget::text::Wrapping;
use iced::widget::{
    Column, Row, Space, button, center, column, container, row, scrollable, space, stack, text,
    text_input, tooltip,
};
use iced::{Alignment, Color, Element, Fill, Length, Padding};

/// Width of the side bar, unfolded and folded.
pub const WIDTH: f32 = 272.0;
pub const FOLDED_WIDTH: f32 = 68.0;
/// Below this window width, the side bar shows only its icons.
pub const FOLD_BELOW: f32 = 1100.0;

/// The width the side bar takes in this window.
pub fn width(app: &App) -> f32 {
    if folded(app) { FOLDED_WIDTH } else { WIDTH }
}

/// Whether the side bar shows only its icons (narrow window).
pub fn folded(app: &App) -> bool {
    app.window_width < FOLD_BELOW
}

/// A place of the side bar: what it shows and how to get there.
struct Place {
    label: &'static str,
    icon: Icon,
    count: usize,
    active: bool,
    on_press: Message,
}

/// The places, in the order of the design.
fn places(app: &App) -> Vec<Place> {
    let mail = app.screen == Screen::Mail;
    let plain = app.sender.is_none() && app.search_query.is_none();
    let folder = |role: FolderRole| mail && app.in_folder() && app.folder == role;
    let scheduled = app.outbox.iter().filter(|r| r.sent_at.is_none()).count();
    let mut list = vec![
        Place {
            label: "Boîte de réception",
            icon: Icon::Inbox,
            count: app.space_unread(app.space),
            active: folder(FolderRole::Inbox),
            on_press: Message::FolderSelected(FolderRole::Inbox),
        },
        Place {
            label: "Favoris",
            icon: Icon::Star,
            count: 0,
            active: mail && app.starred && plain,
            on_press: Message::StarredSelected,
        },
        Place {
            label: "Plus tard",
            icon: Icon::AlarmClock,
            count: app.snoozed_count,
            active: mail && app.snoozed && plain,
            on_press: Message::Later(LaterMsg::ShowSnoozed),
        },
        Place {
            label: "Programmés",
            icon: Icon::CalendarClock,
            count: scheduled,
            active: mail && app.scheduled_shown(),
            on_press: Message::Later(LaterMsg::ShowScheduled),
        },
        Place {
            label: "Envoyés",
            icon: Icon::Send,
            count: 0,
            active: folder(FolderRole::Sent),
            on_press: Message::FolderSelected(FolderRole::Sent),
        },
        Place {
            label: "Brouillons",
            icon: Icon::FileText,
            count: app.drafts_count,
            active: folder(FolderRole::Drafts),
            on_press: Message::FolderSelected(FolderRole::Drafts),
        },
    ];
    // Gmail's archive is "All Mail", which is not synced: no view.
    if app.archive_view || app.folder == FolderRole::Archive {
        list.push(Place {
            label: "Archives",
            icon: Icon::Archive,
            count: 0,
            active: folder(FolderRole::Archive),
            on_press: Message::FolderSelected(FolderRole::Archive),
        });
    }
    list.extend([
        Place {
            label: "Spam",
            icon: Icon::Shield,
            count: 0,
            active: folder(FolderRole::Junk),
            on_press: Message::FolderSelected(FolderRole::Junk),
        },
        Place {
            label: "Corbeille",
            icon: Icon::Trash,
            count: 0,
            active: folder(FolderRole::Trash),
            on_press: Message::FolderSelected(FolderRole::Trash),
        },
        Place {
            label: "Grand nettoyage",
            icon: Icon::Sparkles,
            count: 0,
            active: app.screen == Screen::Cleanup,
            on_press: Message::ShowCleanup,
        },
    ]);
    list
}

pub fn view(app: &App) -> Element<'_, Message> {
    if folded(app) {
        return folded_view(app);
    }
    let mut middle = Column::new().spacing(4).push(space_picker(app));
    if app.space_menu {
        middle = middle.push(space_menu(app));
    }
    for place in places(app) {
        middle = middle.push(entry(place));
    }
    middle = middle.push(section_title("Comptes"));
    for account in app.space_accounts() {
        let only = AccountFilter::Only(account.id);
        let active = app.filter == only;
        // A second click on the account chosen shows them all again.
        let filter = if active { AccountFilter::All } else { only };
        let line = row![
            container(Space::new().width(10).height(10))
                .style(theme::ringed_disc(super::to_color(account.color))),
            text(account.email.clone())
                .size(13)
                .font(if active { SEMIBOLD } else { theme::INTER })
                .wrapping(Wrapping::None),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        middle = middle.push(tip(
            button(container(line).clip(true))
                .width(Fill)
                .height(38)
                .padding([0, 12])
                .on_press(Message::FilterSelected(filter))
                .style(theme::side_entry(active)),
            if active {
                "Afficher tous les comptes".to_string()
            } else {
                format!("Seulement {}", account.name)
            },
            tooltip::Position::Right,
        ));
    }
    middle = middle.push(
        button(
            row![
                icons::icon(Icon::Plus, 15.0, Tone::Fixed(Color::WHITE)),
                text("Ajouter un compte").size(13).font(MEDIUM),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        )
        .width(Fill)
        .height(36)
        .padding([0, 10])
        .on_press(Message::Add(AddMsg::Open(None)))
        .style(theme::side_entry(false)),
    );

    let content = column![
        brand(),
        search(app),
        space().height(10),
        write_button(),
        space().height(18),
        scrollable(middle)
            .height(Fill)
            .style(side_scrollbar)
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::new()
                    .width(3)
                    .scroller_width(3)
                    .spacing(3),
            )),
        space().height(10),
        bottom(app),
    ]
    .padding(Padding {
        top: 8.0,
        right: 16.0,
        bottom: 12.0,
        left: 16.0,
    });
    container(content).width(WIDTH).height(Fill).into()
}

/// The logo tile and "Avero".
fn brand<'a>() -> Element<'a, Message> {
    container(
        button(
            row![
                container(icons::icon(Icon::Mark, 18.0, Tone::Accent))
                    .center(30)
                    .style(theme::logo_tile),
                text("Avero").size(19).font(BOLD).color(Color::WHITE),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        )
        .padding(0)
        .on_press(Message::ShowMail)
        .style(|_theme: &iced::Theme, _status| button::Style::default()),
    )
    .padding(Padding {
        top: 6.0,
        right: 6.0,
        bottom: 18.0,
        left: 6.0,
    })
    .into()
}

/// "Rechercher", with the search icon and "Ctrl K" (or a clear button
/// once something is typed).
fn search(app: &App) -> Element<'_, Message> {
    let input = text_input("Rechercher", &app.search_text)
        .id(SEARCH_ID)
        .on_input(Message::SearchChanged)
        .on_submit(Message::SearchSubmitted)
        .padding(Padding {
            top: 12.0,
            bottom: 12.0,
            left: 39.0,
            right: 58.0,
        })
        .size(14)
        .style(theme::search_input);
    let icon = container(icons::icon(Icon::Search, 17.0, Tone::Fixed(Color::WHITE)))
        .padding(Padding::new(0.0).left(12))
        .height(Fill)
        .center_y(Fill);
    let end: Element<'_, Message> = if app.search_text.is_empty() {
        text("Ctrl K")
            .size(12)
            .font(SEMIBOLD)
            .color(Color::WHITE)
            .into()
    } else {
        tip(
            button(icons::icon(Icon::Close, 14.0, Tone::Fixed(Color::WHITE)))
                .padding(5)
                .on_press(Message::SearchCleared)
                .style(theme::side_entry(false)),
            "Effacer (Échap)",
            tooltip::Position::Bottom,
        )
    };
    let end = container(end)
        .width(Fill)
        .height(Fill)
        .align_x(Alignment::End)
        .align_y(Alignment::Center)
        .padding(Padding::new(0.0).right(12));
    container(stack![input, icon, end])
        .height(42)
        .width(Fill)
        .into()
}

/// "Écrire": white, the main action.
fn write_button<'a>() -> Element<'a, Message> {
    button(
        container(
            row![
                icons::icon(Icon::Pencil, 17.0, Tone::Accent),
                text("Écrire").size(15).font(BOLD),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .center(Fill),
    )
    .width(Fill)
    .height(44)
    .padding(0)
    .on_press(Message::Compose(ComposeMsg::New))
    .style(theme::write_button)
    .into()
}

/// The space shown (icon, name, chevron); a click opens the menu.
fn space_picker(app: &App) -> Element<'_, Message> {
    let (icon, name) = match app.active_space() {
        Some(space) => (icons::space_icon(&space.icon), space.name.clone()),
        None => (Icon::Layers, "Tous les comptes".to_string()),
    };
    let chevron = if app.space_menu {
        Icon::ChevronUp
    } else {
        Icon::ChevronDown
    };
    let white = Tone::Fixed(Color::WHITE);
    tip(
        button(
            row![
                icons::icon(icon, 18.0, white),
                container(text(name).size(17).font(BOLD).wrapping(Wrapping::None)).clip(true),
                icons::icon(chevron, 14.0, white),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        )
        .height(36)
        .padding([0, 8])
        .on_press(Message::SpaceMenu(!app.space_menu))
        .style(theme::side_entry(false)),
        "Changer d'espace (Ctrl+1, Ctrl+2…, Ctrl+0 pour tous)",
        tooltip::Position::Right,
    )
}

/// The open menu of spaces: "Tous les comptes", each space with its
/// unread mail, then "Nouvel espace".
fn space_menu(app: &App) -> Element<'_, Message> {
    let white = Tone::Fixed(Color::WHITE);
    let line = |icon: Element<'static, Message>, label: String, unread: usize, active, message| {
        let mut content = Row::new()
            .spacing(10)
            .align_y(Alignment::Center)
            .push(icon)
            .push(
                container(text(label).size(13).font(MEDIUM).wrapping(Wrapping::None))
                    .width(Fill)
                    .clip(true),
            );
        if unread > 0 {
            content = content.push(text(unread.to_string()).size(12).font(SEMIBOLD));
        }
        button(content)
            .width(Fill)
            .height(34)
            .padding([0, 12])
            .on_press(message)
            .style(theme::side_entry(active))
    };
    let mut menu = Column::new().spacing(2).push(line(
        icons::icon(Icon::Layers, 15.0, white),
        "Tous les comptes".to_string(),
        app.space_unread(None),
        app.space.is_none(),
        Message::Space(SpaceMsg::Select(None)),
    ));
    for space in &app.spaces {
        menu = menu.push(line(
            icons::icon(icons::space_icon(&space.icon), 15.0, white),
            space.name.clone(),
            app.space_unread(Some(space.id)),
            app.space == Some(space.id),
            Message::Space(SpaceMsg::Select(Some(space.id))),
        ));
    }
    menu = menu.push(line(
        icons::icon(Icon::Plus, 15.0, white),
        "Nouvel espace".to_string(),
        0,
        false,
        Message::Space(SpaceMsg::New),
    ));
    container(menu)
        .padding(4)
        .style(|_theme: &iced::Theme| container::Style {
            background: Some(theme::alpha(Color::BLACK, 0.12).into()),
            border: iced::border::rounded(theme::CONTROL_RADIUS),
            ..container::Style::default()
        })
        .into()
}

/// A place: its name and, when it has some, its counter.
fn entry<'a>(place: Place) -> Element<'a, Message> {
    let mut content = Row::new().align_y(Alignment::Center).push(
        text(place.label)
            .size(14)
            .font(if place.active { BOLD } else { MEDIUM })
            .width(Fill),
    );
    if place.count > 0 {
        content = content.push(
            text(place.count.to_string())
                .size(13)
                .font(if place.active { BOLD } else { SEMIBOLD }),
        );
    }
    button(container(content).height(Fill).center_y(Fill))
        .width(Fill)
        .height(38)
        .padding([0, 12])
        .on_press(place.on_press)
        .style(theme::side_entry(place.active))
        .into()
}

fn section_title<'a>(title: &'a str) -> Element<'a, Message> {
    container(
        text(title.to_uppercase())
            .size(11)
            .font(BOLD)
            .color(Color::WHITE),
    )
    .padding(Padding {
        top: 18.0,
        right: 12.0,
        bottom: 6.0,
        left: 12.0,
    })
    .into()
}

/// The spaces as pills (the active one wider), "+" for a new space, and
/// the settings.
fn bottom(app: &App) -> Element<'_, Message> {
    let mut pills = Row::new().spacing(10).align_y(Alignment::Center);
    for (index, space) in app.spaces.iter().enumerate() {
        let active = app.space == Some(space.id);
        let pill = button(Space::new().width(0).height(0))
            .width(if active { 34 } else { 16 })
            .height(16)
            .padding(0)
            .on_press(Message::Space(SpaceMsg::Select(Some(space.id))))
            .style(theme::space_pill(super::list::space_color(space)));
        let label = if index < 9 {
            format!("Espace {} (Ctrl+{})", space.name, index + 1)
        } else {
            format!("Espace {}", space.name)
        };
        pills = pills.push(tip(pill, label, tooltip::Position::Top));
    }
    pills = pills.push(tip(
        button(center(icons::icon(
            Icon::Plus,
            18.0,
            Tone::Fixed(Color::WHITE),
        )))
        .width(28)
        .height(28)
        .padding(0)
        .on_press(Message::Space(SpaceMsg::New))
        .style(theme::side_entry(false)),
        "Nouvel espace",
        tooltip::Position::Top,
    ));
    row![
        settings_button(app, 34.0),
        container(pills).width(Fill).center_x(Fill),
        // Balances the settings button, so that the pills stay centred.
        Space::new().width(34).height(1),
    ]
    .align_y(Alignment::Center)
    .into()
}

fn settings_button(app: &App, size: f32) -> Element<'_, Message> {
    tip(
        button(center(icons::icon(
            Icon::Settings,
            18.0,
            Tone::Fixed(Color::WHITE),
        )))
        .width(size)
        .height(size)
        .padding(0)
        .on_press(Message::ShowSettings)
        .style(theme::side_entry(app.screen == Screen::Settings)),
        "Réglages",
        tooltip::Position::Top,
    )
}

/// Thin white scrollbars on the gradient.
fn side_scrollbar(
    theme: &iced::Theme,
    status: scrollable::Status,
) -> iced::widget::scrollable::Style {
    let mut style = theme::scrollbar(theme, status);
    let color = theme::alpha(Color::WHITE, 0.35);
    style.vertical_rail.scroller.background = color.into();
    style
}

// ------------------------------------------------------------ folded

/// The narrow side bar: icons with their names in tooltips.
fn folded_view(app: &App) -> Element<'_, Message> {
    let white = Tone::Fixed(Color::WHITE);
    let icon_button = |icon: Icon, label: String, active: bool, message: Message| {
        tip(
            button(center(icons::icon(icon, 19.0, white)))
                .width(40)
                .height(40)
                .padding(0)
                .on_press(message)
                .style(theme::side_entry(active)),
            label,
            tooltip::Position::Right,
        )
    };
    let mut middle = Column::new().spacing(4).align_x(Alignment::Center);
    for place in places(app) {
        let label = if place.count > 0 {
            format!("{} ({})", place.label, place.count)
        } else {
            place.label.to_string()
        };
        middle = middle.push(icon_button(place.icon, label, place.active, place.on_press));
    }
    middle = middle.push(space().height(12));
    for space in &app.spaces {
        let active = app.space == Some(space.id);
        middle = middle.push(tip(
            button(Space::new().width(0).height(0))
                .width(16)
                .height(if active { 30 } else { 16 })
                .padding(0)
                .on_press(Message::Space(SpaceMsg::Select(Some(space.id))))
                .style(theme::space_pill(super::list::space_color(space))),
            format!("Espace {}", space.name),
            tooltip::Position::Right,
        ));
    }
    middle = middle.push(icon_button(
        Icon::Layers,
        "Tous les comptes (Ctrl+0)".to_string(),
        false,
        Message::Space(SpaceMsg::Select(None)),
    ));
    let content = column![
        container(icons::icon(Icon::Mark, 18.0, Tone::Accent))
            .center(30)
            .style(theme::logo_tile),
        space().height(14),
        tip(
            button(center(icons::icon(Icon::Pencil, 18.0, Tone::Accent)))
                .width(40)
                .height(40)
                .padding(0)
                .on_press(Message::Compose(ComposeMsg::New))
                .style(theme::write_button),
            "Écrire (C)",
            tooltip::Position::Right,
        ),
        space().height(12),
        scrollable(middle)
            .height(Fill)
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::new().width(0).scroller_width(0),
            )),
        settings_button(app, 40.0),
    ]
    .spacing(4)
    .align_x(Alignment::Center)
    .padding([12, 0]);
    container(content)
        .width(FOLDED_WIDTH)
        .height(Length::Fill)
        .into()
}
