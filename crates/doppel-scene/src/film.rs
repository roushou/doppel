//! A recording's overlay as pictures to burn into its video: at each
//! instant, the shapes the viewers would draw over the screen (the agent's
//! pointer gliding to where it acts, what it's doing beside it, the marks
//! its calls leave, the marks it puts up for the people watching), in
//! display points. A renderer draws them on the frames without deciding
//! anything.

use doppel_machine::geometry::{Point, Rect, Size};

use crate::attention::{Attention, Glance, LET_GO_MS, LOCK_MS};
use crate::timeline::{
    CALLOUT_FADE_MS, CalloutOn, CalloutShape, Gesture, MARK_FADE_MS, Mark, Moment, scene_at,
};

/// A colour: red, green and blue in its low 24 bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(u32);

impl Rgb {
    /// As `0xRRGGBB`.
    #[must_use]
    pub const fn hex(self) -> u32 {
        self.0
    }
}

/// The sessions' colours, fill and ink, as the viewers have them. A
/// recording's one session (0) takes the first.
const HUES: [(Rgb, Rgb); 4] = [
    (Rgb(0x22_e5ff), Rgb(0x0b_5d6b)),
    (Rgb(0x3d_8bff), Rgb(0x1f_4f9e)),
    (Rgb(0xb1_8cff), Rgb(0x5b_3aa8)),
    (Rgb(0xc3_d94e), Rgb(0x56_6314)),
];

/// What an agent puts up for the people watching, whoever it is.
pub const MARKED: Rgb = Rgb(0xff_5ca8);
/// A failure.
const FAILED: Rgb = Rgb(0xc9_3a3a);
const WHITE: Rgb = Rgb(0xff_ffff);

fn hue(session: u64) -> (Rgb, Rgb) {
    let index = usize::try_from(session.saturating_sub(1) % HUES.len() as u64).unwrap_or(0);
    HUES.get(index).copied().unwrap_or(HUES[0])
}

/// A place in display points; unlike a [`Point`], it may lie off the display.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spot {
    pub x: f64,
    pub y: f64,
}

impl From<Point> for Spot {
    fn from(point: Point) -> Self {
        Self {
            x: point.x(),
            y: point.y(),
        }
    }
}

/// Which corner of a tag its place is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
}

/// How a stroke is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    Solid,
    Dashed,
    /// Solid, with an arrowhead at its end.
    Arrow,
}

/// One thing to draw. `opacity` is the whole shape's, from 0 to 1; `wash`
/// is how much of its colour fills it, under that.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    /// A rectangle's outline, its inside washed with its colour.
    Frame {
        rect: Rect,
        colour: Rgb,
        wash: f64,
        width: f64,
        opacity: f64,
    },
    /// A circle's outline, its inside washed: a ripple, a dot.
    Ring {
        centre: Spot,
        radius: f64,
        colour: Rgb,
        wash: f64,
        width: f64,
        opacity: f64,
    },
    Stroke {
        from: Spot,
        to: Spot,
        colour: Rgb,
        width: f64,
        line: Line,
        opacity: f64,
    },
    /// An agent's pointer, its tip at `tip`.
    Pointer {
        tip: Spot,
        colour: Rgb,
        opacity: f64,
    },
    /// The screen dimmed by `colour`, but for `holes`: what the agent
    /// takes in, lit through it.
    Veil {
        holes: Vec<Rect>,
        colour: Rgb,
        opacity: f64,
    },
    /// A rectangle's four corners, each an arm `arm` long: an element taken
    /// in, or closed in on.
    Brackets {
        rect: Rect,
        arm: f64,
        colour: Rgb,
        width: f64,
        opacity: f64,
    },
    /// One line of words on a plate, `at` being its `corner`.
    Tag {
        at: Spot,
        corner: Corner,
        text: String,
        ink: Rgb,
        plate: Rgb,
        opacity: f64,
    },
}

/// What is drawn over one frame, bottom first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Picture {
    pub shapes: Vec<Shape>,
}

/// The veil over what an agent doesn't take in: its colour, how dark at
/// most, and how long it takes to fall.
const VEIL: Rgb = Rgb(0x05_0612);
const VEIL_DARK: f64 = 0.3;
const VEIL_IN_MS: f64 = 220.0;
/// An element taken in lights fully, then settles to `FLOOR` over this.
const AFTERGLOW_MS: f64 = 500.0;
const FLOOR: f64 = 0.4;
/// An element's ref shows this long as it is taken in.
const READOUT_MS: f64 = 320.0;
/// The lock starts this far out from its element, and ends this close.
const LOCK_FROM: f64 = 36.0;
const LOCK_TO: f64 = 4.0;
/// How long a lock stays once its call ends: the screen changes then.
const LOCK_SETTLE_MS: f64 = 150.0;

/// `rect` grown by `by` on every side.
fn grown(rect: Rect, by: f64) -> Rect {
    Rect::new(
        rect.left() - by,
        rect.top() - by,
        rect.width() + 2.0 * by,
        rect.height() + 2.0 * by,
    )
    .unwrap_or(rect)
}

/// What `attention` draws at `at`: the screen veiled but for what was taken
/// in, each lit in turn and bracketed with its ref; then, once the agent
/// goes for one, the rest let go and brackets close in on that one.
fn taken_in(attention: &Attention, at: f64, shapes: &mut Vec<Shape>) {
    let (fill, ink) = hue(attention.session);
    let landing = ((at - attention.since) / VEIL_IN_MS).clamp(0.0, 1.0);
    let rest = attention
        .moved_on
        .filter(|moved| at >= *moved)
        .map_or(1.0, |moved| {
            (1.0 - (at - moved) / LET_GO_MS).clamp(0.0, 1.0)
        });
    let lock = attention.lock.filter(|lock| at >= lock.since).map(|lock| {
        let shown = lock.until.map_or(1.0, |until| {
            ((until + LOCK_SETTLE_MS - at) / MARK_FADE_MS).clamp(0.0, 1.0)
        });
        (lock, shown)
    });
    let lit: Vec<&Glance> = attention
        .glances
        .iter()
        .filter(|glance| glance.at <= at)
        .collect();

    let veiled = match lock {
        Some((_, shown)) => shown,
        None => rest,
    } * landing;
    if veiled > 0.0 {
        let holes = match lock {
            Some((lock, _)) => vec![grown(lock.frame, LOCK_TO)],
            None => lit.iter().map(|glance| grown(glance.frame, 3.0)).collect(),
        };
        shapes.push(Shape::Veil {
            holes,
            colour: VEIL,
            opacity: VEIL_DARK * veiled,
        });
    }
    if rest > 0.0 {
        for glance in &lit {
            let age = at - glance.at;
            let glow = FLOOR + (1.0 - FLOOR) * (1.0 - age / AFTERGLOW_MS).clamp(0.0, 1.0);
            let frame = glance.frame;
            shapes.push(Shape::Brackets {
                rect: grown(frame, 2.0),
                arm: 8.0_f64
                    .min(frame.width() / 3.0)
                    .min(frame.height() / 3.0)
                    .max(3.0),
                colour: fill,
                width: 1.5,
                opacity: glow * rest,
            });
            if age < READOUT_MS {
                shapes.push(Shape::Tag {
                    at: Spot {
                        x: frame.right() + 4.0,
                        y: frame.top(),
                    },
                    corner: Corner::TopLeft,
                    text: glance.reference.to_string(),
                    ink: WHITE,
                    plate: ink,
                    opacity: (1.0 - age / READOUT_MS) * rest,
                });
            }
        }
    }
    if let Some((lock, shown)) = lock {
        let closing = ease((at - lock.since) / LOCK_MS);
        shapes.push(Shape::Brackets {
            rect: grown(lock.frame, LOCK_FROM + (LOCK_TO - LOCK_FROM) * closing),
            arm: 12.0,
            colour: fill,
            width: 2.5,
            opacity: shown * closing.max(0.35),
        });
    }
}

/// How long the pointer takes to glide to where it acts next.
pub const GLIDE_MS: f64 = 450.0;
/// How long the pointer stays once its agent stops acting, then fades.
pub const POINTER_MS: f64 = 6000.0;
const POINTER_FADE_MS: f64 = 400.0;
/// A ripple's life, and its radius at the end.
const RIPPLE_MS: f64 = 900.0;
const RIPPLE_RADIUS: f64 = 27.0;

/// Fast at first, then settling, as the viewers' pointer does.
fn ease(f: f64) -> f64 {
    let f = f.clamp(0.0, 1.0);
    1.0 - (1.0 - f).powi(3)
}

fn between(from: Spot, to: Spot, f: f64) -> Spot {
    Spot {
        x: from.x + (to.x - from.x) * f,
        y: from.y + (to.y - from.y) * f,
    }
}

/// Where a session's pointer is at `at`: gliding from where it was to
/// where its latest call with a place acted.
fn pointer_at(moments: &[Moment], session: u64, at: f64) -> Option<Spot> {
    let mut placed = moments
        .iter()
        .filter(|m| !m.person && m.call.key.session == session && m.start_ms <= at)
        .filter_map(|m| m.call.target.aim().map(|aim| (m.start_ms, Spot::from(aim))));
    let mut last = placed.next()?;
    let mut before = last.1;
    for next in placed {
        before = last.1;
        last = next;
    }
    Some(between(before, last.1, ease((at - last.0) / GLIDE_MS)))
}

/// How much of something shown from `since` until `until` shows at `at`,
/// fading over `fade` at each end.
fn faded(at: f64, since: f64, until: f64, fade: f64) -> f64 {
    ((at - since) / fade)
        .min((until - at) / fade)
        .clamp(0.0, 1.0)
}

impl Picture {
    /// What is drawn over `moments`' machine, its display `display`, at `at`.
    #[must_use]
    pub fn at(moments: &[Moment], display: Size, at: f64) -> Self {
        let scene = scene_at(moments, at);
        let mut shapes = Vec::new();
        for attention in Attention::at(moments, display, at) {
            taken_in(&attention, at, &mut shapes);
        }

        for shown in &scene.callouts {
            let opacity = faded(at, shown.since, shown.until, CALLOUT_FADE_MS);
            let callout = &shown.callout;
            let (centre, area, label) = match &callout.on {
                CalloutOn::Point(point) => (Spot::from(*point), None, None),
                CalloutOn::Area { frame, label } => (
                    Spot::from(frame.center()),
                    Some(*frame),
                    (!label.is_empty()).then_some(label.clone()),
                ),
            };
            let note = callout.note.clone().or(label);
            match callout.shape {
                CalloutShape::Box => match area {
                    Some(rect) => shapes.push(Shape::Frame {
                        rect,
                        colour: MARKED,
                        wash: 0.18,
                        width: 2.0,
                        opacity,
                    }),
                    None => shapes.push(Shape::Ring {
                        centre,
                        radius: 18.0,
                        colour: MARKED,
                        wash: 0.18,
                        width: 2.0,
                        opacity,
                    }),
                },
                CalloutShape::Dot => {
                    shapes.push(Shape::Ring {
                        centre,
                        radius: 7.0,
                        colour: MARKED,
                        wash: 1.0,
                        width: 3.0,
                        opacity,
                    });
                }
                CalloutShape::Arrow { from } => {
                    let tail = from.map_or(
                        Spot {
                            x: centre.x - 110.0,
                            y: centre.y - 80.0,
                        },
                        Spot::from,
                    );
                    shapes.push(Shape::Stroke {
                        from: tail,
                        to: centre,
                        colour: MARKED,
                        width: 4.0,
                        line: Line::Arrow,
                        opacity,
                    });
                }
            }
            if let Some(note) = note {
                let below = area.map_or(centre.y + 24.0, |rect| rect.bottom() + 8.0);
                let left = area.map_or(centre.x - 7.0, Rect::left);
                shapes.push(Shape::Tag {
                    at: Spot { x: left, y: below },
                    corner: Corner::TopLeft,
                    text: note,
                    ink: WHITE,
                    plate: Rgb(0xbf_4580),
                    opacity,
                });
            }
        }

        for agent in &scene.agents {
            let (fill, ink) = hue(agent.session);
            for mark in &agent.marks {
                let opacity = mark.opacity(at);
                match mark {
                    Mark::Ripple {
                        at: point, since, ..
                    } => {
                        let f = ((at - since) / RIPPLE_MS).clamp(0.0, 1.0);
                        if f < 1.0 {
                            shapes.push(Shape::Ring {
                                centre: Spot::from(*point),
                                radius: RIPPLE_RADIUS * (0.2 + 0.8 * ease(f)),
                                colour: fill,
                                wash: 0.22,
                                width: 2.5,
                                opacity: (1.0 - f).min(opacity),
                            });
                        }
                    }
                    Mark::Box { frame, label, .. } => {
                        shapes.push(Shape::Frame {
                            rect: *frame,
                            colour: fill,
                            wash: 0.22,
                            width: 2.0,
                            opacity,
                        });
                        if !label.is_empty() {
                            shapes.push(Shape::Tag {
                                at: Spot {
                                    x: frame.left(),
                                    y: frame.top() - 4.0,
                                },
                                corner: Corner::BottomLeft,
                                text: label.clone(),
                                ink: WHITE,
                                plate: ink,
                                opacity,
                            });
                        }
                    }
                    Mark::Path { from, to, .. } => {
                        shapes.push(Shape::Stroke {
                            from: Spot::from(*from),
                            to: Spot::from(*to),
                            colour: fill,
                            width: 3.0,
                            line: Line::Dashed,
                            opacity,
                        });
                    }
                    Mark::Failed {
                        at: point, reason, ..
                    } => {
                        let centre = Spot::from(*point);
                        shapes.push(Shape::Ring {
                            centre,
                            radius: 22.0,
                            colour: FAILED,
                            wash: 0.2,
                            width: 2.5,
                            opacity,
                        });
                        shapes.push(Shape::Tag {
                            at: Spot {
                                x: centre.x - 8.0,
                                y: centre.y + 26.0,
                            },
                            corner: Corner::TopLeft,
                            text: reason.clone(),
                            ink: WHITE,
                            plate: FAILED,
                            opacity,
                        });
                    }
                }
            }

            let current = agent.current.as_ref().map(|(call, _)| call);
            // Work off the screen shows as a chip in the corner, not at the pointer.
            let running = current.filter(|call| call.gesture == Gesture::Run);
            if let Some(call) = running {
                shapes.push(Shape::Tag {
                    at: Spot {
                        x: 12.0,
                        y: display.height() - 12.0,
                    },
                    corner: Corner::BottomLeft,
                    text: {
                        let what = if call.caption.is_empty() {
                            call.describe()
                        } else {
                            call.caption.clone()
                        };
                        match &call.why {
                            Some(why) => format!("{what} · {why}"),
                            None => what,
                        }
                    },
                    ink: WHITE,
                    plate: ink,
                    opacity: 1.0,
                });
            }
            let Some(tip) = pointer_at(moments, agent.session, at) else {
                continue;
            };
            let opacity = if current.is_some() {
                1.0
            } else {
                ((agent.active + POINTER_MS + POINTER_FADE_MS - at) / POINTER_FADE_MS)
                    .clamp(0.0, 1.0)
            };
            if opacity == 0.0 {
                continue;
            }
            shapes.push(Shape::Pointer {
                tip,
                colour: fill,
                opacity,
            });
            if let Some(call) = current.filter(|_| running.is_none()) {
                shapes.push(Shape::Tag {
                    at: Spot {
                        x: tip.x + 18.0,
                        y: tip.y + 20.0,
                    },
                    corner: Corner::TopLeft,
                    text: call.describe().to_uppercase(),
                    ink: WHITE,
                    plate: ink,
                    opacity,
                });
                // Why it does it, in its own words, under what it does.
                if let Some(why) = &call.why {
                    shapes.push(Shape::Tag {
                        at: Spot {
                            x: tip.x + 18.0,
                            y: tip.y + 42.0,
                        },
                        corner: Corner::TopLeft,
                        text: why.clone(),
                        ink,
                        plate: fill,
                        opacity,
                    });
                }
            }
        }
        Self { shapes }
    }
}

/// `moments` with each agent call starting up to a glide early, never
/// before that agent's previous call ended: a film knows what comes next,
/// so its pointer arrives as the call lands rather than after the screen
/// has already changed.
fn ahead(moments: &[Moment]) -> Vec<Moment> {
    let mut ended: Vec<(u64, f64)> = Vec::new();
    moments
        .iter()
        .map(|moment| {
            let mut moment = moment.clone();
            if moment.person {
                return moment;
            }
            let session = moment.call.key.session;
            let free = ended
                .iter()
                .find(|(s, _)| *s == session)
                .map_or(f64::NEG_INFINITY, |(_, end)| *end);
            let start = moment.start_ms;
            moment.start_ms = (start - GLIDE_MS).max(free).min(start);
            let end = moment.end_ms.unwrap_or(start).max(start);
            match ended.iter_mut().find(|(s, _)| *s == session) {
                Some(slot) => slot.1 = end,
                None => ended.push((session, end)),
            }
            moment
        })
        .collect()
}

/// A recording's pictures for one video: one per frame at `fps`, the
/// first at `from_ms` in the session's time.
#[derive(Debug, Clone, PartialEq)]
pub struct Reel {
    pub fps: u32,
    pub pictures: Vec<Picture>,
}

impl Reel {
    /// The pictures over `moments`, on a display `display`, from `from_ms`
    /// until `until_ms`.
    #[must_use]
    pub fn shoot(moments: &[Moment], display: Size, from_ms: f64, until_ms: f64, fps: u32) -> Self {
        let moments = ahead(moments);
        let fps = fps.max(1);
        let step = 1000.0 / f64::from(fps);
        let mut pictures = Vec::new();
        let mut frame: u32 = 0;
        loop {
            let at = from_ms + f64::from(frame) * step;
            if at >= until_ms {
                break;
            }
            pictures.push(Picture::at(&moments, display, at));
            frame += 1;
        }
        Self { fps, pictures }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::timeline::{Call, CallKey, Callout, CalloutLasts, Target};

    fn display() -> Size {
        Size::new(1280.0, 800.0).unwrap()
    }

    fn moment(
        request: u64,
        start_ms: f64,
        end_ms: f64,
        gesture: Gesture,
        target: Target,
    ) -> Moment {
        Moment {
            start_ms,
            end_ms: Some(end_ms),
            call: Call {
                key: CallKey {
                    session: 1,
                    request,
                },
                machine: "dev".parse().unwrap(),
                client: "claude".into(),
                caption: "exec /usr/bin/open".into(),
                gesture,
                target,
                callout: None,
                why: None,
            },
            failed: None,
            person: false,
            seen: None,
            refs: None,
            read: None,
        }
    }

    fn click(request: u64, start_ms: f64, x: f64, y: f64) -> Moment {
        moment(
            request,
            start_ms,
            start_ms + 50.0,
            Gesture::Click,
            Target::Point(Point::new(x, y).unwrap()),
        )
    }

    fn pointer(picture: &Picture) -> Option<(Spot, f64)> {
        picture.shapes.iter().find_map(|shape| {
            if let Shape::Pointer { tip, opacity, .. } = shape {
                Some((*tip, *opacity))
            } else {
                None
            }
        })
    }

    fn tags(picture: &Picture) -> Vec<&str> {
        picture
            .shapes
            .iter()
            .filter_map(|shape| {
                if let Shape::Tag { text, .. } = shape {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn the_pointer_glides_to_where_the_agent_acts_next() {
        let moments = [click(1, 0.0, 100.0, 100.0), click(2, 1000.0, 300.0, 100.0)];
        assert_eq!(
            pointer(&Picture::at(&moments, display(), 500.0)).unwrap().0,
            Spot { x: 100.0, y: 100.0 }
        );
        let halfway = pointer(&Picture::at(&moments, display(), 1000.0 + GLIDE_MS / 2.0))
            .unwrap()
            .0;
        assert!(halfway.x > 200.0 && halfway.x < 300.0, "{halfway:?}");
        assert_eq!(
            pointer(&Picture::at(&moments, display(), 1000.0 + GLIDE_MS))
                .unwrap()
                .0,
            Spot { x: 300.0, y: 100.0 }
        );
    }

    #[test]
    fn the_pointer_says_what_it_does_and_fades_once_the_agent_stops() {
        let moments = [click(1, 0.0, 100.0, 100.0)];
        assert_eq!(tags(&Picture::at(&moments, display(), 10.0)), ["CLICKING"]);
        // Done, the pointer stays a while without words, then fades.
        let idle = Picture::at(&moments, display(), 3000.0);
        assert_eq!(pointer(&idle).unwrap().1, 1.0);
        assert!(tags(&idle).is_empty());
        assert!(
            pointer(&Picture::at(
                &moments,
                display(),
                50.0 + POINTER_MS + 1000.0
            ))
            .is_none()
        );
        // A click leaves a ripple that grows and fades.
        assert!(
            Picture::at(&moments, display(), 300.0)
                .shapes
                .iter()
                .any(|shape| matches!(shape, Shape::Ring { opacity, .. } if *opacity < 1.0))
        );
    }

    #[test]
    fn why_the_agent_acts_shows_under_what_it_does() {
        let mut said = click(1, 0.0, 100.0, 100.0);
        said.call.why = Some("to open the pricing page".into());
        assert_eq!(
            tags(&Picture::at(&[said], display(), 10.0)),
            ["CLICKING", "to open the pricing page"]
        );
    }

    #[test]
    fn a_command_shows_as_a_chip_in_the_corner_not_at_the_pointer() {
        let moments = [
            click(1, 0.0, 100.0, 100.0),
            moment(2, 1000.0, 3000.0, Gesture::Run, Target::Nowhere),
        ];
        let picture = Picture::at(&moments, display(), 2000.0);
        assert_eq!(tags(&picture), ["exec /usr/bin/open"]);
        let chip = picture.shapes.iter().find_map(|shape| {
            if let Shape::Tag { at, corner, .. } = shape {
                Some((*at, *corner))
            } else {
                None
            }
        });
        assert_eq!(chip, Some((Spot { x: 12.0, y: 788.0 }, Corner::BottomLeft)));
    }

    #[test]
    fn a_mark_shows_its_box_and_note_fading_in_and_out() {
        let mut mark = moment(1, 0.0, 0.0, Gesture::Mark, Target::Nowhere);
        mark.call.callout = Some(Arc::new(Callout {
            on: CalloutOn::Area {
                frame: Rect::new(40.0, 40.0, 300.0, 40.0).unwrap(),
                label: "heading".into(),
            },
            shape: CalloutShape::Box,
            note: Some("Headline reads well".into()),
            lasts: CalloutLasts::For(3000.0),
        }));
        let moments = [mark];
        let opacity = |at| {
            Picture::at(&moments, display(), at)
                .shapes
                .iter()
                .find_map(|shape| {
                    if let Shape::Frame {
                        opacity, colour, ..
                    } = shape
                    {
                        (*colour == MARKED).then_some(*opacity)
                    } else {
                        None
                    }
                })
        };
        assert!(opacity(CALLOUT_FADE_MS / 2.0).unwrap() < 1.0);
        assert_eq!(opacity(1500.0), Some(1.0));
        assert_eq!(opacity(3000.0), None);
        assert!(tags(&Picture::at(&moments, display(), 1500.0)).contains(&"Headline reads well"));
    }

    #[test]
    fn a_reel_has_a_picture_per_frame() {
        let moments = [click(1, 0.0, 100.0, 100.0)];
        let reel = Reel::shoot(&moments, display(), 0.0, 1000.0, 30);
        assert_eq!(reel.pictures.len(), 30);
        assert_eq!(
            reel.pictures[0],
            Picture::at(&ahead(&moments), display(), 0.0)
        );
        assert!(
            Reel::shoot(&moments, display(), 500.0, 500.0, 30)
                .pictures
                .is_empty()
        );
    }

    #[test]
    fn a_reel_brings_the_pointer_there_as_the_call_lands() {
        let moments = [click(1, 0.0, 100.0, 100.0), click(2, 2000.0, 300.0, 100.0)];
        let reel = Reel::shoot(&moments, display(), 0.0, 2100.0, 100);
        let at = |ms: usize| pointer(&reel.pictures[ms / 10]).unwrap().0;
        assert_eq!(at(2000), Spot { x: 300.0, y: 100.0 });
        assert!(at(1700).x > 100.0, "already on its way");
        // Never before the agent's previous call ended.
        let close = [click(1, 0.0, 100.0, 100.0), click(2, 100.0, 300.0, 100.0)];
        assert_eq!(ahead(&close)[1].start_ms, 50.0);
    }
}
