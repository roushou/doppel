//! The shell that runs the core's screen probe (see
//! [`doppel_machine::screen_probe`]): it samples the host's view through
//! the VMM and has the agent show and hide the probe, over one connection
//! to each.

use doppel_machine::machine::MachineName;
use doppel_machine::screen_probe::{
    Action, Event, HostScreen, Probe, ProbeLimits, ProbeRun, WhyUnconfirmed,
};

use crate::agent::Probes;
use crate::capabilities::Clock;
use crate::files::Filesystem;
use crate::machines::{MachineError, Machines};
use doppel_protocol::transport::{Cancel, Slept};

impl<C: Clock, F: Filesystem> Machines<C, F> {
    /// See [`crate::machines::Machine::catch_up_screen`].
    pub(crate) fn catch_up_screen(
        &self,
        name: &MachineName,
        cancel: &Cancel,
    ) -> Result<HostScreen, MachineError> {
        let mut running = self.machine(name).running(self.deadline())?;
        let mut agent = match running.agent(self.deadline()) {
            Ok(agent) => agent,
            Err(error) => {
                return Ok(HostScreen::Unconfirmed(WhyUnconfirmed::Failed(
                    error.to_string(),
                )));
            }
        };
        match agent.probes() {
            Probes::Supported => {}
            Probes::Unsupported => {
                return Ok(HostScreen::Unconfirmed(WhyUnconfirmed::ProbeUnsupported));
            }
        }
        let run = ProbeRun {
            limits: ProbeLimits::DEFAULT,
        };
        let clock = self.clock();
        let mut cancelled = false;
        let (mut state, mut pending) = run.start(clock.now());
        loop {
            let mut events = Vec::new();
            for action in pending {
                let event = match action {
                    Action::Finish(screen) => {
                        return if cancelled {
                            Err(MachineError::Cancelled)
                        } else {
                            Ok(screen)
                        };
                    }
                    Action::SampleAt(at) => {
                        match cancel.sleep(at.saturating_duration_since(clock.now())) {
                            Slept::Elapsed => {
                                match running.sample_display(Probe::sample_at(), self.deadline()) {
                                    Ok(color) => Event::Sampled {
                                        color,
                                        now: clock.now(),
                                    },
                                    Err(error) => Event::Failed {
                                        why: error.to_string(),
                                        now: clock.now(),
                                    },
                                }
                            }
                            // The core then hides the probe, if it shows.
                            Slept::Cancelled => {
                                cancelled = true;
                                Event::Failed {
                                    why: "cancelled".into(),
                                    now: clock.now(),
                                }
                            }
                        }
                    }
                    Action::Show(probe) => match agent.show_probe(probe, self.deadline()) {
                        Ok(()) => Event::Shown { now: clock.now() },
                        Err(error) => Event::Failed {
                            why: error.to_string(),
                            now: clock.now(),
                        },
                    },
                    Action::Hide => match agent.hide_probe(self.deadline()) {
                        Ok(()) => Event::Hidden { now: clock.now() },
                        Err(error) => Event::Failed {
                            why: error.to_string(),
                            now: clock.now(),
                        },
                    },
                };
                events.push(event);
            }
            pending = Vec::new();
            for event in events {
                let (next, actions) = run.step(state, event)?;
                state = next;
                pending.extend(actions);
            }
        }
    }
}
