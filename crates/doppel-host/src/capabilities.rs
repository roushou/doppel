//! The only place ambient capabilities are touched. Everything else receives
//! them as values (design principles §5).

use std::time::{Instant, SystemTime};

pub use doppel_desktop::deadline::Clock;
use doppel_machine::machine::Entropy;

/// Time as people name it, beside the monotonic [`Clock`] to measure with:
/// to name things by when they happened.
pub trait WallClock: Clock {
    fn wall(&self) -> SystemTime;
}

impl<T: WallClock + ?Sized> WallClock for &T {
    fn wall(&self) -> SystemTime {
        (**self).wall()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one real Clock implementation"
    )]
    fn now(&self) -> Instant {
        Instant::now()
    }
}

impl WallClock for SystemClock {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one real Clock implementation"
    )]
    fn wall(&self) -> SystemTime {
        SystemTime::now()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct OsEntropy;

impl Entropy for OsEntropy {
    type Error = getrandom::Error;
    fn fill(&mut self, bytes: &mut [u8]) -> Result<(), Self::Error> {
        getrandom::fill(bytes)
    }
}
