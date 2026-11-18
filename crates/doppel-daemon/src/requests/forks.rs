//! Running fresh forks of a checkpoint side by side: which names they get,
//! and whether they fit. Pure; the daemon makes and starts them.

use std::sync::{Mutex, PoisonError};

use doppel_machine::bundle::CheckpointLabel;
use doppel_machine::machine::{Entropy, MacAddress, MachineName, SpecError};

/// Where fresh forks' MAC addresses come from. A port: the CLI passes the
/// system's randomness, tests a fixed sequence.
pub trait MacSource: Send + Sync + std::fmt::Debug {
    /// # Errors
    /// No address could be made.
    fn mac(&self) -> Result<MacAddress, MacError>;
}

/// Why a MAC address couldn't be made.
#[derive(Debug, thiserror::Error)]
#[error("no MAC address for a fork: {0}")]
pub struct MacError(pub String);

/// MAC addresses drawn from an entropy source, as VZ makes them.
#[derive(Debug)]
pub struct EntropyMacs<E>(Mutex<E>);

impl<E> EntropyMacs<E> {
    pub const fn new(entropy: E) -> Self {
        Self(Mutex::new(entropy))
    }
}

impl<E> MacSource for EntropyMacs<E>
where
    E: Entropy + Send + std::fmt::Debug,
    E::Error: std::fmt::Display,
{
    fn mac(&self) -> Result<MacAddress, MacError> {
        let mut entropy = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        MacAddress::generate(&mut *entropy).map_err(|error| MacError(error.to_string()))
    }
}

/// The forks a request will make: checked to fit, named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForkPlan {
    names: Vec<MachineName>,
}

/// Why forks can't run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PlanError {
    #[error("a run needs at least one fork")]
    NoForks,
    #[error(
        "{count} forks don't fit: macOS runs {max} VMs at a time and {running} {verb} running, so {free} more can start",
        max = ForkPlan::MAX_RUNNING,
        verb = if *running == 1 { "is" } else { "are" },
    )]
    TooMany {
        count: u32,
        running: usize,
        free: usize,
    },
    #[error("fork names would be too long: {0}")]
    Name(SpecError),
}

impl ForkPlan {
    /// Apple's limit on VMs running at once on a Mac.
    pub(crate) const MAX_RUNNING: usize = 2;

    /// `count` forks of `source`'s `label`, named `{source}-{label}-{n}`
    /// with the lowest numbers no machine in `existing` has, given that
    /// `running` VMs run already.
    pub(crate) fn new(
        source: &MachineName,
        label: &CheckpointLabel,
        count: u32,
        existing: &[MachineName],
        running: usize,
    ) -> Result<Self, PlanError> {
        if count == 0 {
            return Err(PlanError::NoForks);
        }
        let free = Self::MAX_RUNNING.saturating_sub(running);
        let wanted = match usize::try_from(count) {
            Ok(wanted) if wanted <= free => wanted,
            Ok(_) | Err(_) => {
                return Err(PlanError::TooMany {
                    count,
                    running,
                    free,
                });
            }
        };
        let mut names = Vec::with_capacity(wanted);
        for n in 1_u64.. {
            if names.len() == wanted {
                break;
            }
            let name: MachineName = format!("{source}-{}-{n}", label.as_str())
                .parse()
                .map_err(PlanError::Name)?;
            if !existing.contains(&name) {
                names.push(name);
            }
        }
        Ok(Self { names })
    }

    pub(crate) fn names(&self) -> &[MachineName] {
        &self.names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(raw: &str) -> MachineName {
        raw.parse().unwrap_or_else(|error| panic!("{error}"))
    }

    fn label(raw: &str) -> CheckpointLabel {
        raw.parse().unwrap_or_else(|error| panic!("{error:?}"))
    }

    #[test]
    fn forks_take_the_lowest_free_numbers() {
        let plan = ForkPlan::new(&name("dev"), &label("clean"), 2, &[], 0);
        assert_eq!(
            plan.map(|plan| plan.names().to_vec()),
            Ok(vec![name("dev-clean-1"), name("dev-clean-2")])
        );
        let existing = [name("dev-clean-1"), name("dev-clean-3"), name("dev")];
        let plan = ForkPlan::new(&name("dev"), &label("clean"), 2, &existing, 0);
        assert_eq!(
            plan.map(|plan| plan.names().to_vec()),
            Ok(vec![name("dev-clean-2"), name("dev-clean-4")])
        );
    }

    #[test]
    fn forks_must_fit_beside_the_running_machines() {
        assert_eq!(
            ForkPlan::new(&name("dev"), &label("clean"), 2, &[], 1),
            Err(PlanError::TooMany {
                count: 2,
                running: 1,
                free: 1
            })
        );
        assert_eq!(
            ForkPlan::new(&name("dev"), &label("clean"), 3, &[], 0),
            Err(PlanError::TooMany {
                count: 3,
                running: 0,
                free: 2
            })
        );
        assert!(ForkPlan::new(&name("dev"), &label("clean"), 1, &[], 1).is_ok());
        assert_eq!(
            ForkPlan::new(&name("dev"), &label("clean"), 1, &[], 2),
            Err(PlanError::TooMany {
                count: 1,
                running: 2,
                free: 0
            })
        );
        assert_eq!(
            ForkPlan::new(&name("dev"), &label("clean"), 0, &[], 0),
            Err(PlanError::NoForks)
        );
    }

    #[test]
    fn a_name_too_long_for_a_machine_is_refused() {
        let long = name(&"a".repeat(25));
        assert!(matches!(
            ForkPlan::new(&long, &label("clean"), 1, &[], 0),
            Err(PlanError::Name(_))
        ));
    }
}
