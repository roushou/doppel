//! Waiting for something in or about a guest to be ready: try, and try
//! again every so often until it is or a budget runs out.

use std::time::Duration;

use crate::capabilities::Clock;
use crate::files::Filesystem;

use super::Machines;

/// One try: ready with `T`, or not yet, with why (`W`).
pub(crate) enum Poll<T, W> {
    Ready(T),
    NotYet(W),
}

/// The budget ran out: how long it waited, and the last try's why.
#[derive(Debug)]
pub(crate) struct TimedOut<W> {
    pub(crate) waited: Duration,
    pub(crate) last: W,
}

impl<C: Clock, F: Filesystem> Machines<C, F> {
    /// Tries until a try is ready, `every` apart, for at most `budget`;
    /// a try's error ends the wait at once.
    pub(crate) fn poll<T, W, E>(
        &self,
        budget: Duration,
        every: Duration,
        mut try_once: impl FnMut() -> Result<Poll<T, W>, E>,
    ) -> Result<Result<T, TimedOut<W>>, E> {
        let started = self.clock().now();
        loop {
            let last = match try_once()? {
                Poll::Ready(ready) => return Ok(Ok(ready)),
                Poll::NotYet(why) => why,
            };
            let waited = self.clock().now().saturating_duration_since(started);
            if waited >= budget {
                return Ok(Err(TimedOut { waited, last }));
            }
            std::thread::sleep(every);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use doppel_machine::bundle::{DataRoot, RuntimeDir};

    use super::*;
    use crate::capabilities::SystemClock;
    use crate::files::LocalFilesystem;
    use crate::store::Store;
    use crate::vmm::launcher::VmmBinary;

    /// Nothing here touches the store or the VMM.
    fn machines() -> Machines<SystemClock, LocalFilesystem> {
        let unused = PathBuf::from("/nonexistent/doppel-poll");
        Machines::new(
            Store::new(
                DataRoot::new(unused.join("data")).unwrap(),
                RuntimeDir::new(unused.join("run")).unwrap(),
            ),
            VmmBinary::new(unused.join("vmm")),
            SystemClock,
            LocalFilesystem,
        )
    }

    #[test]
    fn a_wait_ends_when_ready_on_an_error_or_when_its_budget_runs_out() {
        let machines = machines();
        let mut tries = 0;
        let ready = machines.poll(Duration::from_secs(60), Duration::ZERO, || {
            tries += 1;
            Ok::<_, ()>(if tries < 3 {
                Poll::NotYet(())
            } else {
                Poll::Ready(tries)
            })
        });
        assert!(matches!(ready, Ok(Ok(3))));
        let failed = machines.poll(Duration::from_secs(60), Duration::ZERO, || {
            Err::<Poll<(), ()>, _>("broken")
        });
        assert!(matches!(failed, Err("broken")));
        let out = machines.poll(Duration::ZERO, Duration::ZERO, || {
            Ok::<Poll<(), _>, ()>(Poll::NotYet("not yet"))
        });
        assert!(matches!(
            out,
            Ok(Err(TimedOut {
                last: "not yet",
                ..
            }))
        ));
    }
}
