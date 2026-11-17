//! A replay's clock: the session's time, played or paused, from 0 to the
//! session's length. It runs on its own, not on a video's: a recording's
//! video ends at the screen's last change, which may be long before the
//! trace does, and the trace must still play and seek to its end. The
//! video follows the clock. Pure: the wall clock is passed in, in ms.

/// Where the clock stands. Times are in ms; `at` is the session's time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Transport {
    Paused { at: f64, length: f64 },
    Playing { from: f64, since: f64, length: f64 },
    Ended { length: f64 },
}

impl Transport {
    /// Paused at the start of a session `length` ms long.
    pub fn start(length: f64) -> Self {
        Self::Paused {
            at: 0.0,
            length: length.max(0.0),
        }
    }

    pub const fn length(self) -> f64 {
        match self {
            Self::Paused { length, .. } | Self::Playing { length, .. } | Self::Ended { length } => {
                length
            }
        }
    }

    pub const fn playing(self) -> bool {
        match self {
            Self::Playing { .. } => true,
            Self::Paused { .. } | Self::Ended { .. } => false,
        }
    }

    /// The session's time at the wall clock's `wall`.
    pub fn at(self, wall: f64) -> f64 {
        match self {
            Self::Paused { at, .. } => at,
            Self::Playing {
                from,
                since,
                length,
            } => (from + (wall - since).max(0.0)).min(length),
            Self::Ended { length } => length,
        }
    }

    /// Plays from where it is; from the start again once at the end.
    #[must_use]
    pub fn play(self, wall: f64) -> Self {
        match self {
            Self::Paused { at, length } => Self::Playing {
                from: if at >= length { 0.0 } else { at },
                since: wall,
                length,
            },
            Self::Playing { .. } => self,
            Self::Ended { length } => Self::Playing {
                from: 0.0,
                since: wall,
                length,
            },
        }
    }

    /// Stops where it is.
    #[must_use]
    pub fn pause(self, wall: f64) -> Self {
        match self {
            Self::Paused { .. } | Self::Ended { .. } => self,
            Self::Playing { length, .. } => Self::Paused {
                at: self.at(wall),
                length,
            },
        }
    }

    /// Moves to the session's time `ms`, clamped to the session; playing on
    /// if it was.
    #[must_use]
    pub fn seek(self, ms: f64, wall: f64) -> Self {
        let length = self.length();
        let at = ms.clamp(0.0, length);
        match self {
            Self::Playing { .. } => Self::Playing {
                from: at,
                since: wall,
                length,
            },
            Self::Paused { .. } | Self::Ended { .. } => Self::Paused { at, length },
        }
    }

    /// The clock as of `wall`: a playing clock that reached the end has ended.
    #[must_use]
    pub fn settle(self, wall: f64) -> Self {
        match self {
            Self::Playing { length, .. } if self.at(wall) >= length => Self::Ended { length },
            Self::Playing { .. } | Self::Paused { .. } | Self::Ended { .. } => self,
        }
    }

    /// A longer session, as more of it becomes known (a video's length,
    /// once open). Never shorter.
    #[must_use]
    pub fn lengthen(self, length: f64, wall: f64) -> Self {
        if length <= self.length() {
            return self;
        }
        match self {
            Self::Paused { at, .. } => Self::Paused { at, length },
            Self::Playing { from, since, .. } => Self::Playing {
                from,
                since,
                length,
            },
            Self::Ended { .. } => Self::Paused {
                at: self.at(wall),
                length,
            },
        }
    }
}

/// Where a video should be for the clock to show the session's time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Follow {
    /// Seconds into the video, inside it.
    Inside(f64),
    /// Past its end: held on its last frame, this many seconds in.
    After(f64),
}

impl Follow {
    /// Where a video `duration` seconds long (unknown until open) should be
    /// for the session's time `seconds` into it.
    pub fn of(seconds: f64, duration: Option<f64>) -> Self {
        match duration {
            // Just inside the end: the last frame, which a reader finds.
            Some(duration) if seconds >= duration => Self::After((duration - 0.001).max(0.0)),
            Some(_) | None => Self::Inside(seconds.max(0.0)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plays_on_the_wall_clock_and_pauses_where_it_got_to() {
        let t = Transport::start(5000.0).play(1000.0);
        assert_eq!(t.at(1500.0), 500.0);
        let t = t.pause(1800.0);
        assert_eq!(
            t,
            Transport::Paused {
                at: 800.0,
                length: 5000.0
            }
        );
        assert_eq!(t.at(9999.0), 800.0);
    }

    #[test]
    fn plays_to_the_sessions_end_then_ends() {
        let t = Transport::start(5000.0).play(0.0);
        assert_eq!(t.at(7000.0), 5000.0);
        assert!(t.settle(4999.0).playing());
        assert_eq!(t.settle(5000.0), Transport::Ended { length: 5000.0 });
    }

    #[test]
    fn plays_from_the_start_again_once_ended() {
        let ended = Transport::start(100.0).play(0.0).settle(200.0);
        assert_eq!(ended.play(300.0).at(350.0), 50.0);
        let at_end = Transport::start(100.0).seek(500.0, 0.0);
        assert_eq!(at_end.play(0.0).at(10.0), 10.0);
    }

    #[test]
    fn seeks_within_the_session_keeping_play_or_pause() {
        let playing = Transport::start(5000.0).play(0.0).seek(3000.0, 100.0);
        assert_eq!(playing.at(600.0), 3500.0);
        let paused = Transport::start(5000.0).seek(9000.0, 0.0);
        assert_eq!(paused.at(0.0), 5000.0);
        assert_eq!(paused.seek(-5.0, 0.0).at(0.0), 0.0);
    }

    #[test]
    fn lengthens_but_never_shortens() {
        let t = Transport::start(1000.0);
        assert_eq!(t.lengthen(500.0, 0.0), t);
        let ended = t.play(0.0).settle(2000.0);
        assert_eq!(
            ended.lengthen(3000.0, 0.0),
            Transport::Paused {
                at: 1000.0,
                length: 3000.0
            }
        );
    }

    #[test]
    fn a_video_holds_its_last_frame_past_its_end() {
        assert_eq!(Follow::of(2.0, Some(10.0)), Follow::Inside(2.0));
        assert_eq!(Follow::of(20.0, None), Follow::Inside(20.0));
        assert_eq!(Follow::of(12.0, Some(10.0)), Follow::After(9.999));
        assert_eq!(Follow::of(1.0, Some(0.0)), Follow::After(0.0));
    }
}
