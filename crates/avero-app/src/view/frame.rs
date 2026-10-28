//! The window's frame: the gradient, the side bar on it, and the card
//! with its shadow.
//!
//! The software renderer repaints every quad that touches a changed
//! area, whole: a window-sized gradient under the card cost more than
//! the rest of a frame each time the list scrolled or a line was
//! hovered. The frame paints the gradient and the shadow only around the
//! card, in pieces that meet exactly.

use super::{MARGIN, SHADOW_RINGS};
use crate::theme::{self, WINDOW_RADIUS};
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Quad, Renderer as _};
use iced::advanced::widget::{self, Tree, Widget};
use iced::advanced::{Clipboard, Shell, mouse, overlay};
use iced::gradient::{ColorStop, Linear};
use iced::{
    Background, Border, Color, Element, Event, Length, Point, Radians, Rectangle, Size, Theme,
    Vector, border,
};

/// The side bar on the left, the card on the rest of the window.
pub struct Frame<'a, Message> {
    /// The side bar, then the card.
    children: [Element<'a, Message>; 2],
}

pub fn frame<'a, Message>(
    sidebar: Element<'a, Message>,
    card: Element<'a, Message>,
) -> Frame<'a, Message> {
    Frame {
        children: [sidebar, card],
    }
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Frame<'_, Message> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = limits.max();
        let rings = SHADOW_RINGS as f32;
        // The side bar runs down to the bottom of the shadow.
        let sidebar_height = (size.height - 2.0 * MARGIN + 2.0 * rings).max(0.0);
        let [sidebar, card] = &mut self.children;
        let sidebar = sidebar
            .as_widget_mut()
            .layout(
                &mut tree.children[0],
                renderer,
                &layout::Limits::new(Size::ZERO, Size::new(size.width, sidebar_height)),
            )
            .move_to(Point::new(0.0, MARGIN));
        let card_size = Size::new(
            (size.width - sidebar.size().width - rings - MARGIN).max(0.0),
            (size.height - 2.0 * MARGIN).max(0.0),
        );
        let card = card
            .as_widget_mut()
            .layout(
                &mut tree.children[1],
                renderer,
                &layout::Limits::new(card_size, card_size),
            )
            .move_to(Point::new(sidebar.size().width + rings, MARGIN));
        layout::Node::with_children(size, vec![sidebar, card])
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let window = layout.bounds();
        let mut children = layout.children();
        let (Some(sidebar), Some(card)) = (children.next(), children.next()) else {
            return;
        };
        let frame = &theme::tokens(theme).frame;
        let linear = theme::frame_gradient(frame);
        for piece in around(window, card.bounds(), WINDOW_RADIUS) {
            for slice in slices(piece, linear.angle) {
                // A pixel more on every side: pieces overlap instead of
                // leaving a faint line where they meet between two
                // pixels, and the card's edges blend into the gradient.
                let slice = slice.expand(1.0);
                fill(
                    renderer,
                    slice,
                    piece_gradient(&linear, window, slice),
                    Radius::default(),
                );
            }
        }
        for (piece, color, radius) in shadow(card.bounds()) {
            fill(renderer, piece, color.into(), radius);
        }
        for ((child, state), layout) in self
            .children
            .iter()
            .zip(&tree.children)
            .zip([sidebar, card])
        {
            child
                .as_widget()
                .draw(state, renderer, theme, style, layout, cursor, viewport);
        }
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        for ((child, state), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                state, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, state), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(state, layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        for ((child, state), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child
                .as_widget_mut()
                .operate(state, layout, renderer, operation);
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        overlay::from_children(
            &mut self.children,
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: 'a> From<Frame<'a, Message>> for Element<'a, Message> {
    fn from(frame: Frame<'a, Message>) -> Self {
        Element::new(frame)
    }
}

fn fill(renderer: &mut iced::Renderer, bounds: Rectangle, background: Background, radius: Radius) {
    if bounds.width <= 0.0 || bounds.height <= 0.0 {
        return;
    }
    renderer.fill_quad(
        Quad {
            bounds,
            border: Border {
                radius,
                ..Border::default()
            },
            ..Quad::default()
        },
        background,
    );
}

type Radius = border::Radius;

/// The window minus the card: four bands, and the squares under the
/// card's rounded corners.
fn around(window: Rectangle, card: Rectangle, radius: f32) -> Vec<Rectangle> {
    let (left, top) = (card.x, card.y);
    let (right, bottom) = (card.x + card.width, card.y + card.height);
    let (window_right, window_bottom) = (window.x + window.width, window.y + window.height);
    vec![
        Rectangle::new(window.position(), Size::new(window.width, top - window.y)),
        Rectangle::new(
            Point::new(window.x, bottom),
            Size::new(window.width, window_bottom - bottom),
        ),
        Rectangle::new(
            Point::new(window.x, top),
            Size::new(left - window.x, card.height),
        ),
        Rectangle::new(
            Point::new(right, top),
            Size::new(window_right - right, card.height),
        ),
        Rectangle::new(Point::new(left, top), Size::new(radius, radius)),
        Rectangle::new(Point::new(right - radius, top), Size::new(radius, radius)),
        Rectangle::new(Point::new(left, bottom - radius), Size::new(radius, radius)),
        Rectangle::new(
            Point::new(right - radius, bottom - radius),
            Size::new(radius, radius),
        ),
    ]
}

/// `piece` cut into strips thin enough for its own gradient to match the
/// window's. The renderer spreads a gradient over the larger of the
/// quad's two extents along its direction, not their sum, and repeats the
/// end colours past it: over a wide piece the corners would drift from
/// the window's colours (5/255 on the side bar); over strips a few
/// pixels thick they stay within half a step.
fn slices(piece: Rectangle, angle: Radians) -> Vec<Rectangle> {
    /// What a strip may leave past the ends of its gradient, in pixels.
    const DRIFT: f32 = 2.0;
    let direction = angle.0 - std::f32::consts::FRAC_PI_2;
    let (dx, dy) = (direction.cos().abs(), direction.sin().abs());
    let across_x = dx * piece.width / 2.0;
    let across_y = dy * piece.height / 2.0;
    let drift = across_x.min(across_y);
    if drift <= DRIFT {
        return vec![piece];
    }
    let count = (drift / DRIFT).ceil() as usize;
    (0..count)
        .map(|i| {
            let (from, to) = (i as f32 / count as f32, (i + 1) as f32 / count as f32);
            if across_x <= across_y {
                Rectangle {
                    x: piece.x + from * piece.width,
                    width: (to - from) * piece.width,
                    ..piece
                }
            } else {
                Rectangle {
                    y: piece.y + from * piece.height,
                    height: (to - from) * piece.height,
                    ..piece
                }
            }
        })
        .collect()
}

/// The shadow's rings, each cut into its visible parts: the two sides,
/// the bottom, and the four corners (the only rounded pieces; the card
/// hides their inner part).
fn shadow(card: Rectangle) -> Vec<(Rectangle, Color, Radius)> {
    let mut pieces = Vec::new();
    for depth in 0..SHADOW_RINGS {
        let grow = (depth + 1) as f32;
        let ring = Rectangle {
            x: card.x - grow,
            y: card.y,
            width: card.width + 2.0 * grow,
            height: card.height + 2.0 * grow,
        };
        let color = theme::shadow_color(depth);
        let r = WINDOW_RADIUS + 1.0 + depth as f32;
        // The renderer rounds a corner by at most half the quad's side:
        // corner pieces are twice the radius.
        let span = 2.0 * r;
        let (right, bottom) = (ring.x + ring.width, ring.y + ring.height);
        let corner = Size::new(span, span);
        let side = Size::new(grow, ring.height - 2.0 * span);
        pieces.extend([
            (
                Rectangle::new(Point::new(ring.x, ring.y + span), side),
                color,
                Radius::default(),
            ),
            (
                Rectangle::new(Point::new(card.x + card.width, ring.y + span), side),
                color,
                Radius::default(),
            ),
            (
                Rectangle::new(
                    Point::new(ring.x + span, card.y + card.height),
                    Size::new(ring.width - 2.0 * span, 2.0 * grow),
                ),
                color,
                Radius::default(),
            ),
            (
                Rectangle::new(ring.position(), corner),
                color,
                Radius::default().top_left(r),
            ),
            (
                Rectangle::new(Point::new(right - span, ring.y), corner),
                color,
                Radius::default().top_right(r),
            ),
            (
                Rectangle::new(Point::new(ring.x, bottom - span), corner),
                color,
                Radius::default().bottom_left(r),
            ),
            (
                Rectangle::new(Point::new(right - span, bottom - span), corner),
                color,
                Radius::default().bottom_right(r),
            ),
        ]);
    }
    pieces
}

/// The part of `linear`, drawn over `whole`, that falls on `piece`, as a
/// gradient of its own: the renderer stretches a gradient over the quad
/// it fills, so the stops are moved to where the whole one has them.
pub fn piece_gradient(linear: &Linear, whole: Rectangle, piece: Rectangle) -> Background {
    let (start, end) = linear.angle.to_distance(&whole);
    let (piece_start, piece_end) = linear.angle.to_distance(&piece);
    let along = end - start;
    let length = along.x * along.x + along.y * along.y;
    if length <= f32::EPSILON {
        return Background::Gradient((*linear).into());
    }
    // Position on the whole gradient (0 at its start, 1 at its end).
    let at = |point: Point| {
        let offset = point - start;
        (offset.x * along.x + offset.y * along.y) / length
    };
    let (from, to) = (at(piece_start), at(piece_end));
    let stops: Vec<ColorStop> = linear.stops.iter().flatten().copied().collect();
    let mut piece_linear = Linear::new(Radians(linear.angle.0));
    piece_linear = piece_linear.add_stop(0.0, color_at(&stops, from));
    if to - from > f32::EPSILON {
        for stop in &stops {
            let offset = (stop.offset - from) / (to - from);
            if offset > 0.0 && offset < 1.0 {
                piece_linear = piece_linear.add_stop(offset, stop.color);
            }
        }
    }
    piece_linear = piece_linear.add_stop(1.0, color_at(&stops, to));
    Background::Gradient(piece_linear.into())
}

/// The colour of a gradient at `offset`, past its ends included.
fn color_at(stops: &[ColorStop], offset: f32) -> Color {
    let Some(first) = stops.first() else {
        return Color::TRANSPARENT;
    };
    if offset <= first.offset {
        return first.color;
    }
    for pair in stops.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if offset <= b.offset {
            let span = b.offset - a.offset;
            let t = if span > 0.0 {
                (offset - a.offset) / span
            } else {
                1.0
            };
            return mix(a.color, b.color, t);
        }
    }
    stops[stops.len() - 1].color
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::Degrees;

    fn sample(linear: &Linear, bounds: Rectangle, point: Point) -> Color {
        let (start, end) = linear.angle.to_distance(&bounds);
        let along = end - start;
        let offset = point - start;
        let t = (offset.x * along.x + offset.y * along.y) / (along.x * along.x + along.y * along.y);
        let stops: Vec<ColorStop> = linear.stops.iter().flatten().copied().collect();
        color_at(&stops, t)
    }

    #[test]
    fn pieces_continue_the_whole_gradient() {
        let whole = Rectangle::new(Point::ORIGIN, Size::new(1400.0, 900.0));
        let linear = Linear::new(Degrees(155.0))
            .add_stop(0.0, Color::from_rgb8(0x4F, 0x3B, 0xD1))
            .add_stop(0.45, Color::from_rgb8(0x5B, 0x4F, 0xE8))
            .add_stop(1.0, Color::from_rgb8(0xA3, 0x5B, 0xD0));
        let card = Rectangle::new(Point::new(275.0, 16.0), Size::new(1109.0, 868.0));
        for piece in around(whole, card, WINDOW_RADIUS)
            .into_iter()
            .flat_map(|piece| slices(piece, linear.angle))
        {
            let Background::Gradient(iced::Gradient::Linear(part)) =
                piece_gradient(&linear, whole, piece)
            else {
                panic!("a gradient");
            };
            for (fx, fy) in [(0.0, 0.0), (1.0, 1.0), (0.5, 0.5), (0.0, 1.0), (1.0, 0.2)] {
                let point = Point::new(piece.x + fx * piece.width, piece.y + fy * piece.height);
                let (a, b) = (sample(&linear, whole, point), sample(&part, piece, point));
                for (x, y) in [(a.r, b.r), (a.g, b.g), (a.b, b.b)] {
                    assert!(
                        (x - y).abs() < 0.002,
                        "{piece:?} at {point:?}: {a:?} / {b:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_gradient_leaves_the_card_uncovered() {
        let whole = Rectangle::new(Point::ORIGIN, Size::new(1400.0, 900.0));
        let card = Rectangle::new(Point::new(275.0, 16.0), Size::new(1109.0, 868.0));
        let inner = Rectangle::new(
            Point::new(card.x + WINDOW_RADIUS, card.y + WINDOW_RADIUS),
            Size::new(
                card.width - 2.0 * WINDOW_RADIUS,
                card.height - 2.0 * WINDOW_RADIUS,
            ),
        );
        for piece in around(whole, card, WINDOW_RADIUS) {
            assert!(piece.intersection(&inner).is_none(), "{piece:?}");
        }
    }
}
