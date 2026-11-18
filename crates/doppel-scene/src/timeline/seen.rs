//! What a call saw or read at an instant, or a person's own look or
//! read, and whether the screen may have changed since.

use std::sync::Arc;

use doppel_desktop::ax::refs::SnapshotRefs;
use doppel_desktop::ax::tree::AxSnapshot;

use super::{Gesture, Moment, Seen, TextRead, Timeline};

/// The snapshot shown at an instant, which call took it, and whether the
/// machine may have changed since.
#[derive(Debug, Clone, PartialEq)]
pub struct SeenAt {
    pub seen: Seen,
    /// What the agent named its elements.
    pub refs: Option<Arc<SnapshotRefs>>,
    /// When it was taken: its call's end.
    pub since_ms: f64,
    /// A later call (or a person) may have changed the screen before `at`.
    pub acted_since: bool,
}

impl Timeline {
    /// The latest snapshot a call had seen by `at`.
    #[must_use]
    pub fn seen_at(&self, at: f64) -> Option<SeenAt> {
        let moments = &self.moments;
        let (taken, seen, since_ms, refs) = moments
            .iter()
            .enumerate()
            .filter_map(|(index, moment)| {
                let end = moment.end_ms.filter(|end| *end <= at)?;
                Some((index, moment.seen.as_ref()?, end, &moment.refs))
            })
            .max_by(|a, b| a.2.total_cmp(&b.2))?;
        let started = moments
            .get(taken)
            .map_or(f64::INFINITY, |moment| moment.start_ms);
        let acted_since = moments.iter().enumerate().any(|(index, moment)| {
            index != taken && moment.start_ms >= started && moment.start_ms <= at && changes(moment)
        });
        Some(SeenAt {
            seen: seen.clone(),
            refs: refs.clone(),
            since_ms,
            acted_since,
        })
    }
}

/// The latest text a call had read by `at`, when it was read, and whether
/// the screen may have changed since.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadAt {
    pub read: Arc<TextRead>,
    pub since_ms: f64,
    pub acted_since: bool,
}

impl Timeline {
    /// The latest text a call had read by `at`.
    #[must_use]
    pub fn read_at(&self, at: f64) -> Option<ReadAt> {
        let moments = &self.moments;
        let (taken, read, since_ms) = moments
            .iter()
            .enumerate()
            .filter_map(|(index, moment)| {
                let end = moment.end_ms.filter(|end| *end <= at)?;
                Some((index, moment.read.as_ref()?, end))
            })
            .max_by(|a, b| a.2.total_cmp(&b.2))?;
        let started = moments
            .get(taken)
            .map_or(f64::INFINITY, |moment| moment.start_ms);
        Some(ReadAt {
            read: read.clone(),
            since_ms,
            acted_since: acted_between(moments, Some(taken), started, at),
        })
    }
}

/// Whether a call other than `skip` that may change the screen started
/// between `from` and `to`.
fn acted_between(moments: &[Moment], skip: Option<usize>, from: f64, to: f64) -> bool {
    moments.iter().enumerate().any(|(index, moment)| {
        Some(index) != skip && moment.start_ms >= from && moment.start_ms <= to && changes(moment)
    })
}

/// A person's own read of a machine's screen text, asked at `at_ms`.
#[derive(Debug, Clone, PartialEq)]
pub struct TextLook {
    pub at_ms: f64,
    pub outcome: TextLooked,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TextLooked {
    Loading,
    Read(Arc<TextRead>),
    Failed(String),
}

/// What the text layer shows at `at`: the newer of the agent's latest read
/// and the person's own.
#[derive(Debug, Clone, PartialEq)]
pub enum Reading<'a> {
    Agent(ReadAt),
    Person {
        look: &'a TextLook,
        acted_since: bool,
    },
    Nothing,
}

impl Timeline {
    /// What the text layer shows at `at`, given the person's own read.
    #[must_use]
    pub fn reading<'a>(&self, look: Option<&'a TextLook>, at: f64) -> Reading<'a> {
        match (self.read_at(at), look) {
            (Some(agent), Some(look)) if agent.since_ms > look.at_ms => Reading::Agent(agent),
            (Some(_) | None, Some(look)) => Reading::Person {
                look,
                acted_since: acted_between(&self.moments, None, look.at_ms, at),
            },
            (Some(agent), None) => Reading::Agent(agent),
            (None, None) => Reading::Nothing,
        }
    }
}

/// A person's own look at a machine's elements, asked at `at_ms`.
#[derive(Debug, Clone, PartialEq)]
pub struct Look {
    pub at_ms: f64,
    pub outcome: Looked,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Looked {
    Loading,
    Seen(Arc<AxSnapshot>),
    Failed(String),
}

/// What the elements panel shows at `at`: the newer of the agent's latest
/// snapshot and the person's own look.
#[derive(Debug, Clone, PartialEq)]
pub enum Showing<'a> {
    Agent(SeenAt),
    Person {
        look: &'a Look,
        /// A call may have changed the screen since the look.
        acted_since: bool,
    },
    Nothing,
}

impl Timeline {
    /// What the elements panel shows at `at`, given the person's own look.
    #[must_use]
    pub fn showing<'a>(&self, look: Option<&'a Look>, at: f64) -> Showing<'a> {
        match (self.seen_at(at), look) {
            (Some(agent), Some(look)) if agent.since_ms > look.at_ms => Showing::Agent(agent),
            (Some(_) | None, Some(look)) => Showing::Person {
                look,
                acted_since: self.moments.iter().any(|moment| {
                    moment.start_ms >= look.at_ms && moment.start_ms <= at && changes(moment)
                }),
            },
            (Some(agent), None) => Showing::Agent(agent),
            (None, None) => Showing::Nothing,
        }
    }
}

/// Whether a call may change what is on the screen.
pub(super) fn changes(moment: &Moment) -> bool {
    if moment.person {
        return true;
    }
    match moment.call.gesture {
        Gesture::Click
        | Gesture::DoubleClick
        | Gesture::SecondaryClick
        | Gesture::Move
        | Gesture::Drag
        | Gesture::Scroll
        | Gesture::Key
        | Gesture::Type
        | Gesture::Act
        | Gesture::Run
        | Gesture::Lifecycle
        | Gesture::Rollback => true,
        Gesture::Look | Gesture::Wait | Gesture::Mark | Gesture::Checkpoint | Gesture::Other => {
            false
        }
    }
}
