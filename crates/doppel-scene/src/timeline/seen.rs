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
        Some(SeenAt {
            seen: seen.clone(),
            refs: refs.clone(),
            since_ms,
            acted_since: acted_between(moments, Some(taken), started, at),
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

/// Which of a panel's asks an answer is for. Each ask takes the next one,
/// so an earlier ask's answer never lands, even one asked in the same
/// millisecond.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ticket(u64);

impl Ticket {
    /// The ticket after this one.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// A person's own ask about a machine's screen (a look at its elements, a
/// read of its text), asked at `at_ms`, and its answer once it comes.
#[derive(Debug, Clone, PartialEq)]
pub struct Asked<T> {
    pub at_ms: f64,
    ticket: Ticket,
    pub outcome: Answer<T>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Answer<T> {
    Loading,
    Got(T),
    Failed(String),
}

impl<T> Asked<T> {
    /// An ask made at `at_ms` with `ticket`, its answer to come.
    #[must_use]
    pub const fn new(ticket: Ticket, at_ms: f64) -> Self {
        Self {
            at_ms,
            ticket,
            outcome: Answer::Loading,
        }
    }

    /// Takes the answer to the ask with `ticket`: `false`, changing
    /// nothing, when it is another ask's.
    pub fn answer(&mut self, ticket: Ticket, outcome: Result<T, String>) -> bool {
        if self.ticket != ticket {
            return false;
        }
        self.outcome = match outcome {
            Ok(value) => Answer::Got(value),
            Err(why) => Answer::Failed(why),
        };
        true
    }
}

/// A person's own look at a machine's elements.
pub type Look = Asked<Arc<AxSnapshot>>;

/// A person's own read of a machine's screen text.
pub type TextLook = Asked<Arc<TextRead>>;

/// What a layer (the elements panel, the text layer) shows at an instant: the newer of what the agent had (`A`)
/// and the person's own ask.
#[derive(Debug, Clone, PartialEq)]
pub enum Layer<'a, A, T> {
    Agent(A),
    Person {
        asked: &'a Asked<T>,
        /// A call may have changed the screen since the ask.
        acted_since: bool,
    },
    Nothing,
}

/// What the elements panel shows.
pub type Showing<'a> = Layer<'a, SeenAt, Arc<AxSnapshot>>;

/// What the text layer shows.
pub type Reading<'a> = Layer<'a, ReadAt, Arc<TextRead>>;

impl Timeline {
    /// What the text layer shows at `at`, given the person's own read.
    #[must_use]
    pub fn reading<'a>(&self, look: Option<&'a TextLook>, at: f64) -> Reading<'a> {
        self.shown(self.read_at(at), |agent| agent.since_ms, look, at)
    }

    /// What the elements panel shows at `at`, given the person's own look.
    #[must_use]
    pub fn showing<'a>(&self, look: Option<&'a Look>, at: f64) -> Showing<'a> {
        self.shown(self.seen_at(at), |agent| agent.since_ms, look, at)
    }

    /// The newer of the agent's latest (taken at `since_ms`) and the
    /// person's own ask, at `at`.
    fn shown<'a, A, T>(
        &self,
        agent: Option<A>,
        since_ms: impl Fn(&A) -> f64,
        asked: Option<&'a Asked<T>>,
        at: f64,
    ) -> Layer<'a, A, T> {
        match (agent, asked) {
            (Some(agent), Some(asked)) if since_ms(&agent) > asked.at_ms => Layer::Agent(agent),
            (Some(_) | None, Some(asked)) => Layer::Person {
                asked,
                acted_since: acted_between(&self.moments, None, asked.at_ms, at),
            },
            (Some(agent), None) => Layer::Agent(agent),
            (None, None) => Layer::Nothing,
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
