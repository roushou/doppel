//! A recording's overlay as pictures to burn into its video: at each
//! instant, the shapes the consoles would draw over the screen (the agent's
//! pointer gliding to where it acts, what it's doing beside it, the marks
//! its calls leave, the marks it puts up for the people watching), in
//! display points. A renderer draws them on the frames without deciding
//! anything.

use doppel_desktop::geometry::{Point, Rect, Size};

pub mod pacing;

use self::pacing::Pace;
use crate::attention::{Attention, Glance, LET_GO_MS, LOCK_MS, aimed};
use crate::timeline::{
    CALLOUT_FADE_MS, CalloutOn, CalloutShape, Gesture, MARK_FADE_MS, Mark, Moment, Scene,
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

/// The sessions' colours, fill and ink, as the consoles have them. A
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
    /// A symbol on a round badge of `colour`, `size` across: what an
    /// agent's pointer is doing, or that what it did worked.
    Glyph {
        centre: Spot,
        glyph: Glyph,
        colour: Rgb,
        size: f64,
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

/// What is drawn over one frame, bottom first, and the instant of the
/// session it shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Picture {
    pub shapes: Vec<Shape>,
    pub at: f64,
}

/// What a glyph shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    /// A press, a click: a fingertip.
    Press,
    /// Typing: a text cursor.
    Type,
    /// A key or a shortcut: a keycap.
    Keys,
    /// A command, or a file moved, in the guest: a terminal.
    Command,
    /// Waiting on something: an hourglass.
    Wait,
    /// Reading the screen: an eye.
    Look,
    /// Pointing something out to the people watching: a pin.
    Point,
    Drag,
    Scroll,
    /// Starting, stopping, restoring or forking the machine.
    Machine,
    /// Keeping a checkpoint: a flag planted.
    Checkpoint,
    /// Rolling back to a checkpoint: a rewind.
    Rollback,
    /// What it did worked: a check.
    Done,
}

impl Glyph {
    /// What a call of `gesture` shows beside the pointer; none for a move.
    pub const fn of(gesture: Gesture) -> Option<Self> {
        match gesture {
            Gesture::Click | Gesture::DoubleClick | Gesture::SecondaryClick | Gesture::Act => {
                Some(Self::Press)
            }
            Gesture::Type => Some(Self::Type),
            Gesture::Key => Some(Self::Keys),
            Gesture::Run => Some(Self::Command),
            Gesture::Wait => Some(Self::Wait),
            Gesture::Look => Some(Self::Look),
            Gesture::Mark => Some(Self::Point),
            Gesture::Drag => Some(Self::Drag),
            Gesture::Scroll => Some(Self::Scroll),
            Gesture::Lifecycle => Some(Self::Machine),
            Gesture::Checkpoint => Some(Self::Checkpoint),
            Gesture::Rollback => Some(Self::Rollback),
            Gesture::Move | Gesture::Other => None,
        }
    }
}

/// A glyph's badge: how wide, and where from the pointer's tip.
const GLYPH_SIZE: f64 = 22.0;
const GLYPH_OFFSET: (f64, f64) = (18.0, 20.0);
/// A wait's breath: one in and out.
const BREATH_MS: f64 = 1400.0;
/// A press dents in where it lands over this long.
const DENT_MS: f64 = 260.0;
const DENT_FROM: f64 = 26.0;
const DENT_TO: f64 = 7.0;
/// The badge shows a check this long after what worked, popping in over
/// `POP_MS`.
const DONE_MS: f64 = 700.0;
const POP_MS: f64 = 150.0;
const DONE: Rgb = Rgb(0x2f_bf71);

/// Whether a call of `gesture` lands on something, to dent and to check.
const fn lands(gesture: Gesture) -> bool {
    match gesture {
        Gesture::Click
        | Gesture::DoubleClick
        | Gesture::SecondaryClick
        | Gesture::Act
        | Gesture::Type
        | Gesture::Drag => true,
        Gesture::Move
        | Gesture::Scroll
        | Gesture::Key
        | Gesture::Look
        | Gesture::Wait
        | Gesture::Run
        | Gesture::Lifecycle
        | Gesture::Checkpoint
        | Gesture::Rollback
        | Gesture::Mark
        | Gesture::Other => false,
    }
}

/// What lands as each agent's call ends: a press dents in where it lands.
fn landed(moments: &[Moment], at: f64, shapes: &mut Vec<Shape>) {
    for moment in moments {
        let Some(end) = moment.end_ms else {
            continue;
        };
        if moment.person || !lands(moment.call.gesture) || at < end || at >= end + DENT_MS {
            continue;
        }
        let Some(rect) = aimed(&moment.call.target) else {
            continue;
        };
        let (fill, _) = hue(moment.call.key.session);
        let f = ease((at - end) / DENT_MS);
        shapes.push(Shape::Ring {
            centre: Spot::from(rect.center()),
            radius: DENT_FROM + (DENT_TO - DENT_FROM) * f,
            colour: fill,
            wash: 0.25,
            width: 3.0,
            opacity: 0.9 * (1.0 - f * f),
        });
    }
}

/// How long ago `session`'s latest call that landed and worked showed its
/// check, while it shows: from halfway through its dent, for [`DONE_MS`].
/// None once that agent has started another call.
fn worked(moments: &[Moment], session: u64, at: f64) -> Option<f64> {
    let latest = moments.iter().rfind(|moment| {
        !moment.person && moment.call.key.session == session && moment.start_ms <= at
    })?;
    let end = latest.end_ms?;
    let shown = at - (end + DENT_MS * 0.5);
    (lands(latest.call.gesture) && latest.failed.is_none() && (0.0..DONE_MS).contains(&shown))
        .then_some(shown)
}

/// How long a rollback's rewind shows after the machine is back.
const REWIND_MS: f64 = 700.0;
/// The rewind's glyph, across.
const REWIND_SIZE: f64 = 96.0;

/// A rollback as a scene: while it runs, the screen dims under a large
/// rewind glyph; once the machine is back, both fade as the screen shows
/// what it returned to.
fn rewound(moments: &[Moment], display: Size, at: f64, shapes: &mut Vec<Shape>) {
    for moment in moments {
        if moment.person || moment.call.gesture != Gesture::Rollback || at < moment.start_ms {
            continue;
        }
        let shown = match moment.end_ms {
            Some(end) if at >= end + REWIND_MS => continue,
            Some(end) if at >= end => 1.0 - ease((at - end) / REWIND_MS),
            Some(_) | None => ((at - moment.start_ms) / VEIL_IN_MS).clamp(0.0, 1.0),
        };
        let (fill, _) = hue(moment.call.key.session);
        shapes.push(Shape::Veil {
            holes: Vec::new(),
            colour: VEIL,
            opacity: 0.55 * shown,
        });
        shapes.push(Shape::Glyph {
            centre: Spot {
                x: display.width() / 2.0,
                y: display.height() / 2.0,
            },
            glyph: Glyph::Rollback,
            colour: fill,
            size: REWIND_SIZE,
            opacity: shown,
        });
    }
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
    let lets_go = attention.lets_go();
    let rest = if at >= lets_go {
        (1.0 - (at - lets_go) / LET_GO_MS).clamp(0.0, 1.0)
    } else {
        1.0
    };
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

/// Fast at first, then settling, as the consoles' pointer does.
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
/// where its latest call with a place acted. A console puts words beside it.
#[must_use]
pub fn pointer_at(moments: &[Moment], session: u64, at: f64) -> Option<Spot> {
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

/// Words a console writes beside an agent's pointer, which a film doesn't
/// burn in: why it makes its call, and who it is when several act.
#[derive(Debug, Clone, PartialEq)]
pub struct Caption {
    pub session: u64,
    /// Its client's name, when several agents act and it has one.
    pub client: Option<String>,
    /// Why it makes its call under way, when it said.
    pub why: Option<String>,
    /// Where its pointer's tip is.
    pub tip: Spot,
}

impl Caption {
    /// Each agent's caption at `at`: one with a call under way that says
    /// why, or, when several act, that names who makes it; and a pointer
    /// to go beside.
    #[must_use]
    pub fn at(moments: &[Moment], at: f64) -> Vec<Self> {
        let scene = Scene::at(moments, at);
        let named = scene.agents.len() > 1;
        scene
            .agents
            .iter()
            .filter_map(|agent| {
                let current = agent.current.as_ref().map(|(call, _)| call)?;
                let client = (named && !agent.client.is_empty()).then(|| agent.client.clone());
                if current.why.is_none() && client.is_none() {
                    return None;
                }
                Some(Self {
                    session: agent.session,
                    client,
                    why: current.why.clone(),
                    tip: pointer_at(moments, agent.session, at)?,
                })
            })
            .collect()
    }
}

/// How much of something shown from `since` until `until` shows at `at`,
/// fading over `fade` at each end.
fn faded(at: f64, since: f64, until: f64, fade: f64) -> f64 {
    ((at - since) / fade)
        .min((until - at) / fade)
        .clamp(0.0, 1.0)
}

impl Picture {
    /// What is drawn over `moments`' machine, its display `display`, at `at`,
    /// played as it happened.
    #[must_use]
    pub fn at(moments: &[Moment], display: Size, at: f64) -> Self {
        Self::filmed(moments, display, at, |session| session)
    }

    /// What is drawn at `at`, as above, in a film whose clock puts a
    /// session instant at `film` of it: what moves on its own (a wait's
    /// breath) moves at the film's pace, however fast the session plays.
    #[must_use]
    pub fn filmed(moments: &[Moment], display: Size, at: f64, film: impl Fn(f64) -> f64) -> Self {
        let scene = Scene::at(moments, at);
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

            // The pointer carries no words: what it does shows in how it moves.
            let Some(tip) = pointer_at(moments, agent.session, at) else {
                continue;
            };
            let opacity = if agent.current.is_some() {
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
            if let Some((call, since)) = &agent.current
                && call.gesture == Gesture::Wait
            {
                // A slow breath around the pointer while it waits.
                let breath = (std::f64::consts::TAU * (film(at) - film(*since)) / BREATH_MS).sin();
                shapes.push(Shape::Ring {
                    centre: tip,
                    radius: 20.0 + 4.0 * breath,
                    colour: fill,
                    wash: 0.08,
                    width: 2.0,
                    opacity: opacity * (0.55 + 0.25 * breath),
                });
            }
            let badge = Spot {
                x: tip.x + GLYPH_OFFSET.0,
                y: tip.y + GLYPH_OFFSET.1,
            };
            // What worked turns the badge into a check; else it shows what
            // the call under way does.
            if let Some(shown) = worked(moments, agent.session, at) {
                shapes.push(Shape::Glyph {
                    centre: badge,
                    glyph: Glyph::Done,
                    colour: DONE,
                    size: GLYPH_SIZE * (0.6 + 0.4 * ease(shown / POP_MS)),
                    opacity: opacity * ((DONE_MS - shown) / 200.0).clamp(0.0, 1.0),
                });
            } else if let Some(glyph) = agent
                .current
                .as_ref()
                .and_then(|(call, _)| Glyph::of(call.gesture))
            {
                shapes.push(Shape::Glyph {
                    centre: badge,
                    glyph,
                    colour: fill,
                    size: GLYPH_SIZE,
                    opacity,
                });
            }
        }
        landed(moments, at, &mut shapes);
        rewound(moments, display, at, &mut shapes);
        Self { shapes, at }
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

/// A recording's pictures for one video: one per frame at `fps`, on the
/// film's clock ([`Pace`]): pauses fast, the agent's moves slower. Each
/// picture says which instant of the session it shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Reel {
    pub fps: u32,
    pub pictures: Vec<Picture>,
}

/// A film sped up at least this much says so in a corner.
const SPED_UP: f64 = 1.5;

impl Reel {
    /// The pictures over `moments`, on a display `display`, from `from_ms`
    /// until `until_ms` of the session.
    #[must_use]
    pub fn shoot(moments: &[Moment], display: Size, from_ms: f64, until_ms: f64, fps: u32) -> Self {
        let moments = ahead(moments);
        let pace = Pace::of(&moments, from_ms, until_ms);
        let fps = fps.max(1);
        let step = 1000.0 / f64::from(fps);
        let length = pace.length();
        let mut pictures = Vec::new();
        let mut frame: u32 = 0;
        loop {
            let film = f64::from(frame) * step;
            if film >= length {
                break;
            }
            let (at, speed) = pace.session_at(film);
            let mut picture =
                Picture::filmed(&moments, display, at, |session| pace.film_at(session));
            if speed >= SPED_UP {
                picture.shapes.push(Shape::Tag {
                    at: Spot {
                        x: display.width() - 12.0,
                        y: 12.0,
                    },
                    corner: Corner::TopRight,
                    text: format!("▸▸ {speed:.1}×"),
                    ink: WHITE,
                    plate: Rgb(0x20_2233),
                    opacity: 0.85,
                });
            }
            pictures.push(picture);
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
    fn a_caption_says_why_beside_the_pointer_while_the_call_is_under_way() {
        let mut said = click(1, 0.0, 100.0, 100.0);
        said.end_ms = Some(2000.0);
        said.call.why = Some("open the menu".into());
        let [caption] = Caption::at(&[said.clone()], 1000.0).try_into().unwrap();
        assert_eq!(caption.why.as_deref(), Some("open the menu"));
        assert_eq!(caption.client, None, "one agent goes unnamed");
        assert_eq!(caption.tip, Spot { x: 100.0, y: 100.0 });
        assert!(
            Caption::at(&[said.clone()], 3000.0).is_empty(),
            "over once it ends"
        );
        let mut quiet = said.clone();
        quiet.call.why = None;
        assert!(Caption::at(&[quiet], 1000.0).is_empty(), "nothing to say");
        let mut other = click(1, 0.0, 300.0, 300.0);
        other.end_ms = Some(2000.0);
        other.call.key.session = 2;
        other.call.client = "codex".into();
        let both = Caption::at(&[said, other], 1000.0);
        assert_eq!(
            both.iter()
                .map(|caption| (caption.client.as_deref(), caption.why.as_deref()))
                .collect::<Vec<_>>(),
            [
                (Some("claude"), Some("open the menu")),
                (Some("codex"), None)
            ]
        );
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
    fn the_pointer_carries_no_words_and_fades_once_the_agent_stops() {
        let mut said = click(1, 0.0, 100.0, 100.0);
        said.call.why = Some("to open the pricing page".into());
        let moments = [said];
        assert!(tags(&Picture::at(&moments, display(), 10.0)).is_empty());
        // Done, the pointer stays a while, then fades.
        assert_eq!(
            pointer(&Picture::at(&moments, display(), 3000.0))
                .unwrap()
                .1,
            1.0
        );
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

    fn glyphs(picture: &Picture) -> Vec<Glyph> {
        picture
            .shapes
            .iter()
            .filter_map(|shape| {
                if let Shape::Glyph { glyph, .. } = shape {
                    Some(*glyph)
                } else {
                    None
                }
            })
            .collect()
    }

    fn press(request: u64, start_ms: f64) -> Moment {
        moment(
            request,
            start_ms,
            start_ms + 30.0,
            Gesture::Act,
            Target::Element {
                frame: Rect::new(100.0, 100.0, 80.0, 30.0).unwrap(),
                label: "Save".into(),
            },
        )
    }

    #[test]
    fn the_pointer_shows_what_it_does_as_a_glyph() {
        let moments = [
            press(1, 0.0),
            moment(2, 2000.0, 2400.0, Gesture::Run, Target::Nowhere),
        ];
        assert_eq!(
            glyphs(&Picture::at(&moments, display(), 10.0)),
            [Glyph::Press]
        );
        assert_eq!(
            glyphs(&Picture::at(&moments, display(), 2100.0)),
            [Glyph::Command]
        );
        // Between calls: no glyph, only the pointer.
        assert!(glyphs(&Picture::at(&moments, display(), 1800.0)).is_empty());
    }

    #[test]
    fn a_press_dents_in_where_it_lands_then_its_badge_checks_it_worked() {
        let moments = [press(1, 0.0)];
        let dent = Picture::at(&moments, display(), 30.0 + DENT_MS / 2.0);
        assert!(dent.shapes.iter().any(|shape| matches!(
            shape,
            Shape::Ring { centre, radius, .. }
                if *centre == (Spot { x: 140.0, y: 115.0 }) && *radius < DENT_FROM
        )));
        let done = Picture::at(&moments, display(), 30.0 + DENT_MS + 100.0);
        assert_eq!(
            glyphs(&done),
            [Glyph::Done],
            "the check takes the badge's place"
        );
        assert!(glyphs(&Picture::at(&moments, display(), 30.0 + DENT_MS + DONE_MS)).is_empty());

        let mut failed = press(1, 0.0);
        failed.failed = Some("no such element".into());
        let picture = Picture::at(&[failed], display(), 30.0 + DENT_MS + 100.0);
        assert!(
            !glyphs(&picture).contains(&Glyph::Done),
            "no check for what failed"
        );
    }

    #[test]
    fn a_rollback_dims_the_screen_under_a_rewind_then_shows_it_back() {
        let moments = [moment(
            1,
            1000.0,
            3000.0,
            Gesture::Rollback,
            Target::Nowhere,
        )];
        let rewind = |at| {
            Picture::at(&moments, display(), at)
                .shapes
                .iter()
                .find_map(|shape| {
                    if let Shape::Glyph {
                        glyph: Glyph::Rollback,
                        centre,
                        opacity,
                        ..
                    } = shape
                    {
                        Some((*centre, *opacity))
                    } else {
                        None
                    }
                })
        };
        assert_eq!(rewind(2000.0), Some((Spot { x: 640.0, y: 400.0 }, 1.0)));
        assert!(
            rewind(3000.0 + REWIND_MS / 2.0).unwrap().1 < 1.0,
            "fading as it's back"
        );
        assert_eq!(rewind(3000.0 + REWIND_MS), None);
        assert_eq!(rewind(900.0), None);
    }

    #[test]
    fn a_wait_breathes_at_the_films_pace() {
        let moments = [
            press(1, 0.0),
            moment(2, 1000.0, 30_000.0, Gesture::Wait, Target::Nowhere),
        ];
        // A film playing the wait four times fast: a quarter of a breath
        // of session is a sixteenth of one on film, so the ring has barely
        // moved; played as it happened, it has swelled all the way.
        let fast = |session: f64| session / 4.0;
        let radius = |picture: Picture| {
            picture.shapes.iter().find_map(|shape| {
                if let Shape::Ring { radius, centre, .. } = shape {
                    (*centre == Spot { x: 140.0, y: 115.0 }).then_some(*radius)
                } else {
                    None
                }
            })
        };
        let at = 1000.0 + BREATH_MS / 4.0;
        let filmed = radius(Picture::filmed(&moments, display(), at, fast)).unwrap();
        let played = radius(Picture::at(&moments, display(), at)).unwrap();
        assert!((played - 24.0).abs() < 1e-6, "{played}");
        assert!(filmed < 22.0, "{filmed}");
    }

    #[test]
    fn a_wait_breathes_around_the_pointer() {
        let moments = [
            press(1, 0.0),
            moment(2, 1000.0, 5000.0, Gesture::Wait, Target::Nowhere),
        ];
        let radius = |at| {
            Picture::at(&moments, display(), at)
                .shapes
                .iter()
                .find_map(|shape| {
                    if let Shape::Ring { radius, centre, .. } = shape {
                        (*centre == Spot { x: 140.0, y: 115.0 }).then_some(*radius)
                    } else {
                        None
                    }
                })
        };
        let (a, b) = (
            radius(1000.0 + BREATH_MS / 4.0),
            radius(1000.0 + BREATH_MS * 0.75),
        );
        assert!(a.unwrap() > b.unwrap(), "in, then out");
        assert_eq!(
            glyphs(&Picture::at(&moments, display(), 2000.0)),
            [Glyph::Wait]
        );
    }

    #[test]
    fn a_film_sped_up_says_so() {
        let moments = [click(1, 10_000.0, 100.0, 100.0)];
        let reel = Reel::shoot(&moments, display(), 0.0, 11_000.0, 30);
        let tag = &tags(&reel.pictures[0])[0];
        assert!(tag.starts_with("▸▸ "), "{tag}");
        // The pause before the move plays in under a third of its time.
        let moved = reel.pictures.iter().position(|p| p.at >= 9_550.0).unwrap();
        assert!(moved < 30 * 3, "{moved}");
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
    fn a_reel_has_a_picture_per_frame_of_the_film() {
        let moments = [click(1, 0.0, 100.0, 100.0)];
        let reel = Reel::shoot(&moments, display(), 0.0, 1000.0, 30);
        assert!(!reel.pictures.is_empty());
        let mut first = Picture::at(&ahead(&moments), display(), 0.0);
        first.shapes.extend(
            reel.pictures[0]
                .shapes
                .iter()
                .filter(|shape| matches!(shape, Shape::Tag { .. }))
                .cloned(),
        );
        assert_eq!(reel.pictures[0], first);
        assert!(
            reel.pictures
                .windows(2)
                .all(|pair| pair[0].at <= pair[1].at),
            "the session's time only goes forward"
        );
        assert!(
            Reel::shoot(&[], display(), 500.0, 500.0, 30)
                .pictures
                .is_empty()
        );
    }

    #[test]
    fn a_reel_brings_the_pointer_there_as_the_call_lands() {
        let moments = [click(1, 0.0, 100.0, 100.0), click(2, 2000.0, 300.0, 100.0)];
        let reel = Reel::shoot(&moments, display(), 0.0, 2100.0, 100);
        let at = |ms: f64| {
            let picture = reel.pictures.iter().find(|p| p.at >= ms).unwrap();
            pointer(picture).unwrap().0
        };
        assert_eq!(at(2000.0), Spot { x: 300.0, y: 100.0 });
        assert!(at(1700.0).x > 100.0, "already on its way");
        // Never before the agent's previous call ended.
        let close = [click(1, 0.0, 100.0, 100.0), click(2, 100.0, 300.0, 100.0)];
        assert_eq!(ahead(&close)[1].start_ms, 50.0);
    }
}
