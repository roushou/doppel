//! The daemon's stop signals: SIGTERM, which launchd sends to stop it, and
//! SIGINT (Ctrl-C) in the foreground. The first stops it cleanly; a second,
//! while it's still finishing, ends the process at once.
//!
//! The signals are blocked and waited for on a thread of their own
//! (`sigwait`), so no handler runs. A blocked mask is inherited by threads
//! spawned later, so they're blocked before the daemon starts any; programs
//! it starts don't inherit it, as `std::process::Command` clears the mask.

use anyhow::{Context, Result};
use doppel_daemon::Stopper;
use nix::sys::signal::{SigSet, Signal};

/// The stop signals, blocked on this thread and every one it starts from
/// now on, until [`Self::stop`] waits for them.
#[derive(Debug)]
pub(crate) struct StopSignals(SigSet);

impl StopSignals {
    /// Blocks them: call it before any thread is started.
    pub(crate) fn block() -> Result<Self> {
        let mut signals = SigSet::empty();
        signals.add(Signal::SIGTERM);
        signals.add(Signal::SIGINT);
        signals
            .thread_block()
            .context("blocking the stop signals")?;
        Ok(Self(signals))
    }

    /// Waits for them on a thread of its own, and stops the daemon at the
    /// first.
    pub(crate) fn stop(self, stopper: Stopper) -> Result<()> {
        std::thread::Builder::new()
            .name("doppeld-signals".into())
            .spawn(move || self.wait(&stopper))
            .context("starting the signal thread")?;
        Ok(())
    }

    fn wait(&self, stopper: &Stopper) {
        let mut stopping = false;
        loop {
            match self.0.wait() {
                Ok(signal) if stopping => {
                    eprintln!("doppeld: {signal} again: exiting without finishing");
                    std::process::exit(1);
                }
                Ok(signal) => {
                    eprintln!("doppeld: {signal}: finishing open sessions");
                    stopping = true;
                    stopper.stop();
                }
                Err(errno) => {
                    eprintln!("doppeld: waiting for signals failed: {errno}");
                    return;
                }
            }
        }
    }
}
