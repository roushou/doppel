//! The scene over a machine's screen at an instant: each agent's pointer,
//! call under way and marks, the marks agents put up, and the strip's lanes.

use std::sync::Arc;

use doppel_desktop::geometry::{Point, Rect};

use super::seen::changes;
use super::{
    Call, Callout, CalloutLasts, Gesture, MARK_FADE_MS, MARK_MS, MIN_UNDER_WAY_MS, Moment,
    SETTLE_MS, Timeline, sweep_ends,
};

/// A mark a call left on the screen, showing from `since` until `until`:
/// a frame over an element or a path just after its call ends, or when its
/// agent's next call starts; a ripple for [`MARK_MS`].
#[derive(Debug, Clone, PartialEq)]
pub enum Mark {
    Ripple {
        at: Point,
        gesture: Gesture,
        since: f64,
        until: f64,
    },
    Box {
        frame: Rect,
        label: String,
        since: f64,
        until: f64,
    },
    Path {
        from: Point,
        to: Point,
        since: f64,
        until: f64,
    },
    Failed {
        at: Point,
        reason: String,
        since: f64,
        until: f64,
    },
}

impl Mark {
    /// When it showed.
    pub const fn since(&self) -> f64 {
        match self {
            Self::Ripple { since, .. }
            | Self::Box { since, .. }
            | Self::Path { since, .. }
            | Self::Failed { since, .. } => *since,
        }
    }

    /// When it goes.
    pub const fn until(&self) -> f64 {
        match self {
            Self::Ripple { until, .. }
            | Self::Box { until, .. }
            | Self::Path { until, .. }
            | Self::Failed { until, .. } => *until,
        }
    }

    /// How opaque it is at `at`: whole, then fading over [`MARK_FADE_MS`]
    /// to its end.
    pub fn opacity(&self, at: f64) -> f64 {
        ((self.until() - at) / MARK_FADE_MS).clamp(0.0, 1.0)
    }
}

/// When the mark of `moments[index]` goes: [`SETTLE_MS`] after its call
/// ends, at once if its agent's next call starts sooner, and after
/// [`MARK_MS`] at the latest.
fn mark_until(moments: &[Moment], index: usize) -> f64 {
    let Some(moment) = moments.get(index) else {
        return f64::NEG_INFINITY;
    };
    let session = moment.call.key.session;
    let next = moments
        .iter()
        .skip(index + 1)
        .find(|later| !later.person && later.call.key.session == session)
        .map_or(f64::INFINITY, |later| later.start_ms);
    let settled = moment
        .end_ms
        .map_or(f64::INFINITY, |end| end.max(moment.start_ms) + SETTLE_MS);
    settled.min(next).min(moment.start_ms + MARK_MS)
}

/// One machine's scene at an instant: each agent's, and the marks agents
/// put on the screen.
#[derive(Debug, Clone, PartialEq)]
pub struct Scene {
    /// Each session that has acted on the machine, by session id.
    pub agents: Vec<AgentScene>,
    /// The marks agents put on the screen that are showing.
    pub callouts: Vec<Shown>,
}

/// One session's part of a machine's scene: its pointer, its call under
/// way, and the marks its calls left.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentScene {
    pub session: u64,
    /// Its client, in its own words.
    pub client: String,
    /// Where its pointer is: the last place it acted.
    pub cursor: Option<Point>,
    /// Its call under way, and since when.
    pub current: Option<(Call, f64)>,
    /// When the look whose band is sweeping the screen began, if one is.
    pub scan: Option<f64>,
    pub marks: Vec<Mark>,
    /// When it last did anything: its pointer fades after a while.
    pub active: f64,
}

impl Scene {
    /// The session's part of the scene, if it has acted.
    pub fn of(&self, session: u64) -> Option<&AgentScene> {
        self.agents.iter().find(|agent| agent.session == session)
    }
}

/// A mark an agent put on the screen, showing from `since` until `until`.
#[derive(Debug, Clone, PartialEq)]
pub struct Shown {
    pub callout: Arc<Callout>,
    pub since: f64,
    pub until: f64,
}

/// How long an agent's mark takes to fade in and out.
pub const CALLOUT_FADE_MS: f64 = 250.0;

impl Scene {
    /// The scene at `at`: the pointer where the latest call that had a place
    /// acted, the call under way (the latest one started), and the marks of
    /// calls that started or failed less than [`MARK_MS`] before. `moments`
    /// are in start order, as a [`Timeline`] keeps them.
    pub(crate) fn at(moments: &[Moment], at: f64) -> Self {
        let mut agents: Vec<AgentScene> = Vec::new();
        for (at_index, moment) in moments
            .iter()
            .enumerate()
            .filter(|(_, moment)| !moment.person && moment.start_ms <= at)
        {
            let session = moment.call.key.session;
            let index = match agents.iter().position(|agent| agent.session == session) {
                Some(index) => index,
                None => {
                    agents.push(AgentScene {
                        session,
                        client: moment.call.client.clone(),
                        cursor: None,
                        current: None,
                        scan: None,
                        marks: Vec::new(),
                        active: f64::NEG_INFINITY,
                    });
                    agents.len() - 1
                }
            };
            let Some(agent) = agents.get_mut(index) else {
                continue;
            };
            if let Some(placed) = moment.call.target.aim() {
                agent.cursor = Some(placed);
            }
            agent.active = agent.active.max(moment.start_ms);
            let shown_until = moment.end_ms.map_or(f64::INFINITY, |end| {
                end.max(moment.start_ms + MIN_UNDER_WAY_MS)
            });
            if at < shown_until {
                agent.current = Some((moment.call.clone(), moment.start_ms));
            }
            if moment.call.gesture == Gesture::Look
                && at < sweep_ends(moment.start_ms, moment.end_ms)
            {
                agent.scan = Some(moment.start_ms);
            }
            if let Some(mark) = moment
                .call
                .leaves(moment.start_ms, mark_until(moments, at_index))
                .filter(|mark| at < mark.until())
            {
                agent.marks.push(mark);
            }
            if let Some(end) = moment.end_ms {
                agent.active = agent.active.max(end.min(at));
                if let (Some(reason), Some(cursor)) = (&moment.failed, agent.cursor)
                    && end <= at
                    && at - end < MARK_MS
                {
                    agent.marks.push(Mark::Failed {
                        at: cursor,
                        reason: reason.clone(),
                        since: end,
                        until: end + MARK_MS,
                    });
                }
            }
        }
        agents.sort_by_key(|agent| agent.session);
        Self {
            agents,
            callouts: callouts_at(moments, at),
        }
    }
}

/// One row of a timeline strip: a session's calls, with who made them.
#[derive(Debug, Clone, PartialEq)]
pub struct Lane<'a> {
    pub session: u64,
    pub client: &'a str,
    pub moments: Vec<&'a Moment>,
}

/// The calls in a strip, a lane per session, by session id. A person's
/// stretches of control stay in the lane of the session they took over.
pub fn lanes<'a>(moments: impl IntoIterator<Item = &'a Moment>) -> Vec<Lane<'a>> {
    let mut lanes: Vec<Lane<'a>> = Vec::new();
    for moment in moments {
        let session = moment.call.key.session;
        match lanes.iter_mut().find(|lane| lane.session == session) {
            Some(lane) => {
                if lane.client.is_empty() {
                    lane.client = &moment.call.client;
                }
                lane.moments.push(moment);
            }
            None => lanes.push(Lane {
                session,
                client: &moment.call.client,
                moments: vec![moment],
            }),
        }
    }
    lanes.sort_by_key(|lane| lane.session);
    lanes
}

/// When a mark call's mark goes: after its time, or, for one that lasts
/// until the agent acts, when the next call that may change the screen
/// starts. `None` for a call that shows no mark (or failed).
fn callout_until(moments: &[Moment], index: usize) -> Option<(&Arc<Callout>, f64, f64)> {
    let moment = moments.get(index)?;
    let callout = moment.call.callout.as_ref()?;
    if moment.person || moment.failed.is_some() {
        return None;
    }
    let timed = moment.start_ms + callout.lasts.at_most();
    let until = match callout.lasts {
        CalloutLasts::For(_) => timed,
        CalloutLasts::UntilActed(_) => moments
            .iter()
            .skip(index + 1)
            .filter(|later| later.start_ms >= moment.start_ms && changes(later))
            .map(|later| later.start_ms)
            .fold(timed, f64::min),
    };
    Some((callout, moment.start_ms, until))
}

/// The agents' marks showing at `at`, oldest first.
pub(crate) fn callouts_at(moments: &[Moment], at: f64) -> Vec<Shown> {
    (0..moments.len())
        .filter_map(|index| {
            let (callout, since, until) = callout_until(moments, index)?;
            (since <= at && at < until).then(|| Shown {
                callout: callout.clone(),
                since,
                until,
            })
        })
        .collect()
}

impl Timeline {
    /// The scene at `at`; see [`Scene`].
    #[must_use]
    pub fn scene_at(&self, at: f64) -> Scene {
        Scene::at(&self.moments, at)
    }
}

impl Timeline {
    /// Whether anything on it still moves at `now`: a call under way, a
    /// mark fading, or an agent's mark showing.
    #[must_use]
    pub fn moving(&self, now: f64) -> bool {
        self.moments.iter().any(|moment| {
            moment.end_ms.is_none()
                || now - moment.end_ms.unwrap_or(now).max(moment.start_ms) < MARK_MS
        }) || !callouts_at(&self.moments, now).is_empty()
    }

    /// The latest time anything happened on it.
    #[must_use]
    pub fn latest(&self, now: f64) -> f64 {
        self.moments
            .iter()
            .map(|moment| moment.end_ms.unwrap_or(now).max(moment.start_ms))
            .fold(f64::NEG_INFINITY, f64::max)
    }
}
