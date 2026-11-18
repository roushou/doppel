//! Whether the host's view of a machine's screen is current.
//!
//! After a VM resumes from saved memory (a start that resumes, a rollback,
//! a checkpoint), Virtualization.framework rebuilds its paravirtual
//! graphics, and for a few seconds the host keeps showing the screen as it
//! was saved while the guest draws on. Screenshots, text recognition and
//! viewers all read the host's view, and a stale frame looks the same as a
//! still screen. So the guest draws on purpose: its agent shows a small
//! square of a colour the host's view doesn't have there (a [`Probe`]), the
//! host samples its view until the square appears, then the agent hides it
//! and the host waits until it's gone. [`ProbeRun`] is that procedure as a
//! state machine; [`HostScreen`] is what it found.
//!
//! The agent removes a probe by itself after [`PROBE_LASTS`], so a shell
//! that stops midway leaves at most a small square in the corner for that
//! long.

use std::fmt;
use std::time::{Duration, Instant};

use crate::geometry::{Point, Rect};

/// How long the agent keeps a probe on screen, at most, when nobody hides it.
pub const PROBE_LASTS: Duration = Duration::from_secs(30);

/// An sRGB colour, 8 bits a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    red: u8,
    green: u8,
    blue: u8,
}

impl Rgb {
    #[must_use]
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }

    #[must_use]
    pub const fn red(self) -> u8 {
        self.red
    }

    #[must_use]
    pub const fn green(self) -> u8 {
        self.green
    }

    #[must_use]
    pub const fn blue(self) -> u8 {
        self.blue
    }

    /// The largest difference in any one channel.
    const fn distance(self, other: Self) -> u8 {
        let red = self.red.abs_diff(other.red);
        let green = self.green.abs_diff(other.green);
        let blue = self.blue.abs_diff(other.blue);
        let most = if red > green { red } else { green };
        if most > blue { most } else { blue }
    }

    /// The differences in all three channels, summed.
    fn spread(self, other: Self) -> u16 {
        u16::from(self.red.abs_diff(other.red))
            + u16::from(self.green.abs_diff(other.green))
            + u16::from(self.blue.abs_diff(other.blue))
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.red, self.green, self.blue)
    }
}

/// A square the agent draws in the display's top-left corner, above every
/// window, in a colour far from what the host's view shows there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    color: Rgb,
}

impl Probe {
    /// Its side, in points.
    const SIDE: u16 = 4;
    /// Colours to draw it in: pure, so colour management moves them little.
    const COLORS: [Rgb; 5] = [
        Rgb::new(255, 0, 0),
        Rgb::new(0, 255, 0),
        Rgb::new(0, 0, 255),
        Rgb::new(0, 0, 0),
        Rgb::new(255, 255, 255),
    ];
    /// How far a sampled channel may be from the probe's and still be it:
    /// the display's colour conversion shifts a pure colour by less.
    const TOLERANCE: u8 = 64;

    /// A probe to draw over `underneath`, the host's view where it goes:
    /// the colour farthest from it.
    #[must_use]
    pub fn over(underneath: Rgb) -> Self {
        let mut color = Self::COLORS[0];
        for candidate in Self::COLORS {
            if candidate.spread(underneath) > color.spread(underneath) {
                color = candidate;
            }
        }
        Self { color }
    }

    /// Where it's drawn, in display points.
    #[must_use]
    pub fn frame() -> Rect {
        Rect::corner_square(Self::SIDE)
    }

    /// Where the host samples its view for it: the probe's centre.
    #[must_use]
    pub fn sample_at() -> Point {
        Self::frame().center()
    }

    #[must_use]
    pub const fn color(self) -> Rgb {
        self.color
    }

    /// Whether `sample`, taken at [`Self::sample_at`], shows the probe.
    #[must_use]
    pub const fn seen_in(self, sample: Rgb) -> bool {
        self.color.distance(sample) <= Self::TOLERANCE
    }
}

/// Whether screenshots show the guest's screen as it is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostScreen {
    /// They do: the probe reached the host's view `after` the check began.
    Current { after: Duration },
    /// They do, but the probe may still show in the corner for up to
    /// [`PROBE_LASTS`]: hiding it failed, or it was still drawn a while
    /// after (`why`).
    CurrentWithProbe { after: Duration, why: String },
    /// Whether they show the screen as it is or as it was is unknown.
    Unconfirmed(WhyUnconfirmed),
}

/// Why [`HostScreen`] couldn't be confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhyUnconfirmed {
    /// The guest's agent can't draw a probe: it predates them.
    ProbeUnsupported,
    /// The probe was drawn, but the host's view didn't show it within `waited`.
    NotShown { waited: Duration },
    /// A step failed: why.
    Failed(String),
}

impl fmt::Display for HostScreen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Current { after } => {
                write!(f, "the screen was current after {} ms", after.as_millis())
            }
            Self::CurrentWithProbe { after, why } => write!(
                f,
                "the screen was current after {} ms, but a small square may show in its top-left corner for up to {} s ({why})",
                after.as_millis(),
                PROBE_LASTS.as_secs()
            ),
            Self::Unconfirmed(why) => write!(
                f,
                "screenshots may show the screen as it was for a few seconds: {why}"
            ),
        }
    }
}

impl fmt::Display for WhyUnconfirmed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProbeUnsupported => f.write_str(
                "the guest's agent is too old to show it has caught up (doppel agent install updates it)",
            ),
            Self::NotShown { waited } => write!(
                f,
                "the guest's drawing didn't reach the host within {} s",
                waited.as_secs()
            ),
            Self::Failed(why) => write!(f, "checking failed ({why})"),
        }
    }
}

/// How long each wait of a [`ProbeRun`] may last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeLimits {
    /// For the probe to appear: the host's view lags by up to ~10 s.
    pub show: Duration,
    /// For it to disappear once hidden: the view is current by then.
    pub clear: Duration,
    /// Between two samples.
    pub interval: Duration,
}

impl ProbeLimits {
    pub const DEFAULT: Self = Self {
        show: Duration::from_secs(20),
        clear: Duration::from_secs(2),
        interval: Duration::from_millis(20),
    };
}

/// What the shell does next.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub enum Action {
    /// Sample the host's view at [`Probe::sample_at`], no earlier than
    /// `at`, then report `Sampled`.
    SampleAt(Instant),
    /// Have the agent draw it, then report `Shown`.
    Show(Probe),
    /// Have the agent remove it, then report `Hidden`.
    Hide,
    Finish(HostScreen),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Sampled {
        color: Rgb,
        now: Instant,
    },
    Shown {
        now: Instant,
    },
    Hidden {
        now: Instant,
    },
    /// The last action failed: why.
    Failed {
        why: String,
        now: Instant,
    },
}

impl Event {
    const fn name(&self) -> &'static str {
        match self {
            Self::Sampled { .. } => "Sampled",
            Self::Shown { .. } => "Shown",
            Self::Hidden { .. } => "Hidden",
            Self::Failed { .. } => "Failed",
        }
    }
}

/// What happens once the probe is hidden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AfterHiding {
    /// It was seen: wait for it to go.
    Clear { after: Duration },
    /// It wasn't: finish with this.
    Finish(HostScreen),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeState {
    /// Sampling the corner before drawing anything.
    Sampling {
        started: Instant,
    },
    Showing {
        started: Instant,
        probe: Probe,
    },
    /// Drawn; sampling until the host's view shows it.
    Watching {
        started: Instant,
        probe: Probe,
    },
    Hiding {
        probe: Probe,
        then: AfterHiding,
    },
    /// Hidden; sampling until the host's view no longer shows it.
    Clearing {
        probe: Probe,
        after: Duration,
        hidden: Instant,
    },
    Done(HostScreen),
}

impl ProbeState {
    const fn name(&self) -> &'static str {
        match self {
            Self::Sampling { .. } => "sampling",
            Self::Showing { .. } => "showing",
            Self::Watching { .. } => "watching",
            Self::Hiding { .. } => "hiding",
            Self::Clearing { .. } => "clearing",
            Self::Done(_) => "done",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("screen probe: {event} isn't expected while {state}")]
pub struct IllegalProbeEvent {
    pub state: &'static str,
    pub event: &'static str,
}

/// The probe procedure with its fixed inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeRun {
    pub limits: ProbeLimits,
}

impl ProbeRun {
    pub fn start(self, now: Instant) -> (ProbeState, Vec<Action>) {
        (
            ProbeState::Sampling { started: now },
            vec![Action::SampleAt(now)],
        )
    }

    /// # Errors
    /// [`IllegalProbeEvent`] for an event the state doesn't expect, such as
    /// `Shown` while sampling or anything after `Done`.
    pub fn step(
        self,
        state: ProbeState,
        event: Event,
    ) -> Result<(ProbeState, Vec<Action>), IllegalProbeEvent> {
        let illegal = IllegalProbeEvent {
            state: state.name(),
            event: event.name(),
        };
        let hide = |probe, then| (ProbeState::Hiding { probe, then }, vec![Action::Hide]);
        let failed = |why| HostScreen::Unconfirmed(WhyUnconfirmed::Failed(why));
        Ok(match (state, event) {
            (ProbeState::Sampling { started }, Event::Sampled { color, .. }) => {
                let probe = Probe::over(color);
                (
                    ProbeState::Showing { started, probe },
                    vec![Action::Show(probe)],
                )
            }
            (ProbeState::Sampling { .. }, Event::Failed { why, .. }) => Self::done(failed(why)),
            (ProbeState::Showing { started, probe }, Event::Shown { now }) => (
                ProbeState::Watching { started, probe },
                vec![Action::SampleAt(now)],
            ),
            // It may be drawn even so: hide it.
            (ProbeState::Showing { probe, .. }, Event::Failed { why, .. }) => {
                hide(probe, AfterHiding::Finish(failed(why)))
            }
            (ProbeState::Watching { started, probe }, Event::Sampled { color, now }) => {
                let waited = now.saturating_duration_since(started);
                if probe.seen_in(color) {
                    hide(probe, AfterHiding::Clear { after: waited })
                } else if waited >= self.limits.show {
                    hide(
                        probe,
                        AfterHiding::Finish(HostScreen::Unconfirmed(WhyUnconfirmed::NotShown {
                            waited,
                        })),
                    )
                } else {
                    (
                        ProbeState::Watching { started, probe },
                        vec![Action::SampleAt(now + self.limits.interval)],
                    )
                }
            }
            (ProbeState::Watching { probe, .. }, Event::Failed { why, .. }) => {
                hide(probe, AfterHiding::Finish(failed(why)))
            }
            (ProbeState::Hiding { probe, then }, Event::Hidden { now }) => match then {
                AfterHiding::Clear { after } => (
                    ProbeState::Clearing {
                        probe,
                        after,
                        hidden: now,
                    },
                    vec![Action::SampleAt(now)],
                ),
                AfterHiding::Finish(screen) => Self::done(screen),
            },
            (ProbeState::Hiding { then, .. }, Event::Failed { why, .. }) => match then {
                AfterHiding::Clear { after } => Self::done(HostScreen::CurrentWithProbe {
                    after,
                    why: format!("hiding it failed: {why}"),
                }),
                // Already unconfirmed; the agent removes it in time.
                AfterHiding::Finish(screen) => Self::done(screen),
            },
            (
                ProbeState::Clearing {
                    probe,
                    after,
                    hidden,
                },
                Event::Sampled { color, now },
            ) => {
                let since = now.saturating_duration_since(hidden);
                if !probe.seen_in(color) {
                    Self::done(HostScreen::Current { after })
                } else if since >= self.limits.clear {
                    Self::done(HostScreen::CurrentWithProbe {
                        after,
                        why: format!("still drawn {} ms after hiding it", since.as_millis()),
                    })
                } else {
                    (
                        ProbeState::Clearing {
                            probe,
                            after,
                            hidden,
                        },
                        vec![Action::SampleAt(now + self.limits.interval)],
                    )
                }
            }
            (ProbeState::Clearing { after, .. }, Event::Failed { why, .. }) => {
                Self::done(HostScreen::CurrentWithProbe {
                    after,
                    why: format!("checking it was gone failed: {why}"),
                })
            }
            (
                ProbeState::Sampling { .. }
                | ProbeState::Showing { .. }
                | ProbeState::Watching { .. }
                | ProbeState::Hiding { .. }
                | ProbeState::Clearing { .. }
                | ProbeState::Done(_),
                Event::Sampled { .. }
                | Event::Shown { .. }
                | Event::Hidden { .. }
                | Event::Failed { .. },
            ) => return Err(illegal),
        })
    }

    fn done(screen: HostScreen) -> (ProbeState, Vec<Action>) {
        (
            ProbeState::Done(screen.clone()),
            vec![Action::Finish(screen)],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUN: ProbeRun = ProbeRun {
        limits: ProbeLimits::DEFAULT,
    };
    const MENU_BAR: Rgb = Rgb::new(236, 236, 236);

    #[expect(
        clippy::disallowed_methods,
        reason = "an arbitrary anchor: only differences from it are asserted"
    )]
    fn anchor() -> Instant {
        Instant::now()
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Steps through `events` from the start, returning the last actions.
    fn run(t0: Instant, events: Vec<Event>) -> (ProbeState, Vec<Action>) {
        let (mut state, mut actions) = RUN.start(t0);
        for event in events {
            (state, actions) = RUN.step(state, event).unwrap();
        }
        (state, actions)
    }

    #[test]
    fn the_probe_is_a_colour_far_from_whats_there() {
        assert_eq!(Probe::over(MENU_BAR).color(), Rgb::new(0, 0, 0));
        assert_eq!(
            Probe::over(Rgb::new(10, 10, 10)).color(),
            Rgb::new(255, 255, 255)
        );
        let probe = Probe::over(Rgb::new(0, 0, 0));
        assert!(!probe.seen_in(Rgb::new(0, 0, 0)));
        // Colour conversion moves it a little; it's still the probe.
        assert!(probe.seen_in(Rgb::new(250, 238, 245)));
    }

    #[test]
    fn a_probe_that_shows_then_goes_means_the_screen_is_current() {
        let t0 = anchor();
        let probe = Probe::over(MENU_BAR);
        let (state, actions) = run(
            t0,
            vec![
                Event::Sampled {
                    color: MENU_BAR,
                    now: t0,
                },
                Event::Shown { now: t0 + ms(5) },
                // The stale frame, then the probe.
                Event::Sampled {
                    color: MENU_BAR,
                    now: t0 + ms(10),
                },
                Event::Sampled {
                    color: probe.color(),
                    now: t0 + ms(3500),
                },
            ],
        );
        assert_eq!(actions, vec![Action::Hide]);
        let (state, actions) = RUN
            .step(state, Event::Hidden { now: t0 + ms(3510) })
            .unwrap();
        assert_eq!(actions, vec![Action::SampleAt(t0 + ms(3510))]);
        let (_, actions) = RUN
            .step(
                state,
                Event::Sampled {
                    color: MENU_BAR,
                    now: t0 + ms(3530),
                },
            )
            .unwrap();
        assert_eq!(
            actions,
            vec![Action::Finish(HostScreen::Current { after: ms(3500) })]
        );
    }

    #[test]
    fn samples_are_spaced_by_the_interval() {
        let t0 = anchor();
        let (_, actions) = run(
            t0,
            vec![
                Event::Sampled {
                    color: MENU_BAR,
                    now: t0,
                },
                Event::Shown { now: t0 },
                Event::Sampled {
                    color: MENU_BAR,
                    now: t0 + ms(3),
                },
            ],
        );
        assert_eq!(actions, vec![Action::SampleAt(t0 + ms(23))]);
    }

    #[test]
    fn a_probe_never_seen_is_hidden_and_the_screen_unconfirmed() {
        let t0 = anchor();
        let (state, actions) = run(
            t0,
            vec![
                Event::Sampled {
                    color: MENU_BAR,
                    now: t0,
                },
                Event::Shown { now: t0 },
                Event::Sampled {
                    color: MENU_BAR,
                    now: t0 + ms(20_000),
                },
            ],
        );
        assert_eq!(actions, vec![Action::Hide]);
        let (_, actions) = RUN
            .step(
                state,
                Event::Hidden {
                    now: t0 + ms(20_010),
                },
            )
            .unwrap();
        assert_eq!(
            actions,
            vec![Action::Finish(HostScreen::Unconfirmed(
                WhyUnconfirmed::NotShown { waited: ms(20_000) }
            ))]
        );
    }

    #[test]
    fn a_failed_show_still_hides_what_may_be_drawn() {
        let t0 = anchor();
        let (state, actions) = run(
            t0,
            vec![
                Event::Sampled {
                    color: MENU_BAR,
                    now: t0,
                },
                Event::Failed {
                    why: "agent gone".into(),
                    now: t0,
                },
            ],
        );
        assert_eq!(actions, vec![Action::Hide]);
        let (_, actions) = RUN
            .step(
                state,
                Event::Failed {
                    why: "agent gone".into(),
                    now: t0,
                },
            )
            .unwrap();
        assert_eq!(
            actions,
            vec![Action::Finish(HostScreen::Unconfirmed(
                WhyUnconfirmed::Failed("agent gone".into())
            ))]
        );
    }

    #[test]
    fn a_failed_first_sample_draws_nothing() {
        let t0 = anchor();
        let (_, actions) = run(
            t0,
            vec![Event::Failed {
                why: "no VMM".into(),
                now: t0,
            }],
        );
        assert_eq!(
            actions,
            vec![Action::Finish(HostScreen::Unconfirmed(
                WhyUnconfirmed::Failed("no VMM".into())
            ))]
        );
    }

    #[test]
    fn a_probe_left_drawn_is_said() {
        let t0 = anchor();
        let probe = Probe::over(MENU_BAR);
        let seen = vec![
            Event::Sampled {
                color: MENU_BAR,
                now: t0,
            },
            Event::Shown { now: t0 },
            Event::Sampled {
                color: probe.color(),
                now: t0 + ms(40),
            },
        ];
        // Hiding failed.
        let mut failing = seen.clone();
        failing.push(Event::Failed {
            why: "timeout".into(),
            now: t0 + ms(50),
        });
        let (_, actions) = run(t0, failing);
        assert!(matches!(
            &actions[..],
            [Action::Finish(HostScreen::CurrentWithProbe { after, .. })] if *after == ms(40)
        ));
        // Hidden, but still drawn past the limit.
        let mut lingering = seen;
        lingering.push(Event::Hidden { now: t0 + ms(50) });
        lingering.push(Event::Sampled {
            color: probe.color(),
            now: t0 + ms(2050),
        });
        let (_, actions) = run(t0, lingering);
        assert!(matches!(
            &actions[..],
            [Action::Finish(HostScreen::CurrentWithProbe { why, .. })] if why.contains("2000 ms")
        ));
    }

    #[test]
    fn unexpected_events_are_refused() {
        let t0 = anchor();
        let (state, _) = RUN.start(t0);
        assert_eq!(
            RUN.step(state, Event::Shown { now: t0 }),
            Err(IllegalProbeEvent {
                state: "sampling",
                event: "Shown"
            })
        );
        let done = ProbeState::Done(HostScreen::Current { after: ms(1) });
        assert!(
            RUN.step(
                done,
                Event::Sampled {
                    color: MENU_BAR,
                    now: t0
                }
            )
            .is_err()
        );
    }

    #[test]
    fn the_probe_sits_in_the_corner() {
        let at = Probe::sample_at();
        assert_eq!((at.x(), at.y()), (2.0, 2.0));
        assert!(Probe::frame().contains(at));
    }
}
