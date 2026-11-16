//! The session's refs: for each machine, the refs of the latest snapshot shown.

use std::collections::HashMap;

use doppel_machine::ax::compact::{CompactSnapshot, CompactionPolicy};
use doppel_machine::ax::refs::{Generation, Ref, RefError, RefTarget, SnapshotRefs};
use doppel_machine::ax::tree::AxSnapshot;
use doppel_machine::machine::MachineName;

#[derive(Debug, Default)]
pub(crate) struct Refs {
    latest: HashMap<MachineName, doppel_machine::ax::refs::RefTable>,
    /// Generations count across machines, so a ref is never reused in a session.
    issued: Option<Generation>,
    /// Snapshots shown since last taken, with their refs, for viewers.
    shown: Vec<(MachineName, SnapshotRefs)>,
}

impl Refs {
    /// Compacts a snapshot for the model and makes its refs the machine's latest.
    pub(crate) fn show(
        &mut self,
        machine: &MachineName,
        snapshot: &AxSnapshot,
        policy: CompactionPolicy,
    ) -> CompactSnapshot {
        let generation = self.issued.map_or(Generation::FIRST, Generation::next);
        self.issued = Some(generation);
        let compact = CompactSnapshot::build(snapshot, generation, policy);
        self.latest.insert(machine.clone(), compact.refs.clone());
        self.shown
            .push((machine.clone(), compact.refs.named(snapshot.id)));
        compact
    }

    /// The snapshots shown since last asked, each with the refs the model
    /// was given for its elements.
    pub(crate) fn take_shown(&mut self) -> Vec<(MachineName, SnapshotRefs)> {
        std::mem::take(&mut self.shown)
    }

    /// The machine's guest was replaced (rolled back, restarted): its refs could
    /// now name different elements, so none resolve until the next snapshot.
    pub(crate) fn forget(&mut self, machine: &MachineName) {
        self.latest.remove(machine);
    }

    /// # Errors
    /// [`RefError`]: no snapshot of this machine yet, a stale ref, or an unknown one.
    pub(crate) fn resolve(&self, machine: &MachineName, r: Ref) -> Result<&RefTarget, RefError> {
        self.latest
            .get(machine)
            .ok_or(RefError::NoSnapshot)?
            .resolve(r)
    }
}
