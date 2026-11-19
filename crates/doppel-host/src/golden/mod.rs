//! Runs a [`GoldenPlan`] through a machine's running agent.

pub mod plan;

use crate::golden::plan::{GoldenError, GoldenPlan, GoldenStepName};
use crate::guest_step::StepError;
use crate::guest_user::GuestUserName;
use doppel_machine::machine::MachineName;

use crate::agent::AgentError;
use crate::capabilities::Clock;
use crate::files::Filesystem;
use crate::login::{PasswordFile, PasswordFileError};
use crate::machines::{MachineError, Machines};
use doppel_protocol::transport::Cancel;

#[derive(Debug, thiserror::Error)]
pub enum GoldenPrepareError {
    #[error(transparent)]
    Machine(#[from] MachineError),
    #[error(transparent)]
    Password(#[from] PasswordFileError),
    #[error(
        "no login password stored for '{0}' (doppel set-password {0}); golden prepare needs it for sudo and auto-login"
    )]
    NoPassword(MachineName),
    #[error("{step}: {source}")]
    Agent {
        step: &'static str,
        #[source]
        source: AgentError,
    },
    #[error(transparent)]
    Plan(#[from] GoldenError),
}

impl<C: Clock, F: Filesystem> Machines<C, F> {
    /// Applies the golden settings to a running, logged-in machine.
    ///
    /// # Errors
    /// [`GoldenPrepareError`]: no stored password, no agent, or a failed
    /// step (named, with its stderr; never the password).
    pub(crate) fn golden_prepare(
        &self,
        name: &MachineName,
        progress: impl FnMut(GoldenStepName),
    ) -> Result<(), GoldenPrepareError> {
        let password = PasswordFile
            .read(&self.store().bundle(name))?
            .ok_or_else(|| GoldenPrepareError::NoPassword(name.clone()))?;
        let agent = |step: &'static str| move |source| GoldenPrepareError::Agent { step, source };
        let output = self
            .agent(name, self.deadline())?
            .exec(
                &GuestUserName::probe().map_err(GoldenError::from)?,
                &Cancel::never(),
            )
            .map_err(agent("reading the guest user"))?;
        let user = GuestUserName::parse(&output).map_err(GoldenError::from)?;
        let plan = GoldenPlan::new(&user, &password)?;
        self.run_steps(name, &plan.steps, progress)
            .map_err(|error| match error {
                StepError::Machine(error) => GoldenPrepareError::Machine(error),
                StepError::Agent { doing, source } => GoldenPrepareError::Agent {
                    step: doing,
                    source,
                },
                StepError::Failed { step, failed } => {
                    GoldenPrepareError::Plan(GoldenError::StepFailed { step, failed })
                }
            })
    }
}
