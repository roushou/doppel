//! A film's shapes (`doppel_scene::film`) drawn over a screen: what an
//! export burns into its video, drawn live, so the consoles and the films
//! show the same. This module draws; the film decides.

use doppel_desktop::geometry::Rect;
use doppel_scene::film::{Corner, Glyph, Line, Rgb, Shape, Spot};
use gpui::{
    BorderStyle, Bounds, Div, FontWeight, Hsla, ParentElement, PathBuilder, Pixels, Point, Styled,
    Window, canvas, div, point, px, quad, size,
};

use crate::screen::Picture;
use crate::ui::icons::Icon;
use crate::ui::theme::MONO;

/// `rgb` at `alpha`, 0 to 1.
fn colour(rgb: Rgb, alpha: f64) -> Hsla {
    let mut colour: Hsla = gpui::rgb(rgb.hex()).into();
    colour.a = alpha.clamp(0.0, 1.0) as f32;
    colour
}

/// White at `alpha`: a pointer's edge, a badge's symbol.
fn white(alpha: f64) -> Hsla {
    let mut white = gpui::white();
    white.a = alpha.clamp(0.0, 1.0) as f32;
    white
}

/// The icon a glyph shows on its badge.
const fn icon(glyph: Glyph) -> Icon {
    match glyph {
        Glyph::Press => Icon::Pointer,
        Glyph::Type => Icon::TextCursorInput,
        Glyph::Keys => Icon::Keyboard,
        Glyph::Command => Icon::Terminal,
        Glyph::Wait => Icon::Hourglass,
        Glyph::Look => Icon::Eye,
        Glyph::Point => Icon::MapPin,
        Glyph::Drag => Icon::Hand,
        Glyph::Scroll => Icon::ArrowDownUp,
        Glyph::Machine => Icon::Power,
        Glyph::Checkpoint => Icon::Flag,
        Glyph::Rollback => Icon::Rewind,
        Glyph::Done => Icon::Check,
    }
}

/// Where display points land in a window, and how big a display point is
/// there: the film's shapes scale with the picture, as they do in a video.
#[derive(Debug, Clone, Copy)]
struct Place {
    picture: Picture,
    /// Window points per display point.
    scale: f32,
}

impl Place {
    fn of(picture: Picture) -> Self {
        Self {
            picture,
            scale: picture.width / picture.points.width() as f32,
        }
    }

    /// A display spot, from the box's top left, in window points.
    fn spot(self, spot: Spot) -> (f32, f32) {
        self.picture.place(spot.x, spot.y)
    }

    fn length(self, length: f64) -> f32 {
        length as f32 * self.scale
    }

    /// A display rectangle in the window, the box at `origin`.
    fn bounds(self, rect: Rect, origin: Point<Pixels>) -> Bounds<Pixels> {
        let (x, y) = self.picture.place(rect.left(), rect.top());
        Bounds::new(
            origin + point(px(x), px(y)),
            size(
                px(self.length(rect.width())),
                px(self.length(rect.height())),
            ),
        )
    }

    fn at(self, spot: Spot, origin: Point<Pixels>) -> Point<Pixels> {
        let (x, y) = self.spot(spot);
        origin + point(px(x), px(y))
    }
}

/// `shapes` over a screen whose picture is at `picture`, bottom first.
/// Badges and tags are elements over the rest, which one canvas paints.
pub(crate) fn layer(shapes: Vec<Shape>, picture: Picture) -> Div {
    let place = Place::of(picture);
    let mut layer = div().absolute().inset_0().overflow_hidden();
    let mut on_top = Vec::new();
    let mut painted = Vec::new();
    for shape in shapes {
        match shape {
            Shape::Glyph { .. } | Shape::Tag { .. } => on_top.push(shape),
            Shape::Frame { .. }
            | Shape::Ring { .. }
            | Shape::Stroke { .. }
            | Shape::Pointer { .. }
            | Shape::Veil { .. }
            | Shape::Brackets { .. } => painted.push(shape),
        }
    }
    layer = layer.child(
        canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, (), window: &mut Window, _| {
                for shape in &painted {
                    paint(shape, place, bounds.origin, window);
                }
            },
        )
        .absolute()
        .inset_0(),
    );
    for shape in &on_top {
        if let Some(element) = element(shape, place) {
            layer = layer.child(element);
        }
    }
    layer
}

/// Paints one of the shapes a canvas draws.
fn paint(shape: &Shape, place: Place, origin: Point<Pixels>, window: &mut Window) {
    match shape {
        Shape::Frame {
            rect,
            colour: rgb,
            wash,
            width,
            opacity,
        } => window.paint_quad(quad(
            place.bounds(*rect, origin),
            px(0.),
            colour(*rgb, wash * opacity),
            px(place.length(*width)),
            colour(*rgb, *opacity),
            BorderStyle::Solid,
        )),
        Shape::Ring {
            centre,
            radius,
            colour: rgb,
            wash,
            width,
            opacity,
        } => {
            let r = place.length(*radius);
            let c = place.at(*centre, origin);
            window.paint_quad(quad(
                Bounds::new(c - point(px(r), px(r)), size(px(2. * r), px(2. * r))),
                px(r),
                colour(*rgb, wash * opacity),
                px(place.length(*width)),
                colour(*rgb, *opacity),
                BorderStyle::Solid,
            ));
        }
        Shape::Stroke {
            from,
            to,
            colour: rgb,
            width,
            line,
            opacity,
        } => stroke(
            place,
            origin,
            (*from, *to),
            colour(*rgb, *opacity),
            *width,
            *line,
            window,
        ),
        Shape::Pointer {
            tip,
            colour: rgb,
            opacity,
        } => pointer(
            place.at(*tip, origin),
            place.scale,
            colour(*rgb, *opacity),
            white(*opacity),
            window,
        ),
        Shape::Veil {
            lit,
            colour: rgb,
            opacity,
        } => {
            // Even-odd: the whole picture, less each lit rectangle (no two overlap).
            let picture = place.picture;
            let mut path = PathBuilder::fill();
            let mut rect = |bounds: Bounds<Pixels>| {
                path.move_to(bounds.origin);
                path.line_to(bounds.top_right());
                path.line_to(bounds.bottom_right());
                path.line_to(bounds.bottom_left());
                path.close();
            };
            rect(Bounds::new(
                origin + point(px(picture.left), px(picture.top)),
                size(px(picture.width), px(picture.height)),
            ));
            for piece in lit.rects() {
                rect(place.bounds(*piece, origin));
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, colour(*rgb, *opacity));
            }
        }
        Shape::Brackets {
            rect,
            arm,
            colour: rgb,
            width,
            opacity,
        } => {
            let bounds = place.bounds(*rect, origin);
            let arm = px(place
                .length(*arm)
                .min(f32::from(bounds.size.width) / 2.)
                .min(f32::from(bounds.size.height) / 2.));
            let mut path = PathBuilder::stroke(px(place.length(*width)));
            for (corner, dx, dy) in [
                (bounds.origin, arm, arm),
                (bounds.top_right(), -arm, arm),
                (bounds.bottom_left(), arm, -arm),
                (bounds.bottom_right(), -arm, -arm),
            ] {
                path.move_to(corner + point(dx, px(0.)));
                path.line_to(corner);
                path.line_to(corner + point(px(0.), dy));
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, colour(*rgb, *opacity));
            }
        }
        // Elements, not painted.
        Shape::Glyph { .. } | Shape::Tag { .. } => {}
    }
}

/// A line from `ends.0` to `ends.1`; an arrow's head ends at its end.
fn stroke(
    place: Place,
    origin: Point<Pixels>,
    (from, to): (Spot, Spot),
    ink: Hsla,
    width: f64,
    line: Line,
    window: &mut Window,
) {
    let from = place.at(from, origin);
    let mut to = place.at(to, origin);
    let mut path = PathBuilder::stroke(px(place.length(width)));
    match line {
        Line::Dashed => {
            path = path.dash_array(&[px(place.length(3.)), px(place.length(5.))]);
        }
        Line::Arrow => {
            let (dx, dy) = (f32::from(to.x - from.x), f32::from(to.y - from.y));
            let length = dx.hypot(dy).max(1.);
            let (ux, uy) = (dx / length, dy / length);
            let (head, half) = (place.length(14.), place.length(8.4));
            let base = to - point(px(ux * head), px(uy * head));
            let mut tip = PathBuilder::fill();
            tip.move_to(to);
            tip.line_to(base + point(px(-uy * half), px(ux * half)));
            tip.line_to(base + point(px(uy * half), px(-ux * half)));
            tip.close();
            if let Ok(tip) = tip.build() {
                window.paint_path(tip, ink);
            }
            to = base;
        }
        Line::Solid => {}
    }
    path.move_to(from);
    path.line_to(to);
    if let Ok(path) = path.build() {
        window.paint_path(path, ink);
    }
}

/// The agent's arrow, its tip at `tip`, 22 display points tall, edged white.
fn pointer(tip: Point<Pixels>, scale: f32, fill: Hsla, edge: Hsla, window: &mut Window) {
    const OUTLINE: [(f32, f32); 7] = [
        (4.0, 2.5),
        (4.0, 19.0),
        (8.6, 14.8),
        (11.6, 21.4),
        (14.6, 20.0),
        (11.7, 13.6),
        (18.0, 13.6),
    ];
    let unit = 22.0 / 24.0 * scale;
    let at = |(x, y): (f32, f32)| tip + point(px((x - 4.0) * unit), px((y - 2.5) * unit));
    let trace = |mut path: PathBuilder| {
        path.move_to(at(OUTLINE[0]));
        for corner in &OUTLINE[1..] {
            path.line_to(at(*corner));
        }
        path.close();
        path.build()
    };
    if let Ok(body) = trace(PathBuilder::fill()) {
        window.paint_path(body, fill);
    }
    if let Ok(outline) = trace(PathBuilder::stroke(px(1.5 * scale))) {
        window.paint_path(outline, edge);
    }
}

/// A badge or a tag, as an element.
fn element(shape: &Shape, place: Place) -> Option<Div> {
    match shape {
        Shape::Glyph {
            centre,
            glyph,
            colour: rgb,
            size: side,
            opacity,
        } => {
            let side = place.length(*side);
            let (x, y) = place.spot(*centre);
            Some(
                div()
                    .absolute()
                    .left(px(x - side / 2.))
                    .top(px(y - side / 2.))
                    .size(px(side))
                    .rounded_full()
                    .bg(colour(*rgb, 1.0))
                    .shadow_md()
                    .flex()
                    .items_center()
                    .justify_center()
                    .opacity(*opacity as f32)
                    .child(icon(*glyph).svg(side * 0.55, white(1.0))),
            )
        }
        Shape::Tag {
            at,
            corner,
            text,
            ink,
            plate,
            opacity,
        } => {
            let (x, y) = place.spot(*at);
            let tag = div()
                .absolute()
                .px(px(place.length(6.)))
                .py(px(place.length(3.)))
                .bg(colour(*plate, 1.0))
                .text_color(colour(*ink, 1.0))
                .text_size(px(place.length(11.)))
                .font_weight(FontWeight::MEDIUM)
                .font_family(MONO)
                .whitespace_nowrap()
                .opacity(*opacity as f32)
                .child(text.clone());
            // An anchor at the spot, the tag growing away from its corner.
            let anchor = div().absolute().w(px(0.)).h(px(0.)).flex();
            Some(match corner {
                Corner::TopLeft => anchor.left(px(x)).top(px(y)).child(tag.relative()),
                Corner::TopRight => anchor
                    .left(px(x))
                    .top(px(y))
                    .justify_end()
                    .child(tag.relative().flex_none()),
                Corner::BottomLeft => anchor
                    .left(px(x))
                    .top(px(y))
                    .items_end()
                    .child(tag.relative().flex_none()),
            })
        }
        Shape::Frame { .. }
        | Shape::Ring { .. }
        | Shape::Stroke { .. }
        | Shape::Pointer { .. }
        | Shape::Veil { .. }
        | Shape::Brackets { .. } => None,
    }
}
