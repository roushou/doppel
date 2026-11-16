//! Refs: the handles a model uses to name an element it saw in a snapshot.
//!
//! A ref names its snapshot (`s3e12` is element 12 of the session's third
//! snapshot), so a ref from an older snapshot can never be mistaken for one in
//! the latest. Only the latest snapshot's refs resolve; older ones are a typed
//! [`RefError::Stale`]. The agent then re-checks the element itself before acting.

use std::fmt;
use std::str::FromStr;

use super::tree::{ElementTarget, NodeIndex, Role, SnapshotId};

/// The number of a snapshot shown in one session, from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Generation(u32);

impl Generation {
    pub const FIRST: Self = Self(1);

    /// `None` for 0: generations count from 1.
    #[must_use]
    pub const fn new(raw: u32) -> Option<Self> {
        if raw == 0 { None } else { Some(Self(raw)) }
    }

    /// The generation after this one.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for Generation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "s{}", self.0)
    }
}

/// `s<generation>e<element>`, both from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ref {
    generation: Generation,
    element: u32,
}

impl Ref {
    #[must_use]
    pub const fn generation(self) -> Generation {
        self.generation
    }
}

impl fmt::Display for Ref {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}e{}", self.generation, self.element)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RefError {
    #[error("{0:?} is not a ref; refs look like s3e12 and come from snapshot")]
    Malformed(String),
    #[error("no snapshot has been taken of this machine in this session; call snapshot first")]
    NoSnapshot,
    #[error(
        "{stale} is from an older snapshot (the latest is {latest}); use a ref from the latest snapshot"
    )]
    Stale { stale: Ref, latest: Generation },
    #[error("{0} is not in the latest snapshot")]
    Unknown(Ref),
}

impl FromStr for Ref {
    type Err = RefError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let malformed = || RefError::Malformed(s.to_owned());
        let (generation, element) = s
            .strip_prefix('s')
            .and_then(|rest| rest.split_once('e'))
            .ok_or_else(malformed)?;
        let number = |digits: &str| -> Result<u32, RefError> {
            // Digits only: no signs, no spaces, no leading zeros.
            if digits.is_empty()
                || digits.starts_with('0')
                || !digits.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(malformed());
            }
            digits.parse().map_err(|_| malformed())
        };
        Ok(Self {
            generation: Generation(number(generation)?),
            element: number(element)?,
        })
    }
}

/// What a ref points at, and enough to describe it back to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefTarget {
    pub element: ElementTarget,
    pub role: Role,
    pub name: Option<String>,
}

impl fmt::Display for RefTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.name {
            Some(name) => write!(f, "{} {name:?}", self.role),
            None => write!(f, "{}", self.role),
        }
    }
}

/// The refs of one shown snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefTable {
    generation: Generation,
    targets: Vec<RefTarget>,
}

impl RefTable {
    #[must_use]
    pub const fn new(generation: Generation) -> Self {
        Self {
            generation,
            targets: Vec::new(),
        }
    }

    /// Adds a target and returns its ref.
    pub fn issue(&mut self, target: RefTarget) -> Ref {
        self.targets.push(target);
        Ref {
            generation: self.generation,
            element: u32::try_from(self.targets.len()).unwrap_or(u32::MAX),
        }
    }

    #[must_use]
    pub const fn generation(&self) -> Generation {
        self.generation
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.targets.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    /// The table's refs as the nodes of `snapshot` they name: the snapshot
    /// every target of a table built from it points into.
    #[must_use]
    pub fn named(&self, snapshot: SnapshotId) -> SnapshotRefs {
        SnapshotRefs {
            snapshot,
            generation: self.generation,
            nodes: self
                .targets
                .iter()
                .map(|target| target.element.node)
                .collect(),
        }
    }

    /// # Errors
    /// [`RefError::Stale`] for an older generation, [`RefError::Unknown`] otherwise.
    pub fn resolve(&self, r: Ref) -> Result<&RefTarget, RefError> {
        if r.generation < self.generation {
            return Err(RefError::Stale {
                stale: r,
                latest: self.generation,
            });
        }
        if r.generation > self.generation {
            return Err(RefError::Unknown(r));
        }
        let slot = usize::try_from(r.element)
            .ok()
            .and_then(|n| n.checked_sub(1))
            .ok_or(RefError::Unknown(r))?;
        self.targets.get(slot).ok_or(RefError::Unknown(r))
    }
}

/// What a session named the elements of one snapshot: element `n` (from 1)
/// of `generation` is node `nodes[n - 1]` of `snapshot`. Nodes left out of
/// what the model was shown have no ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRefs {
    snapshot: SnapshotId,
    generation: Generation,
    nodes: Vec<NodeIndex>,
}

impl SnapshotRefs {
    #[must_use]
    pub const fn new(snapshot: SnapshotId, generation: Generation, nodes: Vec<NodeIndex>) -> Self {
        Self {
            snapshot,
            generation,
            nodes,
        }
    }

    #[must_use]
    pub const fn snapshot(&self) -> SnapshotId {
        self.snapshot
    }

    #[must_use]
    pub const fn generation(&self) -> Generation {
        self.generation
    }

    /// The node each element names, in the elements' order.
    #[must_use]
    pub fn nodes(&self) -> &[NodeIndex] {
        &self.nodes
    }

    /// Each named node with its ref.
    pub fn refs(&self) -> impl Iterator<Item = (NodeIndex, Ref)> + '_ {
        self.nodes.iter().zip(1_u32..).map(|(&node, element)| {
            (
                node,
                Ref {
                    generation: self.generation,
                    element,
                },
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ax::tree::{NodeIndex, SnapshotId};

    fn target(node: u32) -> RefTarget {
        RefTarget {
            element: ElementTarget {
                snapshot: SnapshotId::new(77),
                node: NodeIndex::new(node),
            },
            role: Role::Button,
            name: Some("OK".into()),
        }
    }

    #[test]
    fn a_table_names_the_nodes_its_refs_point_at() {
        let mut table = RefTable::new(Generation::new(3).unwrap());
        table.issue(target(4));
        table.issue(target(9));
        let named = table.named(SnapshotId::new(77));
        assert_eq!(named.snapshot(), SnapshotId::new(77));
        let refs: Vec<_> = named
            .refs()
            .map(|(n, r)| (n.get(), r.to_string()))
            .collect();
        assert_eq!(refs, [(4, "s3e1".to_owned()), (9, "s3e2".to_owned())]);
        assert_eq!(Generation::new(0), None);
    }

    #[test]
    fn refs_round_trip_and_reject_lookalikes() {
        let r: Ref = "s3e12".parse().unwrap();
        assert_eq!(r.to_string(), "s3e12");
        for bad in [
            "",
            "e12",
            "s3",
            "s3e",
            "se12",
            "s0e1",
            "s3e0",
            "s03e1",
            "s3e+1",
            "s3e1 ",
            "S3E1",
            "s3e1e2",
            "s99999999999e1",
        ] {
            assert!(
                matches!(bad.parse::<Ref>(), Err(RefError::Malformed(_))),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn only_the_latest_generation_resolves() {
        let mut older = RefTable::new(Generation::FIRST);
        let old = older.issue(target(1));
        let mut table = RefTable::new(Generation::FIRST.next());
        let first = table.issue(target(4));
        let second = table.issue(target(9));
        assert_eq!(second.to_string(), "s2e2");
        assert_eq!(
            table.resolve(first).unwrap().element.node,
            NodeIndex::new(4)
        );
        assert_eq!(
            table.resolve(second).unwrap().element.node,
            NodeIndex::new(9)
        );
        assert_eq!(
            table.resolve(old),
            Err(RefError::Stale {
                stale: old,
                latest: Generation(2)
            })
        );
        // Same generation, element past the end: unknown, not stale.
        let past: Ref = "s2e3".parse().unwrap();
        assert_eq!(table.resolve(past), Err(RefError::Unknown(past)));
        // A future generation was never issued.
        let future: Ref = "s5e1".parse().unwrap();
        assert_eq!(table.resolve(future), Err(RefError::Unknown(future)));
    }
}
