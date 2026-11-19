//! Runs [`GuestStep`]s through a machine's running agent, in order: a fresh
//! connection for each, a write or a run that must exit 0.

pub mod plan;

use doppel_machine::machine::MachineName;
use doppel_protocol::transport::Cancel;

use crate::agent::AgentError;
use crate::capabilities::Clock;
use crate::files::Filesystem;
use crate::guest_step::plan::{GuestAction, GuestStep, RunFailed};
use crate::machines::{MachineError, Machines};

/// Why a step didn't go: no agent, the agent failed while `doing` it, or a
/// run didn't exit 0. `N` names the step.
#[derive(Debug)]
pub(crate) enum StepError<N> {
    Machine(MachineError),
    Agent {
        doing: &'static str,
        source: AgentError,
    },
    Failed {
        step: N,
        failed: RunFailed,
    },
}

impl<C: Clock, F: Filesystem> Machines<C, F> {
    /// Runs `steps` in order, telling `progress` each one's name as it
    /// starts; the first that fails ends the run.
    pub(crate) fn run_steps<N: Copy>(
        &self,
        machine: &MachineName,
        steps: &[GuestStep<N>],
        mut progress: impl FnMut(N),
    ) -> Result<(), StepError<N>> {
        let agent = |doing: &'static str| move |source| StepError::Agent { doing, source };
        for step in steps {
            progress(step.name);
            let mut client = self
                .agent(machine, self.deadline())
                .map_err(StepError::Machine)?;
            match &step.action {
                GuestAction::Write(request) => {
                    client
                        .write_file(request, self.deadline())
                        .map_err(agent("writing"))?;
                }
                GuestAction::Run(request) => {
                    let output = client
                        .exec(request, &Cancel::never())
                        .map_err(agent("running"))?;
                    RunFailed::judge(&output).map_err(|failed| StepError::Failed {
                        step: step.name,
                        failed,
                    })?;
                }
            }
        }
        Ok(())
    }
}
