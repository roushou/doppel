//! The session's refs: for each machine, the refs of the latest snapshot shown.

use std::collections::HashMap;

use doppel_desktop::ax::changes::{Carried, Known};
use doppel_desktop::ax::compact::{CompactSnapshot, CompactionPolicy, Show};
use doppel_desktop::ax::refs::{Generation, Ref, RefError, RefTable, RefTarget, SnapshotRefs};
use doppel_desktop::ax::tree::AxSnapshot;
use doppel_machine::machine::MachineName;

/// What the model holds of a machine's UI after its latest listing.
#[derive(Debug)]
struct Latest {
    /// The refs that listing issued.
    table: RefTable,
    /// Earlier refs whose elements are as they were.
    carried: Carried,
    /// For the next listing of changes.
    known: Known,
}

#[derive(Debug, Default)]
pub(crate) struct Refs {
    latest: HashMap<MachineName, Latest>,
    /// Generations count across machines, so a ref is never reused in a session.
    issued: Option<Generation>,
    /// Snapshots shown since last taken, with their refs, for consoles.
    shown: Vec<(MachineName, SnapshotRefs)>,
}

impl Refs {
    /// Compacts a snapshot for the model and makes its refs the machine's
    /// latest: all of it, or what changed since the machine's latest
    /// listing (all of it when there's none).
    pub(crate) fn show(
        &mut self,
        machine: &MachineName,
        snapshot: &AxSnapshot,
        policy: CompactionPolicy,
    ) -> CompactSnapshot {
        let generation = self.issued.map_or(Generation::FIRST, Generation::next);
        self.issued = Some(generation);
        let compact = match (policy.show, self.latest.get(machine)) {
            (Show::Changes, Some(latest)) => {
                CompactSnapshot::changes(snapshot, generation, policy, &latest.known)
            }
            (Show::Changes, None) | (Show::All, _) => {
                CompactSnapshot::build(snapshot, generation, policy)
            }
        };
        self.latest.insert(
            machine.clone(),
            Latest {
                table: compact.refs.clone(),
                carried: compact.carried.clone(),
                known: compact.known.clone(),
            },
        );
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

    /// A ref of the latest listing, or an earlier one whose element is as it was.
    ///
    /// # Errors
    /// [`RefError`]: no snapshot of this machine yet, a stale ref, or an unknown one.
    pub(crate) fn resolve(&self, machine: &MachineName, r: Ref) -> Result<&RefTarget, RefError> {
        let latest = self.latest.get(machine).ok_or(RefError::NoSnapshot)?;
        if let Some(target) = latest.carried.get(r) {
            return Ok(target);
        }
        if let Some(what) = latest.carried.gone(r) {
            return Err(RefError::Gone {
                r,
                what: what.to_owned(),
                by: latest.table.generation(),
            });
        }
        latest.table.resolve(r)
    }
}
