//! What an agent takes in from a snapshot, and what it then goes for: the
//! elements its model was given refs for, taken in top to bottom as the
//! snapshot lands, and, once its next call aims at one of them, that one
//! alone. Pure: the timeline in; what is taken in at an instant out.

use doppel_machine::ax::refs::Ref;
use doppel_machine::ax::tree::AxSnapshot;
use doppel_machine::geometry::{Rect, Size};

use crate::elements::{self, Kind, Named};
use crate::timeline::{Moment, Seen, Target};

/// At most this many elements are taken in: past that the eye can't follow.
pub const MOST_GLANCES: usize = 60;
/// Taking in a whole screen, top to bottom, takes this long.
pub const SWEEP_MS: f64 = 650.0;
/// Closing in on the element the agent goes for takes this long.
pub const LOCK_MS: f64 = 380.0;
/// What was taken in fades this long once the agent moves on.
pub const LET_GO_MS: f64 = 300.0;
/// A target a point stands for: this wide, around it.
const POINT_TARGET: f64 = 28.0;

/// One element taken in: where it is on screen, its ref, and when.
#[derive(Debug, Clone, PartialEq)]
pub struct Glance {
    pub frame: Rect,
    pub reference: Ref,
    pub at: f64,
}

/// What the agent goes for after taking a snapshot in: where, from when
/// (its call's start), and until when (its call's end).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lock {
    pub frame: Rect,
    pub since: f64,
    pub until: Option<f64>,
}

/// What one agent took in from its latest snapshot by an instant.
#[derive(Debug, Clone, PartialEq)]
pub struct Attention {
    pub session: u64,
    /// When the snapshot landed.
    pub since: f64,
    /// Every element taken in, in the order it was, lit or still to be.
    pub glances: Vec<Glance>,
    /// When the agent moved on: its next call's start.
    pub moved_on: Option<f64>,
    /// What that next call aimed at, if it aimed at a place.
    pub lock: Option<Lock>,
}

/// Elements worth taking in: what has content or can be acted on.
fn worth(kind: Kind, named: bool) -> bool {
    match kind {
        Kind::Control | Kind::Field | Kind::Text | Kind::Menu => true,
        Kind::Other => named,
        Kind::Window | Kind::Container => false,
    }
}

/// The glances of `snapshot`, its elements named by `named`, taken in from
/// `since` on a display `display`: top to bottom over [`SWEEP_MS`], the
/// topmost as it lands.
fn glances(snapshot: &AxSnapshot, named: &Named, display: Size, since: f64) -> Vec<Glance> {
    let mut picked: Vec<(Rect, Ref)> = snapshot
        .tree
        .iter()
        .filter_map(|(index, node)| {
            let reference = named.get(index)?;
            let kind = Kind::of(&node.role);
            if elements::wrapper(node) || !worth(kind, !elements::label(node).is_empty()) {
                return None;
            }
            let frame = elements::seen(snapshot, index)?;
            (frame.width() >= 3.0 && frame.height() >= 3.0).then_some((frame, reference))
        })
        .take(MOST_GLANCES)
        .collect();
    picked.sort_by(|a, b| a.0.top().total_cmp(&b.0.top()));
    // The sweep runs over what was taken in, not the whole display: the
    // first element lights as the snapshot lands, the last at its end.
    let (first, last) = match (picked.first(), picked.last()) {
        (Some(first), Some(last)) => (first.0.top(), last.0.top()),
        _ => return Vec::new(),
    };
    let span = (last - first).max(display.height() / 8.0).max(1.0);
    picked
        .into_iter()
        .map(|(frame, reference)| Glance {
            frame,
            reference,
            at: since + SWEEP_MS * ((frame.top() - first) / span).clamp(0.0, 1.0),
        })
        .collect()
}

/// Where a call aimed: the element it acted on, or a small square around
/// the point.
pub(crate) fn aimed(target: &Target) -> Option<Rect> {
    let around = |x: f64, y: f64| {
        Rect::new(
            x - POINT_TARGET / 2.0,
            y - POINT_TARGET / 2.0,
            POINT_TARGET,
            POINT_TARGET,
        )
        .ok()
    };
    match target {
        Target::Element { frame, .. } => Some(*frame),
        Target::Point(at) => around(at.x(), at.y()),
        Target::Drag { from, .. } => around(from.x(), from.y()),
        Target::Nowhere => None,
    }
}

impl Attention {
    /// What each agent on `moments`' display `display` takes in at `at`:
    /// from its latest snapshot that has landed and that its model was
    /// given refs for. Snapshots still to be fetched (recorded ones) take
    /// nothing in.
    #[must_use]
    pub fn at(moments: &[Moment], display: Size, at: f64) -> Vec<Self> {
        let mut found: Vec<Self> = Vec::new();
        for (index, moment) in moments.iter().enumerate() {
            let Some(end) = moment.end_ms.filter(|end| *end <= at && !moment.person) else {
                continue;
            };
            let Some(Seen::Live(snapshot)) = &moment.seen else {
                continue;
            };
            let named = Named::of(snapshot, moment.refs.as_deref());
            if !named.known() {
                continue;
            }
            let session = moment.call.key.session;
            let next = moments
                .iter()
                .skip(index + 1)
                .find(|later| !later.person && later.call.key.session == session);
            let lock = next.and_then(|next| {
                aimed(&next.call.target).map(|frame| Lock {
                    frame,
                    since: next.start_ms,
                    until: next.end_ms,
                })
            });
            let attention = Self {
                session,
                since: end,
                glances: glances(snapshot, &named, display, end),
                moved_on: next.map(|next| next.start_ms),
                lock,
            };
            match found.iter_mut().find(|known| known.session == session) {
                Some(known) => *known = attention,
                None => found.push(attention),
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use doppel_machine::agent::{ProcessId, RunningApp};
    use doppel_machine::ax::refs::{Generation, SnapshotRefs};
    use doppel_machine::ax::tree::{
        AxNode, AxText, AxTree, NodeIndex, Role, SnapshotId, States, Truncation,
    };

    use super::*;
    use crate::timeline::{Call, CallKey, Gesture};

    fn node(role: &str, title: &str, frame: (f64, f64, f64, f64)) -> AxNode {
        AxNode {
            role: Role::parse(role),
            subrole: None,
            title: AxText::new(title),
            description: None,
            identifier: None,
            value: None,
            frame: Some(Rect::new(frame.0, frame.1, frame.2, frame.3).unwrap()),
            states: States::default(),
            actions: Vec::new(),
        }
    }

    fn display() -> Size {
        Size::new(1000.0, 800.0).unwrap()
    }

    fn snapshot() -> AxSnapshot {
        let up = Some(NodeIndex::new(0));
        AxSnapshot {
            id: SnapshotId::new(7),
            app: RunningApp {
                pid: ProcessId::new(412),
                bundle_id: None,
                name: "Chrome".into(),
            },
            tree: AxTree::new(vec![
                (None, node("AXWindow", "Lumen", (0.0, 0.0, 1000.0, 800.0))),
                (up, node("AXButton", "Save", (100.0, 600.0, 80.0, 30.0))),
                (
                    up,
                    node("AXTextField", "Email", (100.0, 100.0, 300.0, 30.0)),
                ),
                (up, node("AXButton", "Hidden", (100.0, 300.0, 80.0, 30.0))),
            ])
            .unwrap(),
            truncation: Truncation::Complete,
        }
    }

    fn moment(request: u64, start_ms: f64, end_ms: Option<f64>, target: Target) -> Moment {
        Moment {
            start_ms,
            end_ms,
            call: Call {
                key: CallKey {
                    session: 1,
                    request,
                },
                machine: "dev".parse().unwrap(),
                client: "claude".into(),
                caption: "a call".into(),
                gesture: Gesture::Act,
                target,
                callout: None,
                why: None,
            },
            failed: None,
            person: false,
            seen: None,
            refs: None,
            read: None,
        }
    }

    fn look(end_ms: f64, named: Vec<u32>) -> Moment {
        let mut look = moment(1, end_ms - 30.0, Some(end_ms), Target::Nowhere);
        let snapshot = snapshot();
        look.refs = Some(Arc::new(SnapshotRefs::new(
            snapshot.id,
            Generation::FIRST,
            named.into_iter().map(NodeIndex::new).collect(),
        )));
        look.seen = Some(Seen::Live(Arc::new(snapshot)));
        look
    }

    #[test]
    fn takes_in_what_its_model_was_given_top_to_bottom() {
        // The window frames the rest; "Hidden" wasn't given a ref.
        let moments = [look(1000.0, vec![0, 1, 2])];
        let [attention] = Attention::at(&moments, display(), 1100.0)
            .try_into()
            .unwrap();
        let tops: Vec<f64> = attention.glances.iter().map(|g| g.frame.top()).collect();
        assert_eq!(tops, [100.0, 600.0]);
        assert_eq!(attention.glances[0].at, 1000.0, "the first as it lands");
        assert_eq!(
            attention.glances[1].at,
            1000.0 + SWEEP_MS,
            "the last as it ends"
        );
        assert_eq!(attention.glances[0].reference.to_string(), "s1e3");
        assert_eq!(attention.lock, None);
        assert!(
            Attention::at(&moments, display(), 999.0).is_empty(),
            "not landed yet"
        );
    }

    #[test]
    fn goes_for_what_the_next_call_aims_at() {
        let save = Rect::new(100.0, 600.0, 80.0, 30.0).unwrap();
        let moments = [
            look(1000.0, vec![1, 2]),
            moment(
                2,
                2500.0,
                Some(2530.0),
                Target::Element {
                    frame: save,
                    label: "Save".into(),
                },
            ),
        ];
        let [attention] = Attention::at(&moments, display(), 2000.0)
            .try_into()
            .unwrap();
        assert_eq!(attention.moved_on, Some(2500.0));
        assert_eq!(
            attention.lock,
            Some(Lock {
                frame: save,
                since: 2500.0,
                until: Some(2530.0)
            })
        );
    }

    #[test]
    fn a_snapshot_its_model_named_nothing_of_takes_nothing_in() {
        let mut unnamed = look(1000.0, vec![1]);
        unnamed.refs = None;
        assert!(Attention::at(&[unnamed], display(), 1100.0).is_empty());
    }
}
