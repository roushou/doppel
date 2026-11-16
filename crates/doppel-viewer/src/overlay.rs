//! What an agent does, drawn over its machine's screen, and the strip of
//! its calls under it: the browser viewer's `Overlay.svelte` and
//! `Timeline.svelte`. The scene comes from [`crate::timeline`]; this module
//! only draws it, for the instant `now`.

use gpui::{
    AnyElement, Bounds, Div, FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement,
    PathBuilder, Pixels, StatefulInteractiveElement, Styled, Window, canvas, div, fill,
    linear_color_stop, linear_gradient, pattern_slash, point, prelude::FluentBuilder, px, relative,
    size,
};

use crate::icons::Icon;
use crate::screen::Picture;
use crate::theme::{Hue, MONO, Theme};
use crate::timeline::{
    CALLOUT_FADE_MS, CalloutOn, CalloutShape, Family, Gesture, Mark, Moment, Scene, Shown, swept,
};
use crate::ui;

/// How long the agent's pointer stays once it stops acting.
const POINTER_MS: f64 = 6000.0;
/// How long the pointer takes to glide to where the agent acts next.
const GLIDE_MS: f64 = 450.0;
/// How long a ripple takes to spread and fade.
const RIPPLE_MS: f64 = 900.0;
/// The widest a caption gets, and how far from its pointer it sits.
const CAPTION_MAX: f32 = 300.;
const CAPTION_OFFSET: (f32, f32) = (18., 20.);

/// Which side of its pointer a caption goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Right,
    /// Near the picture's right edge, where a caption on the right would be cut off.
    Left,
}

/// The side a caption of a pointer at `x` goes, with the picture's right edge at `edge`.
fn caption_side(x: f32, edge: f32) -> Side {
    if x + CAPTION_OFFSET.0 + CAPTION_MAX > edge {
        Side::Left
    } else {
        Side::Right
    }
}

/// The agent's pointer, gliding from where it was to where it acts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Glide {
    from: (f64, f64),
    to: (f64, f64),
    since: f64,
}

impl Glide {
    /// Follows the scene's pointer: a new place starts a glide from where
    /// the pointer is now.
    pub(crate) fn follow(glide: Option<Self>, to: (f64, f64), now: f64) -> Self {
        match glide {
            // The clock went back (a replay seeked): start afresh.
            Some(glide) if now < glide.since => Self {
                from: to,
                to,
                since: now,
            },
            Some(glide) if glide.to == to => glide,
            Some(glide) => Self {
                from: glide.at(now),
                to,
                since: now,
            },
            None => Self {
                from: to,
                to,
                since: now,
            },
        }
    }

    /// Each agent's pointer in `scene`, by session, following it: a session
    /// gone from the scene is forgotten.
    pub(crate) fn follow_scene(
        glides: &mut std::collections::HashMap<u64, Self>,
        scene: &Scene,
        now: f64,
    ) {
        glides.retain(|session, _| scene.agents.iter().any(|agent| agent.session == *session));
        for agent in &scene.agents {
            if let Some(at) = agent.cursor {
                let glide =
                    Self::follow(glides.get(&agent.session).copied(), (at.x(), at.y()), now);
                glides.insert(agent.session, glide);
            }
        }
    }

    /// Where the pointer is at `now`, in display points.
    pub(crate) fn at(&self, now: f64) -> (f64, f64) {
        let t = ease_out(((now - self.since) / GLIDE_MS).clamp(0.0, 1.0));
        (
            self.from.0 + (self.to.0 - self.from.0) * t,
            self.from.1 + (self.to.1 - self.from.1) * t,
        )
    }

    /// Whether it's still moving.
    pub(crate) fn moving(&self, now: f64) -> bool {
        now - self.since < GLIDE_MS
    }
}

fn ease_out(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
}

/// The icon of a call's gesture.
const fn gesture_icon(gesture: Gesture) -> Icon {
    match gesture {
        Gesture::Click | Gesture::DoubleClick | Gesture::SecondaryClick => Icon::MousePointerClick,
        Gesture::Move => Icon::Move,
        Gesture::Drag => Icon::Hand,
        Gesture::Scroll => Icon::ArrowDownUp,
        Gesture::Key => Icon::Keyboard,
        Gesture::Type => Icon::TextCursorInput,
        Gesture::Act => Icon::MousePointer2,
        Gesture::Look => Icon::ScanEye,
        Gesture::Wait => Icon::Hourglass,
        Gesture::Run => Icon::Terminal,
        Gesture::Lifecycle => Icon::Power,
        Gesture::Mark => Icon::Highlighter,
        Gesture::Other => Icon::Bot,
    }
}

/// A caption in the agent's colour: what it's doing, or why it failed.
/// Square and flat, in small mono; the caller upper-cases its words.
/// How far below the pointer's caption the agent's reason sits.
const WHY_BELOW: f32 = 24.;

fn chip(fill: Hsla, theme: &Theme) -> Div {
    div()
        .absolute()
        .flex()
        .items_center()
        .gap(px(6.))
        .pl(px(6.))
        .pr(px(8.))
        .py(px(4.))
        .bg(fill)
        .text_color(theme.white)
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .font_family(MONO)
        .whitespace_nowrap()
        .max_w(px(CAPTION_MAX))
        .overflow_hidden()
}

/// The scene over a screen whose picture is at `picture`: each agent's
/// pointer, gliding as `glide` says for its session, in its session's hue,
/// named by its client when several act.
pub(crate) fn layer(
    scene: &Scene,
    glide: impl Fn(u64) -> Option<Glide>,
    picture: Picture,
    now: f64,
    theme: &Theme,
) -> impl IntoElement {
    let mut layer = div().absolute().inset_0().overflow_hidden();
    let named = scene.agents.len() > 1;
    // One band at a time, the latest look's: several mixed read as mud.
    if let Some((start, session)) = scene
        .agents
        .iter()
        .filter_map(|agent| agent.scan.map(|start| (start, agent.session)))
        .max_by(|a, b| a.0.total_cmp(&b.0))
    {
        layer = layer.child(sweep(picture, swept(start, now) as f32, Hue::of(session)));
    }
    for agent in &scene.agents {
        let hue = Hue::of(agent.session);
        for mark in &agent.marks {
            layer = layer.child(mark_element(mark, picture, now, hue, theme));
        }
    }
    for shown in &scene.callouts {
        layer = layer.child(callout_element(shown, picture, now, theme));
    }
    for agent in &scene.agents {
        let hue = Hue::of(agent.session);
        let current = agent.current.as_ref().map(|(call, _)| call);
        let words = |call: &crate::timeline::Call| {
            ui::upper(&if named && !agent.client.is_empty() {
                format!("{} · {}", agent.client, call.describe())
            } else {
                call.describe()
            })
        };
        let cursor = glide(agent.session)
            .filter(|_| agent.current.is_some() || now - agent.active < POINTER_MS)
            .map(|glide| glide.at(now));
        if let Some((x, y)) = cursor {
            let (x, y) = picture.place(x, y);
            layer = layer.child(arrow(x, y, hue.fill, theme));
            let caption = match current {
                Some(call) => chip(hue.ink, theme)
                    .child(gesture_icon(call.gesture).svg(13., theme.white))
                    .child(div().overflow_hidden().text_ellipsis().child(words(call))),
                None if named && !agent.client.is_empty() => chip(hue.ink, theme)
                    .child(Icon::Bot.svg(13., theme.white))
                    .child(ui::upper(&agent.client)),
                None => chip(hue.ink, theme)
                    .p(px(4.))
                    .child(Icon::Bot.svg(13., theme.white)),
            };
            let side = caption_side(x, picture.left + picture.width);
            let place = |chip: Div, below: f32| {
                let top = px(y + CAPTION_OFFSET.1 + below);
                match side {
                    Side::Right => chip.left(px(x + CAPTION_OFFSET.0)).top(top),
                    // A zero-wide anchor just left of the pointer, the caption
                    // ending at it.
                    Side::Left => div()
                        .absolute()
                        .left(px(x - 6.))
                        .top(top)
                        .w(px(0.))
                        .flex()
                        .justify_end()
                        .child(chip.relative().flex_none()),
                }
            };
            layer = layer.child(place(caption, 0.));
            // Why it does it, in its own words, under what it does.
            if let Some(why) = current.and_then(|call| call.why.as_deref()) {
                let said = chip(hue.fill, theme).text_color(hue.ink).child(
                    div()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(why.to_owned()),
                );
                layer = layer.child(place(said, WHY_BELOW));
            }
        }
        // A call with no place shows only in the tab above the screen and
        // the trace: nothing over the guest's own menu bar.
    }
    layer
}

/// A soft band sweeping down the screen while an agent reads it, `sweep`
/// (0 to 1) of the way down.
fn sweep(picture: Picture, sweep: f32, hue: Hue) -> Div {
    let band = picture.height * 0.3;
    let top = -band + sweep * (picture.height + band);
    div()
        .absolute()
        .left(px(picture.left))
        .top(px(picture.top))
        .w(px(picture.width))
        .h(px(picture.height))
        .overflow_hidden()
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(px(top))
                .h(px(band))
                .bg(linear_gradient(
                    180.,
                    linear_color_stop(hue.fill.opacity(0.0), 0.0),
                    linear_color_stop(hue.fill.opacity(0.55), 1.0),
                )),
        )
}

fn mark_element(mark: &Mark, picture: Picture, now: f64, hue: Hue, theme: &Theme) -> AnyElement {
    let since = mark.since();
    let fading = mark.opacity(now) as f32;
    match mark {
        Mark::Ripple { at, gesture, .. } => {
            let (x, y) = picture.place(at.x(), at.y());
            let rounded = match gesture {
                Gesture::Scroll => Rounding::Square,
                Gesture::Click
                | Gesture::DoubleClick
                | Gesture::SecondaryClick
                | Gesture::Move
                | Gesture::Drag
                | Gesture::Key
                | Gesture::Type
                | Gesture::Act
                | Gesture::Look
                | Gesture::Wait
                | Gesture::Run
                | Gesture::Lifecycle
                | Gesture::Mark
                | Gesture::Other => Rounding::Round,
            };
            let first = ripple(x, y, now - since, hue.fill, hue.soft, rounded);
            match gesture {
                Gesture::DoubleClick => div()
                    .absolute()
                    .inset_0()
                    .child(first)
                    .child(ripple(
                        x,
                        y,
                        now - since - 140.0,
                        hue.fill,
                        hue.soft,
                        rounded,
                    ))
                    .into_any_element(),
                Gesture::SecondaryClick => first.border_dashed().into_any_element(),
                Gesture::Click
                | Gesture::Move
                | Gesture::Drag
                | Gesture::Scroll
                | Gesture::Key
                | Gesture::Type
                | Gesture::Act
                | Gesture::Look
                | Gesture::Wait
                | Gesture::Run
                | Gesture::Lifecycle
                | Gesture::Mark
                | Gesture::Other => first.into_any_element(),
            }
        }
        Mark::Box { frame, label, .. } => {
            let (left, top) = picture.place(frame.left(), frame.top());
            let (right, bottom) =
                picture.place(frame.left() + frame.width(), frame.top() + frame.height());
            div()
                .absolute()
                .left(px(left))
                .top(px(top))
                .w(px(right - left))
                .h(px(bottom - top))
                .border_2()
                .border_color(hue.fill)
                .bg(hue.soft)
                .opacity(fading)
                .when(!label.is_empty(), |frame| {
                    frame.child(
                        div()
                            .absolute()
                            .left(px(-2.))
                            .top(px(-23.))
                            .px(px(6.))
                            .py(px(2.))
                            .bg(hue.fill)
                            .text_color(theme.white)
                            .font_family(MONO)
                            .text_size(px(10.5))
                            .font_weight(FontWeight::MEDIUM)
                            .whitespace_nowrap()
                            .child(label.clone()),
                    )
                })
                .into_any_element()
        }
        Mark::Path { from, to, .. } => {
            let from = picture.place(from.x(), from.y());
            let to = picture.place(to.x(), to.y());
            let agent = hue.fill;
            let white = theme.white;
            div()
                .absolute()
                .inset_0()
                .opacity(fading)
                .child(
                    canvas(
                        |_, _, _| {},
                        move |bounds: Bounds<Pixels>, (), window: &mut Window, _| {
                            let at = |(x, y): (f32, f32)| {
                                point(bounds.origin.x + px(x), bounds.origin.y + px(y))
                            };
                            // Dashed, 3 on and 5 off, square-ended.
                            let (dx, dy) = (to.0 - from.0, to.1 - from.1);
                            let length = dx.hypot(dy);
                            if length < 1. {
                                return;
                            }
                            let (ux, uy) = (dx / length, dy / length);
                            let mut along = 0.;
                            while along < length {
                                let end = (along + 3.).min(length);
                                let mut dash = PathBuilder::stroke(px(3.));
                                dash.move_to(at((from.0 + ux * along, from.1 + uy * along)));
                                dash.line_to(at((from.0 + ux * end, from.1 + uy * end)));
                                if let Ok(dash) = dash.build() {
                                    window.paint_path(dash, agent);
                                }
                                along += 8.;
                            }
                        },
                    )
                    .size_full(),
                )
                .child(spot(from, 4., agent, None))
                .child(spot(to, 6., white, Some(agent)))
                .into_any_element()
        }
        Mark::Failed { at, reason, .. } => {
            let (x, y) = picture.place(at.x(), at.y());
            let failed = gpui::rgba(0xff6b_6bff).into();
            div()
                .absolute()
                .inset_0()
                .child(ripple(
                    x,
                    y,
                    now - since,
                    failed,
                    gpui::rgba(0xff6b_6b33).into(),
                    Rounding::Round,
                ))
                .child(
                    chip(theme.failed_ink, theme)
                        .left(px(x - 8.))
                        .top(px(y + 26.))
                        .opacity(fading)
                        .child(ui::upper(reason)),
                )
                .into_any_element()
        }
    }
}

/// How far a dot's pulse spreads, and how often, in ms.
const PULSE_MS: f64 = 1400.0;
/// A dot's radius, and a box's ring around a point, in window points.
const DOT: f32 = 7.;
const RING: f32 = 18.;
/// Where an arrow starts when the agent didn't say: this far up and left
/// of what it points at, or mirrored to stay on the picture.
const ARROW_REACH: (f32, f32) = (110., 80.);

/// What an agent pointed out, fading in and out, with its note.
fn callout_element(shown: &Shown, picture: Picture, now: f64, theme: &Theme) -> AnyElement {
    let showing = ((now - shown.since) / CALLOUT_FADE_MS)
        .min((shown.until - now) / CALLOUT_FADE_MS)
        .clamp(0.0, 1.0) as f32;
    let callout = &shown.callout;
    // What it is on, in window points: left, top, right, bottom.
    let (left, top, right, bottom) = match &callout.on {
        CalloutOn::Point(at) => {
            let (x, y) = picture.place(at.x(), at.y());
            (x, y, x, y)
        }
        CalloutOn::Area { frame, .. } => {
            let (left, top) = picture.place(frame.left(), frame.top());
            let (right, bottom) =
                picture.place(frame.left() + frame.width(), frame.top() + frame.height());
            (left, top, right, bottom)
        }
    };
    let label = match &callout.on {
        CalloutOn::Area { label, .. } if !label.is_empty() => Some(label.clone()),
        CalloutOn::Area { .. } | CalloutOn::Point(_) => None,
    };
    let mut drawn = div().absolute().inset_0().opacity(showing);
    let ink = theme.mark;
    // Where the note goes: by the arrow's tail, else under what it's on
    // (over it near the picture's bottom).
    let mut note_at = if bottom + 40. > picture.top + picture.height {
        (left, top - 34.)
    } else {
        (left, bottom + 10.)
    };
    match callout.shape {
        CalloutShape::Box => {
            let on_point = matches!(callout.on, CalloutOn::Point(_));
            let (left, top, right, bottom) = if on_point {
                (left - RING, top - RING, right + RING, bottom + RING)
            } else {
                (left - 3., top - 3., right + 3., bottom + 3.)
            };
            if note_at.1 > top {
                note_at = (left, bottom + 10.);
            } else {
                note_at = (left, top - 34.);
            }
            drawn = drawn.child(
                div()
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(right - left))
                    .h(px(bottom - top))
                    .border_2()
                    .border_color(ink)
                    .bg(theme.mark_soft),
            );
        }
        CalloutShape::Dot => {
            let (x, y) = ((left + right) / 2., (top + bottom) / 2.);
            let pulse = (now - shown.since).rem_euclid(PULSE_MS);
            note_at = (x - DOT, y + DOT + 14.);
            drawn = drawn
                .child(ripple(x, y, pulse, ink, theme.mark_soft, Rounding::Round))
                .child(
                    div()
                        .absolute()
                        .left(px(x - DOT))
                        .top(px(y - DOT))
                        .size(px(DOT * 2.))
                        .bg(ink)
                        .border(px(3.))
                        .border_color(theme.white),
                );
        }
        CalloutShape::Arrow { from } => {
            let from = from.map(|from| picture.place(from.x(), from.y()));
            let (tail, tip) = arrow_ends((left, top, right, bottom), from, picture);
            note_at = (tail.0 - 8., tail.1 + if tail.1 > tip.1 { 8. } else { -34. });
            let white = theme.white;
            drawn = drawn.child(
                canvas(
                    |_, _, _| {},
                    move |bounds: Bounds<Pixels>, (), window: &mut Window, _| {
                        let at = |(x, y): (f32, f32)| {
                            point(bounds.origin.x + px(x), bounds.origin.y + px(y))
                        };
                        let (dx, dy) = (tip.0 - tail.0, tip.1 - tail.1);
                        let length = dx.hypot(dy).max(1.);
                        let (ux, uy) = (dx / length, dy / length);
                        let head = 14.;
                        let base = (tip.0 - ux * head, tip.1 - uy * head);
                        let wing = |side: f32| {
                            (
                                base.0 - uy * head * 0.6 * side,
                                base.1 + ux * head * 0.6 * side,
                            )
                        };
                        for (width, colour) in [(7., white), (4., ink)] {
                            let mut line = PathBuilder::stroke(px(width));
                            line.move_to(at(tail));
                            line.line_to(at(base));
                            if let Ok(line) = line.build() {
                                window.paint_path(line, colour);
                            }
                        }
                        let mut arrowhead = PathBuilder::fill();
                        arrowhead.move_to(at(tip));
                        arrowhead.line_to(at(wing(1.)));
                        arrowhead.line_to(at(wing(-1.)));
                        arrowhead.close();
                        if let Ok(arrowhead) = arrowhead.build() {
                            window.paint_path(arrowhead, ink);
                        }
                    },
                )
                .size_full(),
            );
        }
    }
    let words = match (&callout.note, label) {
        (Some(note), _) => Some(note.clone()),
        (None, Some(label)) => Some(label),
        (None, None) => None,
    };
    if let Some(words) = words {
        drawn = drawn.child(
            chip(theme.mark_ink, theme)
                .left(px(note_at.0))
                .top(px(note_at.1))
                .max_w(px(320.))
                .child(Icon::Highlighter.svg(13., theme.white))
                .child(div().overflow_hidden().text_ellipsis().child(words)),
        );
    }
    drawn.into_any_element()
}

/// Where an arrow to what spans `(left, top, right, bottom)` starts and
/// ends, in window points: from `from`, or from up and left of it (mirrored
/// to stay on the picture), to just outside its edge.
fn arrow_ends(
    (left, top, right, bottom): (f32, f32, f32, f32),
    from: Option<(f32, f32)>,
    picture: Picture,
) -> ((f32, f32), (f32, f32)) {
    let centre = ((left + right) / 2., (top + bottom) / 2.);
    let tail = from.unwrap_or_else(|| {
        let (reach_x, reach_y) = ARROW_REACH;
        let x = if left - reach_x >= picture.left {
            left - reach_x
        } else {
            right + reach_x
        };
        let y = if top - reach_y >= picture.top {
            top - reach_y
        } else {
            bottom + reach_y
        };
        (x, y)
    });
    let (dx, dy) = (centre.0 - tail.0, centre.1 - tail.1);
    let length = dx.hypot(dy);
    if length < 1. {
        return (tail, centre);
    }
    // Where the line from the tail to the centre leaves the box, then a gap.
    let (half_w, half_h) = ((right - left) / 2. + 4., (bottom - top) / 2. + 4.);
    let reach = [
        (dx.abs() > f32::EPSILON).then(|| half_w / dx.abs()),
        (dy.abs() > f32::EPSILON).then(|| half_h / dy.abs()),
    ]
    .into_iter()
    .flatten()
    .fold(1., f32::min);
    let tip = (centre.0 - dx * reach, centre.1 - dy * reach);
    (tail, tip)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rounding {
    Round,
    Square,
}

/// A ring spreading from a point and fading, `elapsed` ms in.
fn ripple(x: f32, y: f32, elapsed: f64, ring: Hsla, fill: Hsla, rounding: Rounding) -> Div {
    let t = (elapsed / RIPPLE_MS).clamp(0.0, 1.0);
    let spread = 0.2 + 1.05 * ease_out(t);
    let size = (44.0 * spread) as f32;
    let shown = if elapsed < 0.0 { 0.0 } else { (1.0 - t) as f32 };
    div()
        .absolute()
        .left(px(x - size / 2.))
        .top(px(y - size / 2.))
        .size(px(size))
        .map(|ripple| match rounding {
            Rounding::Round => ripple.rounded_full(),
            Rounding::Square => ripple,
        })
        .border(px(2.5))
        .border_color(ring)
        .bg(fill)
        .opacity(shown)
}

/// A dot at the end of a drag.
fn spot((x, y): (f32, f32), radius: f32, fill: Hsla, ring: Option<Hsla>) -> Div {
    div()
        .absolute()
        .left(px(x - radius))
        .top(px(y - radius))
        .size(px(radius * 2.))
        .rounded_full()
        .bg(fill)
        .when_some(ring, |spot, ring| spot.border(px(3.)).border_color(ring))
}

/// The agent's arrow, its tip at `(x, y)`.
fn arrow(x: f32, y: f32, fill: Hsla, theme: &Theme) -> impl IntoElement {
    // The browser viewer's path, in a 24-point box drawn at 22 points.
    const OUTLINE: [(f32, f32); 7] = [
        (4.0, 2.5),
        (4.0, 19.0),
        (8.6, 14.8),
        (11.6, 21.4),
        (14.6, 20.0),
        (11.7, 13.6),
        (18.0, 13.6),
    ];
    let edge = theme.white;
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window: &mut Window, _| {
            let scale = 22.0 / 24.0;
            let at = |(px_x, px_y): (f32, f32)| {
                point(
                    bounds.origin.x + px(x - 4.0 + px_x * scale),
                    bounds.origin.y + px(y - 2.0 + px_y * scale),
                )
            };
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
            if let Ok(outline) = trace(PathBuilder::stroke(px(1.5))) {
                window.paint_path(outline, edge);
            }
        },
    )
    .absolute()
    .inset_0()
}

/// The colour of a call's run on the tape: its agent's, except that
/// looking and waiting are drawn faint, so what changed the screen stands
/// out, and marks are pink.
fn family_colour(family: Family, agent: Hsla, theme: &Theme) -> Hsla {
    match family {
        Family::Look | Family::Wait => theme.faint,
        Family::Mark => theme.mark,
        Family::Pointer | Family::Keys | Family::Run | Family::Machine | Family::Other => agent,
    }
}

/// The stretch of time a strip shows, in ms on its timeline's clock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Span {
    pub(crate) from: f64,
    pub(crate) to: f64,
    /// Where calls still under way end, for now.
    pub(crate) now: f64,
    /// The playhead, when there is one.
    pub(crate) at: Option<f64>,
}

/// The tape of calls in `span`, with five times along it: one lane, or a
/// lane per session, named and in its hue, when more than one acted. Each
/// call is a run of dots in its session's hue, on hairline lanes.
pub(crate) fn strip(
    id: impl Into<gpui::ElementId>,
    moments: &[Moment],
    span: Span,
    label: impl Fn(f64) -> String,
    theme: &Theme,
) -> impl IntoElement {
    let id = id.into();
    let Span { from, to, now, .. } = span;
    let shown: Vec<&Moment> = moments
        .iter()
        .filter(|moment| moment.start_ms <= to && moment.end_ms.unwrap_or(now) >= from)
        .collect();
    let lanes = crate::timeline::lanes(shown.iter().copied());
    // Room for two lanes whoever acts, so the screen above never moves; a
    // third agent's and on scroll into view.
    let mut rows = div()
        .id((id.clone(), "lanes"))
        .h(px(LANES_SHOWN * (LANE + 1.)))
        .flex()
        .flex_col()
        .overflow_y_scroll()
        .when(lanes.len() <= 1, |rows| rows.justify_center());
    if lanes.len() > 1 {
        for (index, lane) in lanes.iter().enumerate() {
            let hue = Hue::of(lane.session);
            let name = if lane.client.is_empty() {
                format!("session {}", lane.session)
            } else {
                lane.client.to_owned()
            };
            rows = rows.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(theme.line)
                    .child(
                        div()
                            .w(px(110.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap(px(7.))
                            .font_family(MONO)
                            .text_size(px(10.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.muted)
                            .overflow_hidden()
                            .child(ui::dot(9., hue.fill))
                            .child(
                                div()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(ui::upper(&name)),
                            ),
                    )
                    .child(div().flex_1().child(lane_bar(
                        (id.clone(), index),
                        &lane.moments,
                        span,
                        theme,
                    ))),
            );
        }
    } else {
        rows = rows.child(
            lane_bar((id.clone(), 0), &shown, span, theme)
                .border_b_1()
                .border_color(theme.line),
        );
    }
    let span_ms = (to - from).max(1.0);
    let ticks = [0.0, 0.25, 0.5, 0.75, 1.0].map(|f| label(from + f * span_ms));
    div().pt(px(2.)).child(rows).child(
        div()
            .h(px(18.))
            .pt(px(4.))
            .flex()
            .justify_between()
            .when(lanes.len() > 1, |ticks| ticks.ml(px(118.)))
            .font_family(MONO)
            .text_size(px(10.5))
            .text_color(theme.muted)
            .children(ticks),
    )
}

/// A lane's height, and how many lanes the strip keeps room for.
const LANE: f32 = 22.;
const LANES_SHOWN: f32 = 2.;

/// How a run of dots is drawn: 4 points on, 2 off.
const DOT_ON: f32 = 4.;
const DOT_EVERY: f32 = 6.;

/// One lane of calls in `span`, each a run of dots, with the playhead.
fn lane_bar(
    (id, lane): (gpui::ElementId, usize),
    moments: &[&Moment],
    span: Span,
    theme: &Theme,
) -> Div {
    let Span { from, to, now, at } = span;
    let span = (to - from).max(1.0);
    let x = |ms: f64| ((ms - from) / span).clamp(0.0, 1.0) as f32;
    let bars =
        moments
            .iter()
            .enumerate()
            .fold(div().relative().h(px(LANE)), |bars, (index, moment)| {
                let left = x(moment.start_ms);
                let right = x(moment.end_ms.unwrap_or(now));
                let run = div()
                    .absolute()
                    .left(relative(left))
                    .w(relative(right - left))
                    .min_w(px(DOT_ON));
                let run = if moment.person {
                    // A person's control, hatched.
                    run.top(px(7.))
                        .h(px(8.))
                        .bg(pattern_slash(theme.text, 2., 3.))
                } else {
                    let colour = match moment.failed {
                        Some(_) => theme.danger,
                        None => family_colour(
                            moment.call.gesture.family(),
                            Hue::of(moment.call.key.session).fill,
                            theme,
                        ),
                    };
                    run.top(px(9.))
                        .h(px(4.))
                        .child(
                            canvas(
                                |_, _, _| {},
                                move |bounds: Bounds<Pixels>, (), window: &mut Window, _| {
                                    let end = bounds.origin.x + bounds.size.width;
                                    let mut at = bounds.origin.x;
                                    while at < end {
                                        let width = (end - at).min(px(DOT_ON));
                                        window.paint_quad(fill(
                                            Bounds::new(
                                                point(at, bounds.origin.y),
                                                size(width, bounds.size.height),
                                            ),
                                            colour,
                                        ));
                                        at += px(DOT_EVERY);
                                    }
                                },
                            )
                            .size_full(),
                        )
                        .when(moment.failed.is_some(), |run| {
                            run.child(
                                div()
                                    .absolute()
                                    .top(px(-3.))
                                    .bottom(px(-3.))
                                    .left(px(-3.))
                                    .right(px(-3.))
                                    .border_1()
                                    .border_color(theme.danger),
                            )
                        })
                };
                match moment.end_ms {
                    Some(_) => bars.child(run),
                    None => bars.child(ui::pulse(
                        gpui::ElementId::NamedInteger(
                            format!("{id:?}-{lane}-open").into(),
                            index as u64,
                        ),
                        std::time::Duration::from_millis(1200),
                        run,
                    )),
                }
            });
    // A playhead past either end sits at it: a 1pt line with a square head.
    bars.when_some(at, |bars, at| {
        bars.child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(relative(x(at)))
                .w(px(1.))
                .bg(theme.text)
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left(px(-4.))
                        .size(px(9.))
                        .bg(theme.text),
                ),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_caption_flips_left_near_the_right_edge() {
        assert_eq!(caption_side(100., 1000.), Side::Right);
        assert_eq!(caption_side(682., 1000.), Side::Right);
        assert_eq!(caption_side(683., 1000.), Side::Left);
        assert_eq!(caption_side(990., 1000.), Side::Left);
    }

    #[test]
    fn the_pointer_glides_from_where_it_is() {
        let first = Glide::follow(None, (100.0, 100.0), 0.0);
        assert_eq!(first.at(0.0), (100.0, 100.0), "it starts where it acts");
        let next = Glide::follow(Some(first), (200.0, 100.0), 1000.0);
        assert_eq!(next.at(1000.0), (100.0, 100.0));
        assert!(next.moving(1200.0));
        assert_eq!(next.at(1000.0 + GLIDE_MS), (200.0, 100.0));
        assert_eq!(Glide::follow(Some(next), (200.0, 100.0), 5000.0), next);
    }

    #[test]
    fn an_arrow_stops_short_of_what_it_points_at_and_stays_on_the_picture() {
        let picture = Picture {
            left: 0.,
            top: 0.,
            width: 1000.,
            height: 800.,
            points: doppel_machine::geometry::Size::new(1000., 800.).unwrap(),
        };
        // Room up and left: the tail goes there; the tip is outside the box.
        let (tail, tip) = arrow_ends((300., 300., 400., 340.), None, picture);
        assert_eq!(tail, (190., 220.));
        assert!((tip.1 - 296.).abs() < 0.01 && tip.0 < 350., "{tip:?}");
        // In the top left corner: mirrored to stay on the picture.
        let (tail, _) = arrow_ends((10., 10., 50., 30.), None, picture);
        assert_eq!(tail, (160., 110.));
        // A point: the tip is near it, from where the agent said.
        let (tail, tip) = arrow_ends((500., 500., 500., 500.), Some((500., 300.)), picture);
        assert_eq!(tail, (500., 300.));
        assert_eq!(tip, (500., 496.));
    }
}
