//! A film's clock: how fast each stretch of a session plays. An agent
//! thinks in pauses where nothing moves and acts in bursts too quick to
//! follow; a film plays those pauses fast and the agent's own moves
//! slower, easing from one speed into the next so it never lurches. Pure:
//! the timeline in; a map from the film's time to the session's out.

use crate::attention::SWEEP_MS;
use crate::timeline::{Gesture, Moment};

/// How fast a stretch where nothing moves plays, and the fastest anything
/// plays: a film that shows the agent faster than this oversells it.
pub const IDLE_SPEED: f64 = 4.0;
/// How fast the agent's own moves play.
pub const MOVE_SPEED: f64 = 0.6;
/// How long a move plays at its speed from its start: the pointer lands.
/// A call that takes longer (an app that's slow to open) plays the rest as
/// a pause until it ends.
const MOVE_HOLD_MS: f64 = 300.0;
/// How long after a move ends it still plays at its speed: the screen
/// answers.
const MOVE_TAIL_MS: f64 = 300.0;
/// How long a snapshot's sweep, and a beat after, plays at its own speed.
const SWEEP_TAIL_MS: f64 = 150.0;
/// A stretch where nothing moves shorter than this, between moves, plays
/// as it happened: a burst of moves reads as one gesture.
const BURST_GAP_MS: f64 = 800.0;
/// How long the film takes to ease from one speed into the next.
const EASE_MS: f64 = 400.0;
/// How long a call on the machine itself (a start, a checkpoint, a
/// rollback) plays, at most as fast as [`IDLE_SPEED`]: long enough to
/// see, short enough not to wait through.
const MACHINE_MS: f64 = 1200.0;

/// How a call of a gesture plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Played {
    /// It moves something on screen worth watching slower.
    Slow,
    /// It works on the machine as a whole: it plays in [`MACHINE_MS`].
    Machine,
    /// It shows as the sweep of what it saw, at its own speed (looks,
    /// waits), or as nothing.
    Swept,
}

const fn played(gesture: Gesture) -> Played {
    match gesture {
        Gesture::Click
        | Gesture::DoubleClick
        | Gesture::SecondaryClick
        | Gesture::Move
        | Gesture::Drag
        | Gesture::Scroll
        | Gesture::Key
        | Gesture::Type
        | Gesture::Act
        | Gesture::Run
        | Gesture::Mark => Played::Slow,
        Gesture::Lifecycle | Gesture::Checkpoint | Gesture::Rollback => Played::Machine,
        Gesture::Look | Gesture::Wait | Gesture::Other => Played::Swept,
    }
}

/// A stretch of the session, from `from` until `until`, played at `speed`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Stretch {
    from: f64,
    until: f64,
    speed: f64,
}

impl Stretch {
    fn played(&self) -> f64 {
        (self.until - self.from) / self.speed
    }
}

/// A film's clock over a session, from `from_ms` until `until_ms`. Each
/// stretch has its own speed, and the clock eases between them: the
/// session's time at a film instant is the average, over [`EASE_MS`]
/// around it, of where the stretches alone would put it. So speed changes
/// as a ramp, and the film is as long as its stretches played.
#[derive(Debug, Clone, PartialEq)]
pub struct Pace {
    /// Where each stretch starts, and where the last ends: (film, session),
    /// both increasing.
    knots: Vec<(f64, f64)>,
}

impl Pace {
    /// The clock for `moments`, from `from_ms` until `until_ms` of the session.
    #[must_use]
    pub fn of(moments: &[Moment], from_ms: f64, until_ms: f64) -> Self {
        // Where each speed holds: the slowest that covers an instant wins.
        let mut windows: Vec<(f64, f64, f64)> = Vec::new();
        for moment in moments.iter().filter(|moment| !moment.person) {
            let end = moment.end_ms.unwrap_or(until_ms).max(moment.start_ms);
            match played(moment.call.gesture) {
                Played::Slow => {
                    let held = (moment.start_ms + MOVE_HOLD_MS).min(end);
                    windows.push((moment.start_ms, held, MOVE_SPEED));
                    windows.push((end, end + MOVE_TAIL_MS, MOVE_SPEED));
                }
                Played::Machine => {
                    let speed = ((end - moment.start_ms) / MACHINE_MS).clamp(1.0, IDLE_SPEED);
                    windows.push((moment.start_ms, end, speed));
                    windows.push((end, end + MOVE_TAIL_MS, MOVE_SPEED));
                }
                Played::Swept => {
                    if moment.seen.is_some() {
                        windows.push((end, end + SWEEP_MS + SWEEP_TAIL_MS, 1.0));
                    }
                }
            }
        }
        let mut cuts: Vec<f64> = vec![from_ms, until_ms];
        for &(from, until, _) in &windows {
            cuts.extend([from, until]);
        }
        cuts.retain(|cut| (from_ms..=until_ms).contains(cut));
        cuts.sort_by(f64::total_cmp);
        cuts.dedup();
        let stretches: Vec<Stretch> = cuts
            .windows(2)
            .filter_map(|pair| {
                let (from, until) = (pair[0], pair[1]);
                (until > from).then(|| {
                    let middle = f64::midpoint(from, until);
                    let idle = if until - from < BURST_GAP_MS {
                        1.0
                    } else {
                        IDLE_SPEED
                    };
                    Stretch {
                        from,
                        until,
                        speed: windows
                            .iter()
                            .filter(|(start, end, _)| (*start..*end).contains(&middle))
                            .map(|(_, _, speed)| *speed)
                            .reduce(f64::min)
                            .unwrap_or(idle),
                    }
                })
            })
            .collect();
        let mut knots = vec![(0.0, from_ms)];
        let mut film = 0.0;
        for stretch in &stretches {
            film += stretch.played();
            knots.push((film, stretch.until));
        }
        Self { knots }
    }

    /// How long the film runs.
    #[must_use]
    pub fn length(&self) -> f64 {
        self.knots.last().map_or(0.0, |(film, _)| *film)
    }

    /// Where the stretches alone put the film's `film` in the session,
    /// carried on at the first and last stretch's speed beyond the film.
    fn stepped(&self, film: f64) -> f64 {
        let (Some(first), Some(last)) = (self.knots.first(), self.knots.last()) else {
            return 0.0;
        };
        if self.knots.len() < 2 {
            return first.1;
        }
        let pair = if film <= first.0 {
            [self.knots[0], self.knots[1]]
        } else if film >= last.0 {
            [self.knots[self.knots.len() - 2], *last]
        } else {
            let after = self.knots.partition_point(|(at, _)| *at <= film);
            [self.knots[after - 1], self.knots[after]]
        };
        let [(f0, s0), (f1, s1)] = pair;
        s0 + (film - f0) * (s1 - s0) / (f1 - f0)
    }

    /// The area under [`Self::stepped`] from `a` until `b`: exact, since
    /// it's a straight line between knots.
    fn area(&self, a: f64, b: f64) -> f64 {
        let mut points = vec![a];
        points.extend(
            self.knots
                .iter()
                .map(|(film, _)| *film)
                .filter(|film| (a..b).contains(film) && *film > a),
        );
        points.push(b);
        points
            .windows(2)
            .map(|pair| {
                (pair[1] - pair[0]) * f64::midpoint(self.stepped(pair[0]), self.stepped(pair[1]))
            })
            .sum()
    }

    /// The session's time at the film's `film`, and the speed it plays at
    /// there; past the end, the end.
    #[must_use]
    pub fn session_at(&self, film: f64) -> (f64, f64) {
        let (Some(first), Some(last)) = (self.knots.first(), self.knots.last()) else {
            return (0.0, 1.0);
        };
        if film >= last.0 {
            return (last.1, 1.0);
        }
        let (a, b) = (film - EASE_MS / 2.0, film + EASE_MS / 2.0);
        let at = (self.area(a, b) / EASE_MS).clamp(first.1, last.1);
        let speed = (self.stepped(b) - self.stepped(a)) / EASE_MS;
        (at, speed)
    }

    /// The film's time at the session's `at`.
    #[must_use]
    pub fn film_at(&self, at: f64) -> f64 {
        let (mut low, mut high) = (0.0, self.length());
        for _ in 0..64 {
            let middle = f64::midpoint(low, high);
            if self.session_at(middle).0 < at {
                low = middle;
            } else {
                high = middle;
            }
        }
        high
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::{Call, CallKey, Target};

    fn call(start_ms: f64, end_ms: f64, gesture: Gesture) -> Moment {
        Moment {
            start_ms,
            end_ms: Some(end_ms),
            call: Call {
                key: CallKey {
                    session: 1,
                    request: 1,
                },
                machine: "dev".parse().unwrap(),
                client: "claude".into(),
                caption: "a call".into(),
                gesture,
                target: Target::Nowhere,
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

    #[test]
    fn pauses_play_fast_and_moves_slow() {
        let moments = [call(5000.0, 5100.0, Gesture::Click)];
        let pace = Pace::of(&moments, 0.0, 20_000.0);
        // Five idle seconds before the move go by in well under two.
        assert!(pace.film_at(5000.0) < 2000.0, "{}", pace.film_at(5000.0));
        // In the middle of its landing and answer, it plays at its speed.
        assert!((pace.session_at(pace.film_at(5200.0)).1 - MOVE_SPEED).abs() < 1e-9);
        assert!(pace.session_at(pace.film_at(10_000.0)).1 >= IDLE_SPEED);
        // The map goes both ways.
        for at in [0.0, 2500.0, 5050.0, 9000.0] {
            let (back, _) = pace.session_at(pace.film_at(at));
            assert!((back - at).abs() < 1e-6, "{at} came back as {back}");
        }
    }

    #[test]
    fn a_slow_call_plays_slow_only_as_it_lands_and_ends() {
        // An app that takes two seconds to open.
        let moments = [call(1000.0, 3000.0, Gesture::Run)];
        let pace = Pace::of(&moments, 0.0, 5000.0);
        let speed = |at: f64| pace.session_at(pace.film_at(at)).1;
        assert!((speed(1150.0) - MOVE_SPEED).abs() < 1e-9, "as it lands");
        assert!(speed(2150.0) >= IDLE_SPEED - 1e-9, "while it works");
        assert!(
            (speed(3150.0) - MOVE_SPEED).abs() < 1e-9,
            "as the screen answers"
        );
    }

    #[test]
    fn nothing_plays_faster_than_idle_speed() {
        let moments = [
            call(1000.0, 1100.0, Gesture::Click),
            call(61_400.0, 61_500.0, Gesture::Click),
            call(62_000.0, 82_000.0, Gesture::Rollback),
        ];
        let pace = Pace::of(&moments, 0.0, 83_000.0);
        let mut film = 0.0;
        while film < pace.length() {
            let speed = pace.session_at(film).1;
            assert!(speed <= IDLE_SPEED + 1e-9, "{speed} at {film}");
            film += 10.0;
        }
        // A minute's thought plays as a quarter of one, give or take the
        // easing in and out of it.
        let long = pace.film_at(61_400.0) - pace.film_at(1400.0);
        assert!((long - 60_000.0 / IDLE_SPEED).abs() < EASE_MS, "{long}");
    }

    #[test]
    fn a_rollback_plays_in_moments() {
        let moments = [call(1000.0, 4000.0, Gesture::Rollback)];
        let pace = Pace::of(&moments, 1000.0, 4000.0);
        assert!(
            (pace.length() - MACHINE_MS).abs() < 1e-6,
            "{}",
            pace.length()
        );
        // A quick one plays as it happened.
        let quick = [call(1000.0, 1500.0, Gesture::Checkpoint)];
        let pace = Pace::of(&quick, 1000.0, 1500.0);
        assert!((pace.length() - 500.0).abs() < 1e-6, "{}", pace.length());
    }

    #[test]
    fn a_wait_plays_fast_too() {
        let moments = [call(1000.0, 9000.0, Gesture::Wait)];
        let pace = Pace::of(&moments, 0.0, 10_000.0);
        assert!(pace.session_at(pace.film_at(5000.0)).1 >= IDLE_SPEED);
        assert!(
            pace.length() <= 10_000.0 / IDLE_SPEED + 1e-6,
            "{}",
            pace.length()
        );
    }

    /// A busy session: clicks close together, a long think, a wait.
    fn busy() -> Vec<Moment> {
        vec![
            call(1000.0, 1100.0, Gesture::Click),
            call(1400.0, 1500.0, Gesture::Type),
            call(1900.0, 2000.0, Gesture::Key),
            call(4000.0, 4100.0, Gesture::Act),
            call(21_000.0, 21_100.0, Gesture::Click),
            call(22_000.0, 26_000.0, Gesture::Wait),
            call(26_500.0, 26_600.0, Gesture::Click),
        ]
    }

    #[test]
    fn the_speed_eases_from_one_frame_to_the_next() {
        let pace = Pace::of(&busy(), 0.0, 28_000.0);
        let frame = 1000.0 / 30.0;
        let mut film = 0.0;
        let mut before = pace.session_at(0.0).1;
        while film < pace.length() - frame {
            film += frame;
            let speed = pace.session_at(film).1;
            // A frame changes the speed by a small step, however far apart
            // the speeds it eases between.
            let most = frame / EASE_MS * 15.0;
            assert!(
                (speed - before).abs() <= most,
                "at {film}: {before} to {speed}"
            );
            before = speed;
        }
    }

    #[test]
    fn easing_keeps_the_film_in_step_with_the_session() {
        let pace = Pace::of(&busy(), 0.0, 28_000.0);
        let mut last = 0.0;
        let mut film = 0.0;
        while film < pace.length() {
            let (at, _) = pace.session_at(film);
            assert!(at >= last, "the session never runs backwards");
            last = at;
            film += 10.0;
        }
        assert!((pace.session_at(pace.length()).0 - 28_000.0).abs() < 1e-6);
        for at in [500.0, 1450.0, 3000.0, 12_000.0, 24_000.0] {
            let (back, _) = pace.session_at(pace.film_at(at));
            assert!((back - at).abs() < 1e-3, "{at} came back as {back}");
        }
    }

    #[test]
    fn a_burst_of_moves_plays_as_one_gesture() {
        // Clicks 400 ms apart: the gap between plays as it happened, not
        // sped up between two slow landings.
        let moments = [
            call(1000.0, 1050.0, Gesture::Click),
            call(1950.0, 2000.0, Gesture::Click),
        ];
        let pace = Pace::of(&moments, 0.0, 3000.0);
        let gap = pace.film_at(1950.0) - pace.film_at(1350.0);
        assert!((gap - 600.0).abs() < EASE_MS / 2.0, "{gap}");
    }
}
