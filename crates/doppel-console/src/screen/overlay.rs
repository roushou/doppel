//! What an agent does, drawn over its machine's screen, and the strip of
//! its calls under it. Over the screen go the film's shapes
//! (`doppel_scene::film`), the same an export burns in, with the agent's
//! reason for its call beside its pointer; the web console's
//! `Overlay.svelte` and `Timeline.svelte` show the same.

use doppel_scene::film;
use gpui::{
    Bounds, Div, FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels,
    StatefulInteractiveElement, Styled, Window, canvas, div, fill, pattern_slash, point,
    prelude::FluentBuilder, px, relative, size,
};

use crate::screen::Picture;
use crate::timeline::{Family, Moment};
use crate::ui;
use crate::ui::theme::{Hue, MONO, Theme};

/// The widest a caption gets.
const CAPTION_MAX: f32 = 300.;
/// Where a caption starts from its pointer's tip, in display points: just
/// past the film's badge, level with it.
const CAPTION_FROM: (f64, f64) = (33., 11.);

/// Which side of its pointer a caption goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Right,
    /// Near the picture's right edge, where a caption on the right would be cut off.
    Left,
}

/// The side a caption starting at `x` goes, with the picture's right edge at `edge`.
fn caption_side(x: f32, edge: f32) -> Side {
    if x + CAPTION_MAX > edge {
        Side::Left
    } else {
        Side::Right
    }
}

/// A caption in the agent's colour, flat, in small mono.
fn chip(fill: Hsla, ink: Hsla) -> Div {
    div()
        .absolute()
        .px(px(8.))
        .py(px(4.))
        .bg(fill)
        .text_color(ink)
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .font_family(MONO)
        .whitespace_nowrap()
        .max_w(px(CAPTION_MAX))
        .overflow_hidden()
        .text_ellipsis()
}

/// What agents do over a screen whose picture is at `picture`, at `now`:
/// the film's shapes for `moments`, and beside each agent's pointer why it
/// does what it does (named by its client when several act).
pub(crate) fn layer(moments: &[Moment], picture: Picture, now: f64) -> impl IntoElement {
    let drawn = film::Picture::at(moments, picture.points, now);
    let colours = film::SessionColours::of(moments);
    let mut layer = crate::screen::shapes::layer(drawn.shapes, picture);
    for caption in film::Caption::at(moments, now) {
        let words = match (caption.why, caption.client) {
            (Some(why), Some(client)) => format!("{} · {why}", ui::upper(&client)),
            (Some(why), None) => why,
            (None, Some(client)) => ui::upper(&client).to_string(),
            (None, None) => continue,
        };
        let tip = caption.tip;
        let hue = Hue::of(colours.hue(caption.session));
        let (x, y) = picture.place(tip.x + CAPTION_FROM.0, tip.y + CAPTION_FROM.1);
        let caption = chip(hue.fill, hue.ink).child(words);
        layer = layer.child(match caption_side(x, picture.left + picture.width) {
            Side::Right => caption.left(px(x)).top(px(y)),
            // Ending just left of the pointer.
            Side::Left => {
                let (left, _) = picture.place(tip.x - 8., tip.y);
                div()
                    .absolute()
                    .left(px(left))
                    .top(px(y))
                    .w(px(0.))
                    .flex()
                    .justify_end()
                    .child(caption.relative().flex_none())
            }
        });
    }
    layer
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
    let colours = film::SessionColours::of(moments);
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
            let hue = Hue::of(colours.hue(lane.session));
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
                        &colours,
                        span,
                        theme,
                    ))),
            );
        }
    } else {
        rows = rows.child(
            lane_bar((id.clone(), 0), &shown, &colours, span, theme)
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
    colours: &film::SessionColours,
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
                            Hue::of(colours.hue(moment.call.key.session)).fill,
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
        assert_eq!(caption_side(700., 1000.), Side::Right);
        assert_eq!(caption_side(701., 1000.), Side::Left);
        assert_eq!(caption_side(990., 1000.), Side::Left);
    }
}
