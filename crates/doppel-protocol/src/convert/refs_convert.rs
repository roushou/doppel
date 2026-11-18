use doppel_desktop::ax::refs::{Generation, SnapshotRefs};
use doppel_desktop::ax::tree::{NodeIndex, SnapshotId};

use super::ParseError;
use crate::pb::daemon::v1 as daemon;

impl TryFrom<daemon::ElementRefs> for SnapshotRefs {
    type Error = ParseError;
    fn try_from(wire: daemon::ElementRefs) -> Result<Self, ParseError> {
        let generation = Generation::new(wire.generation).ok_or(ParseError::Invalid {
            field: "generation",
            reason: "generations count from 1".into(),
        })?;
        Ok(Self::new(
            SnapshotId::new(wire.snapshot),
            generation,
            wire.nodes.into_iter().map(NodeIndex::new).collect(),
        ))
    }
}

impl From<&SnapshotRefs> for daemon::ElementRefs {
    fn from(refs: &SnapshotRefs) -> Self {
        Self {
            snapshot: refs.snapshot().get(),
            generation: refs.generation().get(),
            nodes: refs.nodes().iter().map(|node| node.get()).collect(),
        }
    }
}
