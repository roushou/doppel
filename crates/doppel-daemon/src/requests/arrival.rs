//! A request as it arrives: who may ask it, the machine it names (parsed
//! once, so a bad name is refused before it joins a session or a line),
//! whether it waits its turn, and what it may do to the guest.

use doppel_machine::machine::MachineName;
use doppel_protocol::pb::daemon::v1::daemon_request::Operation;

use crate::call_events::Guest;
use crate::requests::Refusal;
use crate::requests::handle::{Askers, Traits, Turn};

/// What a request is, settled before it runs.
#[derive(Debug, Clone)]
pub(crate) struct Arrival {
    pub(crate) askers: Askers,
    pub(crate) turn: Turn,
    pub(crate) guest: Guest,
    /// The machine it names; a name that isn't one never gets here.
    pub(crate) machine: Option<MachineName>,
    pub(crate) looked_for: Option<String>,
}

impl Arrival {
    /// # Errors
    /// The machine it names isn't a machine name.
    pub(crate) fn of(operation: Option<&Operation>) -> Result<Self, Refusal> {
        let Some(traits) = operation.map(Traits::of) else {
            return Ok(Self {
                askers: Askers::Anyone,
                turn: Turn::Skips,
                guest: Guest::Kept,
                machine: None,
                looked_for: None,
            });
        };
        Ok(Self {
            askers: traits.askers,
            turn: traits.turn,
            guest: traits.guest,
            machine: traits
                .machine
                .map(|(field, raw)| Refusal::name(field, raw))
                .transpose()?,
            looked_for: traits.looked_for.map(str::to_owned),
        })
    }

    /// The machine whose line it takes its place in, in the order requests
    /// arrived (see [`crate::scheduler`]): only one that waits its turn.
    pub(crate) fn reserves(&self) -> Option<MachineName> {
        match self.turn {
            Turn::Waits => self.machine.clone(),
            Turn::Skips => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use doppel_protocol::pb::daemon::v1 as daemon;
    use doppel_protocol::rpc::Rpc;

    use super::*;

    #[test]
    fn a_bad_machine_name_is_refused_on_arrival_by_its_own_field() {
        let fork = daemon::ForkMachine {
            source: "Not A Name".into(),
            ..Default::default()
        };
        let refused = Arrival::of(Some(&fork.into_operation())).unwrap_err();
        assert!(
            matches!(
                refused,
                Refusal::Argument {
                    field: "source",
                    ..
                }
            ),
            "{refused:?}"
        );
    }

    #[test]
    fn only_a_request_that_waits_takes_a_place_in_its_machine_s_line() {
        let start = daemon::StartMachine {
            name: "dev".into(),
            ..Default::default()
        };
        let arrival = Arrival::of(Some(&start.into_operation())).unwrap();
        assert_eq!(arrival.reserves(), Some("dev".parse().unwrap()));
        let control = daemon::TakeControl { name: "dev".into() };
        let arrival = Arrival::of(Some(&control.into_operation())).unwrap();
        assert_eq!(
            (arrival.machine.is_some(), arrival.reserves()),
            (true, None)
        );
        assert!(Arrival::of(None).unwrap().machine.is_none());
    }
}
