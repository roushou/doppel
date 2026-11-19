//! Runs an [`InstallPlan`] through a machine's running agent, then waits
//! until the new build answers.

pub mod plan;

use std::time::{Duration, Instant};

use crate::agent_install::plan::{AgentBuild, GuestAccount, InstallError, InstallPlan, StepName};
use crate::guest_step::StepError;
use doppel_machine::machine::MachineName;

use crate::agent::AgentError;
use crate::capabilities::Clock;
use crate::files::Filesystem;
use crate::machines::{AgentReach, MachineError, Machines, Poll, TimedOut};
use doppel_protocol::transport::Cancel;

/// How long the new build has to answer after the restart.
pub const RESTART_TIMEOUT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(500);

#[derive(Debug, thiserror::Error)]
pub enum AgentInstallError {
    #[error(transparent)]
    Machine(#[from] MachineError),
    #[error("{step}: {source}")]
    Agent {
        step: &'static str,
        #[source]
        source: AgentError,
    },
    #[error(transparent)]
    Plan(#[from] InstallError),
    #[error(
        "build {build} didn't answer within {waited:?} of the restart; {last}. See ~/Library/Logs/doppel-agent.log in the guest"
    )]
    NotBack {
        build: AgentBuild,
        waited: Duration,
        last: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct Installed {
    pub build: AgentBuild,
    /// From the restart to the new build's first answer.
    pub restart: Duration,
}

impl<C: Clock, F: Filesystem> Machines<C, F> {
    /// Installs `archive` (a zipped `Doppel Agent.app`) as the machine's
    /// LaunchAgent, through whatever agent answers now.
    ///
    /// # Errors
    /// [`AgentInstallError`]: no agent, a failed step, or no answer from the
    /// new build in time (its state is then unknown: look at its log).
    pub(crate) fn install_agent(
        &self,
        machine: &MachineName,
        archive: Vec<u8>,
        build: AgentBuild,
        progress: impl FnMut(StepName),
    ) -> Result<Installed, AgentInstallError> {
        let agent = |step: &'static str| {
            move |source: AgentError| AgentInstallError::Agent { step, source }
        };
        let probe = GuestAccount::probe()?;
        let output = self
            .agent(machine, self.deadline())?
            .exec(&probe, &Cancel::never())
            .map_err(agent("reading the guest account"))?;
        let plan = InstallPlan::new(archive, build, &GuestAccount::parse(&output)?)?;

        self.run_steps(machine, &plan.steps, progress)
            .map_err(|error| match error {
                StepError::Machine(error) => AgentInstallError::Machine(error),
                StepError::Agent { doing, source } => AgentInstallError::Agent {
                    step: doing,
                    source,
                },
                StepError::Failed { step, failed } => {
                    AgentInstallError::Plan(InstallError::StepFailed { step, failed })
                }
            })?;
        self.await_build(machine, plan.build)
    }

    fn await_build(
        &self,
        machine: &MachineName,
        build: AgentBuild,
    ) -> Result<Installed, AgentInstallError> {
        let clock = self.clock();
        let started: Instant = clock.now();
        // Only an agent not up yet is waited for: a stopped machine or a
        // wrong answer ends the wait at once.
        let answered = self.poll(RESTART_TIMEOUT, POLL, || {
            Ok::<_, MachineError>(match self.reach_agent(machine, self.deadline())? {
                AgentReach::Answers(client) => match client.peer_build() {
                    Some(answered) if *answered == build => Poll::Ready(()),
                    Some(other) => Poll::NotYet(format!("build {other} answered")),
                    None => Poll::NotYet("an agent without a build id answered".to_owned()),
                },
                AgentReach::NotYet(why) => Poll::NotYet(why),
            })
        })?;
        match answered {
            Ok(()) => Ok(Installed {
                restart: clock.now().saturating_duration_since(started),
                build,
            }),
            Err(TimedOut { waited, last }) => Err(AgentInstallError::NotBack {
                build,
                waited,
                last,
            }),
        }
    }
}
