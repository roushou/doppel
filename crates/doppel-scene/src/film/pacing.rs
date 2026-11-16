//! A film's clock: how fast each stretch of a session plays. An agent
//! thinks in pauses where nothing moves and acts in bursts too quick to
//! follow; a film plays those pauses fast and the agent's own moves
//! slower. Pure: the timeline in; a map from the film's time to the
//! session's out.

use crate::attention::SWEEP_MS;
use crate::timeline::{Gesture, Moment};

/// How fast a stretch where nothing moves plays.
pub const IDLE_SPEED: f64 = 3.5;
/// How fast the agent's own moves play.
pub const MOVE_SPEED: f64 = 0.6;
/// How long after a move ends it still plays at its speed.
const MOVE_TAIL_MS: f64 = 300.0;
/// How long a snapshot's sweep, and a beat after, plays at its own speed.
const SWEEP_TAIL_MS: f64 = 150.0;

/// Whether a call of this gesture moves something on screen worth watching
/// slower. Looks and waits show as the sweep, at its own speed.
const fn moves(gesture: Gesture) -> bool {
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
        | Gesture::Lifecycle
        | Gesture::Checkpoint
        | Gesture::Rollback
        | Gesture::Mark => true,
        Gesture::Look | Gesture::Wait | Gesture::Other => false,
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

/// A film's clock over a session, from `from_ms` until `until_ms`: the
/// stretches it plays, each at its speed.
#[derive(Debug, Clone, PartialEq)]
pub struct Pace {
    stretches: Vec<Stretch>,
}

impl Pace {
    /// The clock for `moments`, from `from_ms` until `until_ms` of the session.
    #[must_use]
    pub fn of(moments: &[Moment], from_ms: f64, until_ms: f64) -> Self {
        // Where each speed holds: the slowest that covers an instant wins.
        let mut windows: Vec<(f64, f64, f64)> = Vec::new();
        for moment in moments.iter().filter(|moment| !moment.person) {
            let end = moment.end_ms.unwrap_or(until_ms).max(moment.start_ms);
            if moves(moment.call.gesture) {
                windows.push((moment.start_ms, end + MOVE_TAIL_MS, MOVE_SPEED));
            } else if moment.seen.is_some() {
                windows.push((end, end + SWEEP_MS + SWEEP_TAIL_MS, 1.0));
            }
        }
        let mut cuts: Vec<f64> = vec![from_ms, until_ms];
        for &(from, until, _) in &windows {
            cuts.extend([from, until]);
        }
        cuts.retain(|cut| (from_ms..=until_ms).contains(cut));
        cuts.sort_by(f64::total_cmp);
        cuts.dedup();
        let stretches = cuts
            .windows(2)
            .filter_map(|pair| {
                let (from, until) = (pair[0], pair[1]);
                (until > from).then(|| {
                    let middle = f64::midpoint(from, until);
                    Stretch {
                        from,
                        until,
                        speed: windows
                            .iter()
                            .filter(|(start, end, _)| (*start..*end).contains(&middle))
                            .map(|(_, _, speed)| *speed)
                            .reduce(f64::min)
                            .unwrap_or(IDLE_SPEED),
                    }
                })
            })
            .collect();
        Self { stretches }
    }

    /// How long the film runs.
    #[must_use]
    pub fn length(&self) -> f64 {
        self.stretches.iter().map(Stretch::played).sum()
    }

    /// The film's time at the session's `at`.
    #[must_use]
    pub fn film_at(&self, at: f64) -> f64 {
        let mut film = 0.0;
        for stretch in &self.stretches {
            if at <= stretch.from {
                break;
            }
            film += (at.min(stretch.until) - stretch.from) / stretch.speed;
        }
        film
    }

    /// The session's time at the film's `film`, and the speed it plays at;
    /// past the end, the end.
    #[must_use]
    pub fn session_at(&self, film: f64) -> (f64, f64) {
        let mut left = film;
        for stretch in &self.stretches {
            let played = stretch.played();
            if left < played {
                return (stretch.from + left * stretch.speed, stretch.speed);
            }
            left -= played;
        }
        (self.stretches.last().map_or(0.0, |last| last.until), 1.0)
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
        assert_eq!(pace.session_at(pace.film_at(5050.0) + 1.0).1, MOVE_SPEED);
        assert_eq!(pace.session_at(pace.film_at(10_000.0)).1, IDLE_SPEED);
        // The map goes both ways.
        for at in [0.0, 2500.0, 5050.0, 9000.0] {
            let (back, _) = pace.session_at(pace.film_at(at));
            assert!((back - at).abs() < 1e-6, "{at} came back as {back}");
        }
    }

    #[test]
    fn a_wait_plays_fast_too() {
        let moments = [call(1000.0, 9000.0, Gesture::Wait)];
        let pace = Pace::of(&moments, 0.0, 10_000.0);
        assert_eq!(pace.session_at(pace.film_at(5000.0)).1, IDLE_SPEED);
        assert!((pace.length() - 10_000.0 / IDLE_SPEED).abs() < 1e-6);
    }
}
