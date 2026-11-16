//! When something must be finished by, carried with each request.
//!
//! A timeout set on a connection is state: it outlives the call it was set
//! for, applies to each read rather than to the call, and must be put back.
//! A [`Deadline`] is a value instead: the shell turns it into exactly the
//! time left before every blocking step, so a call that takes several reads
//! still ends on time, and a sequence of calls can share one deadline.

use std::time::{Duration, Instant};

/// Monotonic time, to measure deadlines against. A port: the shell passes
/// the system's in, and tests a clock they move by hand.
pub trait Clock {
    fn now(&self) -> Instant;
}

/// A borrowed clock is a clock: a short-lived client borrows its owner's.
impl<T: Clock + ?Sized> Clock for &T {
    fn now(&self) -> Instant {
        (**self).now()
    }
}

/// A moment by which an operation must finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Deadline(Instant);

/// The time left before a [`Deadline`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remaining {
    /// Never zero: a deadline reached exactly has [`Remaining::Passed`].
    Left(Duration),
    Passed,
}

impl Deadline {
    /// The longest budget a deadline holds; larger ones are cut to it. Far
    /// beyond any Doppel operation, and small enough that adding it to an
    /// [`Instant`] can't overflow.
    pub const LONGEST: Duration = Duration::from_secs(24 * 60 * 60);

    /// `budget` from `now`.
    #[must_use]
    pub fn after(now: Instant, budget: Duration) -> Self {
        Self(now + budget.min(Self::LONGEST))
    }

    #[must_use]
    pub fn remaining(self, now: Instant) -> Remaining {
        match self.0.checked_duration_since(now) {
            Some(left) if !left.is_zero() => Remaining::Left(left),
            Some(_) | None => Remaining::Passed,
        }
    }

    /// Whichever comes first: a step inside a larger operation must end by
    /// both its own deadline and the operation's.
    #[must_use]
    pub fn earlier(self, other: Self) -> Self {
        self.min(other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[expect(
        clippy::disallowed_methods,
        reason = "an arbitrary anchor: only differences from it are asserted"
    )]
    fn anchor() -> Instant {
        Instant::now()
    }

    #[test]
    fn time_left_shrinks_to_passed_and_never_reports_zero() {
        let start = anchor();
        let by = Deadline::after(start, Duration::from_secs(2));
        assert_eq!(by.remaining(start), Remaining::Left(Duration::from_secs(2)));
        assert_eq!(
            by.remaining(start + Duration::from_millis(1500)),
            Remaining::Left(Duration::from_millis(500))
        );
        assert_eq!(
            by.remaining(start + Duration::from_secs(2)),
            Remaining::Passed
        );
        assert_eq!(
            by.remaining(start + Duration::from_secs(3)),
            Remaining::Passed
        );
        assert_eq!(
            Deadline::after(start, Duration::ZERO).remaining(start),
            Remaining::Passed
        );
    }

    #[test]
    fn the_earlier_deadline_wins_and_huge_budgets_are_capped() {
        let start = anchor();
        let soon = Deadline::after(start, Duration::from_secs(1));
        let later = Deadline::after(start, Duration::from_secs(5));
        assert_eq!(soon.earlier(later), soon);
        assert_eq!(later.earlier(soon), soon);
        assert_eq!(
            Deadline::after(start, Duration::MAX).remaining(start),
            Remaining::Left(Deadline::LONGEST)
        );
    }
}
