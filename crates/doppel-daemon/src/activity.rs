//! What agents do to the machines, as they do it, for consoles to draw over
//! the screen. Pure where it can be: what a call does and where on the
//! display ([`started`]) comes from the request and the elements recent
//! snapshots saw; how it ended ([`ended`]) from its outcome. [`Watchers`]
//! passes each [`daemon::Activity`] on to every console watching.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use doppel_desktop::ax::tree::Subrole;
use doppel_desktop::mark::{Lasts, Mark, MarkAt, Shape};
use doppel_desktop::screen_text::{ScreenText, TextQuery};
use doppel_machine::machine::MachineName;
use doppel_protocol::convert::Millis;
use doppel_protocol::pb::agent::v1 as agent;
use doppel_protocol::pb::common::v1 as common;
use doppel_protocol::pb::daemon::v1 as daemon;
use doppel_protocol::pb::daemon::v1::Gesture;
use doppel_protocol::pb::daemon::v1::call_started::Target;
use doppel_protocol::pb::daemon::v1::daemon_request::Operation;
use doppel_protocol::pb::vmm::v1 as vmm;

use crate::call_events::{CallEvent, SessionCall};
use crate::call_events::{Guest, Traced};

/// How many snapshots of each machine are kept for their elements: an
/// agent acts on the latest, or one just before it.
const SNAPSHOTS_KEPT: usize = 4;

/// How many events a console may fall behind before it misses some: the
/// overlay only decorates the screen, so a slow console loses events rather
/// than holding up the calls.
const BACKLOG: usize = 256;

/// The elements recent snapshots saw on each machine, by snapshot and
/// index, so a call acting on one can say where it is.
#[derive(Debug, Default)]
pub(crate) struct Elements(Mutex<HashMap<MachineName, VecDeque<Seen>>>);

#[derive(Debug)]
struct Seen {
    snapshot: u64,
    nodes: Vec<Option<daemon::ElementTarget>>,
}

impl Elements {
    fn machines(&self) -> MutexGuard<'_, HashMap<MachineName, VecDeque<Seen>>> {
        // Plain data, valid after any panic that poisoned the lock.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keeps where `snapshot`'s elements are on `machine`.
    pub(crate) fn saw(&self, machine: &MachineName, snapshot: &agent::AxSnapshot) {
        let nodes = snapshot.nodes.iter().map(element).collect();
        let mut machines = self.machines();
        let kept = machines.entry(machine.clone()).or_default();
        kept.retain(|seen| seen.snapshot != snapshot.snapshot_id);
        kept.push_back(Seen {
            snapshot: snapshot.snapshot_id,
            nodes,
        });
        while kept.len() > SNAPSHOTS_KEPT {
            kept.pop_front();
        }
    }

    /// Where element `index` of `snapshot` was on `machine`, if a kept
    /// snapshot saw it with a frame.
    pub(crate) fn find(
        &self,
        machine: &MachineName,
        snapshot: u64,
        index: u32,
    ) -> Option<daemon::ElementTarget> {
        let machines = self.machines();
        let seen = machines
            .get(machine)?
            .iter()
            .find(|seen| seen.snapshot == snapshot)?;
        seen.nodes.get(usize::try_from(index).ok()?)?.clone()
    }

    /// Forgets `machine`'s snapshots: its guest restarted or went back, so
    /// their elements are gone.
    pub(crate) fn forget(&self, machine: &MachineName) {
        self.machines().remove(machine);
    }

    /// Keeps where the elements an answered call's snapshot saw are, so a
    /// call acting on one can say where; forgets them when the guest may
    /// have restarted or gone back.
    pub(crate) fn on_call(&self, event: &CallEvent<'_>) {
        match event {
            CallEvent::Ended {
                call:
                    SessionCall {
                        machine: Some(machine),
                        guest,
                        ..
                    },
                answered,
                ..
            } => {
                match guest {
                    Guest::MayRestart => self.forget(machine),
                    Guest::Kept => {}
                }
                if let Some(snapshot) = answered.seen {
                    self.saw(machine, snapshot);
                }
            }
            CallEvent::Ended { .. }
            | CallEvent::Running { .. }
            | CallEvent::TurnBegan { .. }
            | CallEvent::Named { .. } => {}
        }
    }
}

/// A node as a target: its frame and what a person would call it. One with
/// no frame, or an empty one, can't be pointed at.
fn element(node: &agent::AxNode) -> Option<daemon::ElementTarget> {
    let frame = node.frame?;
    let size = frame.size?;
    if !(size.width > 0.0 && size.height > 0.0) {
        return None;
    }
    let label = [&node.title, &node.description]
        .into_iter()
        .find(|text| !text.trim().is_empty())
        .cloned()
        .or_else(|| match &node.value {
            Some(agent::AxValue {
                value: Some(agent::ax_value::Value::Text(text)),
            }) if !text.trim().is_empty() => Some(text.clone()),
            Some(_) | None => None,
        })
        .unwrap_or_else(|| node.role.trim_start_matches("AX").to_owned());
    Some(daemon::ElementTarget {
        frame: Some(frame),
        label,
    })
}

/// What an action on an element does: setting a field's value is typing
/// into it, the rest act on it.
fn acting(action: Option<&agent::PerformAxAction>) -> Gesture {
    use agent::perform_ax_action::Action;
    match action.and_then(|action| action.action.as_ref()) {
        Some(Action::SetValue(_)) => Gesture::Type,
        Some(
            Action::Press(_)
            | Action::Focus(_)
            | Action::Raise(_)
            | Action::ShowMenu(_)
            | Action::Named(_)
            | Action::Select(_),
        )
        | None => Gesture::Act,
    }
}

/// What `operation` does, and where on the display when that's known.
/// `find` looks an element reference up in the snapshots kept.
pub(crate) fn started(
    operation: &Operation,
    find: impl Fn(u64, u32) -> Option<daemon::ElementTarget>,
) -> daemon::CallStarted {
    let (gesture, target) = match operation {
        Operation::SendInput(request) => input(request.input.as_ref()),
        Operation::ActOnElement(request) => (
            acting(request.action.as_ref()),
            request
                .action
                .as_ref()
                .and_then(|action| find(action.snapshot_id, action.node_index))
                .map(Target::Element),
        ),
        Operation::TakeSnapshot(_)
        | Operation::TakeScreenshot(_)
        | Operation::ReadScreenText(_)
        | Operation::GetMachineReport(_)
        | Operation::GetStatus(_) => (Gesture::Look, None),
        Operation::WaitForElement(_) | Operation::WaitForText(_) | Operation::AwaitDesktop(_) => {
            (Gesture::Wait, None)
        }
        Operation::RunProgram(_) | Operation::ReadGuestFile(_) | Operation::WriteGuestFile(_) => {
            (Gesture::Run, None)
        }
        Operation::TakeCheckpoint(_) => (Gesture::Checkpoint, None),
        Operation::RollBack(_) => (Gesture::Rollback, None),
        Operation::StartMachine(_)
        | Operation::StopMachine(_)
        | Operation::PauseMachine(_)
        | Operation::ResumeMachine(_)
        | Operation::RestoreMachine(_)
        | Operation::ForkMachine(_)
        | Operation::RunForks(_)
        | Operation::MakeTemporaryMachine(_)
        | Operation::DeleteMachine(_)
        | Operation::DeleteCheckpoint(_) => (Gesture::Lifecycle, None),
        Operation::ListMachines(_)
        | Operation::GetDaemonInfo(_)
        | Operation::OpenSession(_)
        | Operation::ListSessions(_)
        | Operation::PauseSession(_)
        | Operation::ResumeSession(_)
        | Operation::IssueToken(_)
        | Operation::ListTokens(_)
        | Operation::RevokeToken(_)
        | Operation::ConfigureWeb(_)
        | Operation::GetWebStatus(_)
        | Operation::ListRecordings(_)
        | Operation::NameElements(_)
        | Operation::ObserveElements(_)
        | Operation::ObserveText(_)
        | Operation::OpenRecording(_)
        | Operation::OpenSeenSnapshot(_)
        | Operation::OpenDisplay(_)
        | Operation::OpenEncodedDisplay(_)
        | Operation::SendHostInput(_)
        | Operation::TakeControl(_)
        | Operation::HandBack(_)
        | Operation::WatchActivity(_) => (Gesture::Other, None),
        Operation::MarkScreen(_) => (Gesture::Mark, None),
    };
    let mark = if let Operation::MarkScreen(request) = operation {
        mark(request, &find)
    } else {
        None
    };
    daemon::CallStarted {
        gesture: gesture.into(),
        target,
        mark,
        why: String::new(),
    }
}

/// A mark as consoles draw it: parsed as the request is, its element found.
/// `None` for one that will be refused, so nothing is drawn for it.
fn mark(
    request: &daemon::MarkScreen,
    find: impl Fn(u64, u32) -> Option<daemon::ElementTarget>,
) -> Option<daemon::ScreenMark> {
    let mark = Mark::try_from(request).ok()?;
    let at = match mark.at() {
        MarkAt::Point(at) => daemon::screen_mark::At::Point(at.into()),
        MarkAt::Area(area) => daemon::screen_mark::At::Area(daemon::ElementTarget {
            frame: Some(area.rect().into()),
            label: String::new(),
        }),
        MarkAt::Element(element) => {
            daemon::screen_mark::At::Area(find(element.snapshot.get(), element.node.get())?)
        }
    };
    Some(daemon::ScreenMark {
        shape: request.shape,
        at: Some(at),
        from: match mark.shape() {
            Shape::Arrow { from } => from.map(Into::into),
            Shape::Box | Shape::Dot => None,
        },
        note: mark
            .note()
            .map(|note| note.as_str().to_owned())
            .unwrap_or_default(),
        lasts: Some(match mark.lasts() {
            Lasts::For(time) => daemon::screen_mark::Lasts::ForMs(Millis::saturating(time).get()),
            Lasts::UntilActed(time) => {
                daemon::screen_mark::Lasts::UntilActedMs(Millis::saturating(time).get())
            }
        }),
    })
}

fn input(input: Option<&common::Input>) -> (Gesture, Option<Target>) {
    use common::input::Action;
    let Some(action) = input.and_then(|input| input.action.as_ref()) else {
        return (Gesture::Other, None);
    };
    match action {
        Action::Click(click) => {
            let gesture = match common::MouseButton::try_from(click.button) {
                Ok(common::MouseButton::Right) => Gesture::SecondaryClick,
                Ok(
                    common::MouseButton::Left
                    | common::MouseButton::Middle
                    | common::MouseButton::Unspecified,
                )
                | Err(_) => {
                    if click.count >= 2 {
                        Gesture::DoubleClick
                    } else {
                        Gesture::Click
                    }
                }
            };
            (gesture, click.at.map(Target::Point))
        }
        Action::MoveTo(at) => (Gesture::Move, Some(Target::Point(*at))),
        Action::Drag(drag) => (
            Gesture::Drag,
            Some(Target::Drag(daemon::DragTarget {
                from: drag.from,
                to: drag.to,
            })),
        ),
        Action::Scroll(scroll) => (Gesture::Scroll, scroll.at.map(Target::Point)),
        Action::Key(_) => (Gesture::Key, None),
        Action::TypeText(_) => (Gesture::Type, None),
        Action::Pointer(pointer) => {
            let gesture = match common::PointerPhase::try_from(pointer.phase) {
                Ok(common::PointerPhase::Press) => Gesture::Click,
                Ok(common::PointerPhase::Move) => Gesture::Move,
                Ok(common::PointerPhase::Release | common::PointerPhase::Unspecified) | Err(_) => {
                    Gesture::Other
                }
            };
            (gesture, pointer.at.map(Target::Point))
        }
    }
}

/// What a call read off the screen, and where what it `looked_for` was
/// found, by the same matching the agent's tools use. Text the core can't
/// read is still passed on, with nothing found in it.
pub(crate) fn read(text: &vmm::RecognizedText, looked_for: Option<&str>) -> daemon::TextRead {
    let found = ScreenText::try_from(text.clone())
        .ok()
        .zip(looked_for.and_then(|query| query.parse::<TextQuery>().ok()))
        .map(|(text, query)| {
            text.find(&query)
                .into_iter()
                .map(|found| found.region.into())
                .collect()
        })
        .unwrap_or_default();
    daemon::TextRead {
        text: Some(text.clone()),
        query: looked_for.map(str::to_owned),
        found,
    }
}

/// How a call ended, as its trace tells it, and the snapshot it saw.
pub(crate) fn ended(
    traced: Traced,
    seen: Option<&agent::AxSnapshot>,
    read: Option<daemon::TextRead>,
) -> daemon::CallEnded {
    use daemon::call_ended::Outcome;
    daemon::CallEnded {
        outcome: Some(match traced {
            Traced::Done => Outcome::Done(daemon::Done {}),
            Traced::Failed(why) => Outcome::Failed(why),
            Traced::TimedOut(why) => Outcome::TimedOut(why.unwrap_or_default()),
            Traced::Refused(why) => Outcome::Refused(why),
        }),
        seen: seen.map(shown),
        refs: None,
        read,
    }
}

/// A snapshot as consoles may see it: as the agent got it, but with no
/// password field's value. Other values stay: the screen shows them.
pub(crate) fn shown(snapshot: &agent::AxSnapshot) -> agent::AxSnapshot {
    let mut shown = snapshot.clone();
    for node in &mut shown.nodes {
        match Subrole::parse(&node.subrole) {
            Some(Subrole::SecureTextField) => node.value = None,
            Some(
                Subrole::CloseButton
                | Subrole::MinimizeButton
                | Subrole::ZoomButton
                | Subrole::FullScreenButton
                | Subrole::SearchField
                | Subrole::Other(_),
            )
            | None => {}
        }
    }
    shown
}

/// How many calls of each machine a new console is told of first.
const RECENT: usize = 64;

/// A call as kept for a console that begins watching later: its start, and
/// its end once it has one.
#[derive(Debug)]
struct Past {
    /// The activity that started it.
    started: daemon::Activity,
    at: Instant,
    ended: Option<(Instant, daemon::CallEnded)>,
}

/// Each machine's latest calls, oldest first. Only the latest call that saw
/// a snapshot keeps it: a new console is shown what the agent last saw, and
/// older snapshots would only cost memory.
#[derive(Debug, Default)]
struct History(HashMap<String, VecDeque<Past>>);

impl History {
    fn record(&mut self, activity: &daemon::Activity, at: Instant) {
        use daemon::activity::Phase;
        let calls = self.0.entry(activity.machine.clone()).or_default();
        match &activity.phase {
            Some(Phase::Started(_)) => {
                calls.push_back(Past {
                    started: activity.clone(),
                    at,
                    ended: None,
                });
                while calls.len() > RECENT {
                    calls.pop_front();
                }
            }
            Some(Phase::Ended(ended)) => {
                // Only the latest snapshot and the latest read are kept.
                for past in calls.iter_mut() {
                    if let Some((_, earlier)) = &mut past.ended {
                        if ended.seen.is_some() {
                            earlier.seen = None;
                            earlier.refs = None;
                        }
                        if ended.read.is_some() {
                            earlier.read = None;
                        }
                    }
                }
                let call = calls.iter_mut().rev().find(|past| {
                    past.started.session == activity.session
                        && past.started.request == activity.request
                });
                if let Some(call) = call {
                    call.ended = Some((at, ended.clone()));
                }
            }
            // Names go with the call that kept the snapshot they name.
            Some(Phase::Named(refs)) => {
                let call = calls.iter_mut().rev().find_map(|past| {
                    past.ended.as_mut().map(|(_, ended)| ended).filter(|ended| {
                        ended
                            .seen
                            .as_ref()
                            .is_some_and(|seen| seen.snapshot_id == refs.snapshot)
                    })
                });
                if let Some(ended) = call {
                    ended.refs = Some(refs.clone());
                }
            }
            // Recent calls are told, not recorded.
            Some(Phase::Recent(_)) | None => {}
        }
    }

    /// Every machine's kept calls as a new console is told of them at `now`.
    fn recent(&self, now: Instant) -> Vec<daemon::Activity> {
        let ago = |at: Instant| {
            u64::try_from(now.saturating_duration_since(at).as_millis()).unwrap_or(u64::MAX)
        };
        let mut machines: Vec<&String> = self.0.keys().collect();
        machines.sort();
        machines
            .into_iter()
            .flat_map(|machine| &self.0[machine])
            .filter_map(|past| {
                let Some(daemon::activity::Phase::Started(started)) = &past.started.phase else {
                    return None;
                };
                Some(daemon::Activity {
                    phase: Some(daemon::activity::Phase::Recent(daemon::CallRecent {
                        started: Some(started.clone()),
                        ended: past.ended.as_ref().map(|(_, ended)| ended.clone()),
                        started_ms_ago: ago(past.at),
                        ended_ms_ago: past.ended.as_ref().map(|(at, _)| ago(*at)),
                    })),
                    ..past.started.clone()
                })
            })
            .collect()
    }
}

#[derive(Debug, Default)]
struct Watching {
    next: u64,
    /// By watcher.
    senders: Vec<(WatcherId, SyncSender<daemon::Activity>)>,
    history: History,
}

/// One console watching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WatcherId(u64);

/// The consoles watching: each is told of every machine's recent calls
/// first, then gets every activity from when it began watching. One lock
/// covers both, so no call falls between what a new console is told and
/// what it then gets.
#[derive(Debug, Default)]
pub(crate) struct Watchers(Mutex<Watching>);

impl Watchers {
    fn watching(&self) -> MutexGuard<'_, Watching> {
        // Plain data, valid after any panic that poisoned the lock.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts watching at `now`; dropping the receiver, or
    /// [`Self::unwatch`], stops. The recent calls come first, the latest of
    /// them if there are more than [`BACKLOG`] allows.
    pub(crate) fn watch(&self, now: Instant) -> (WatcherId, Receiver<daemon::Activity>) {
        let (sender, receiver) = mpsc::sync_channel(BACKLOG);
        let mut watching = self.watching();
        let recent = watching.history.recent(now);
        let skip = recent.len().saturating_sub(BACKLOG / 2);
        for activity in recent.into_iter().skip(skip) {
            // Room was left for them; were one refused, the rest would be too.
            if sender.try_send(activity).is_err() {
                break;
            }
        }
        watching.next += 1;
        let id = WatcherId(watching.next);
        watching.senders.push((id, sender));
        (id, receiver)
    }

    /// Forgets `machine`'s recent calls: it was deleted, and a machine made
    /// later under its name (a temporary one, whose names repeat) must not
    /// show a new console the calls of the one before.
    pub(crate) fn forget(&self, machine: &MachineName) {
        self.watching().history.0.remove(machine.as_str());
    }

    /// Stops watcher `id`: its receiver gets what was already sent, then
    /// ends.
    pub(crate) fn unwatch(&self, id: WatcherId) {
        self.watching()
            .senders
            .retain(|(watcher, _)| *watcher != id);
    }

    /// Tells the consoles what a session call does: that it started when it
    /// gets its first turn (or as it ends, without one), how it ended, and
    /// what the session named. Calls without a machine aren't shown.
    pub(crate) fn on_call(&self, event: &CallEvent<'_>) {
        let shown = |call: &SessionCall, phase| {
            call.machine.as_ref().map(|machine| daemon::Activity {
                session: call.session.get(),
                request: call.request,
                machine: machine.to_string(),
                client: call.client.clone(),
                caption: call.caption.clone(),
                phase: Some(phase),
            })
        };
        let started = |call: &SessionCall| {
            call.acted
                .clone()
                .and_then(|acted| shown(call, daemon::activity::Phase::Started(acted)))
        };
        match event {
            CallEvent::Running { .. } => {}
            CallEvent::TurnBegan { call, at } => {
                if let Some(activity) = started(call) {
                    self.publish(&activity, *at);
                }
            }
            CallEvent::Ended {
                call,
                began,
                ended,
                answered,
                ..
            } => {
                if began.is_none()
                    && let Some(activity) = started(call)
                {
                    self.publish(&activity, *ended);
                }
                let phase = daemon::activity::Phase::Ended(self::ended(
                    answered.traced.clone(),
                    answered.seen,
                    answered.read.clone(),
                ));
                if let Some(activity) = shown(call, phase) {
                    self.publish(&activity, *ended);
                }
            }
            CallEvent::Named {
                session,
                request,
                client,
                machine,
                refs,
                at,
            } => self.publish(
                &daemon::Activity {
                    session: session.get(),
                    request: *request,
                    machine: machine.to_string(),
                    client: client.clone(),
                    caption: String::new(),
                    phase: Some(daemon::activity::Phase::Named((*refs).clone())),
                },
                *at,
            ),
        }
    }

    /// Passes `activity`, which happened at `at`, to every console still
    /// watching, and keeps it for those that begin later. One that has
    /// fallen [`BACKLOG`] behind misses it.
    pub(crate) fn publish(&self, activity: &daemon::Activity, at: Instant) {
        let mut watching = self.watching();
        watching.history.record(activity, at);
        watching
            .senders
            .retain(|(_, sender)| match sender.try_send(activity.clone()) {
                Ok(()) | Err(TrySendError::Full(_)) => true,
                Err(TrySendError::Disconnected(_)) => false,
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doppel_host::capabilities::Clock as _;

    fn point(x: f64, y: f64) -> common::Point {
        common::Point { x, y }
    }

    fn node(title: &str, role: &str, frame: Option<(f64, f64, f64, f64)>) -> agent::AxNode {
        agent::AxNode {
            title: title.into(),
            role: role.into(),
            frame: frame.map(|(x, y, width, height)| common::Rect {
                origin: Some(point(x, y)),
                size: Some(common::Size { width, height }),
            }),
            ..agent::AxNode::default()
        }
    }

    fn send(action: common::input::Action) -> Operation {
        Operation::SendInput(daemon::SendInput {
            name: "dev".into(),
            via: 0,
            input: Some(common::Input {
                action: Some(action),
            }),
        })
    }

    fn none(_: u64, _: u32) -> Option<daemon::ElementTarget> {
        None
    }

    fn dev() -> MachineName {
        "dev".parse().unwrap()
    }

    #[test]
    fn a_click_is_shown_where_it_lands_and_how() {
        let click = |button: common::MouseButton, count| {
            started(
                &send(common::input::Action::Click(common::Click {
                    at: Some(point(10.0, 20.0)),
                    button: button.into(),
                    count,
                })),
                none,
            )
        };
        let single = click(common::MouseButton::Left, 1);
        assert_eq!(single.gesture(), Gesture::Click);
        assert_eq!(single.target, Some(Target::Point(point(10.0, 20.0))));
        assert_eq!(
            click(common::MouseButton::Left, 2).gesture(),
            Gesture::DoubleClick
        );
        assert_eq!(
            click(common::MouseButton::Right, 1).gesture(),
            Gesture::SecondaryClick
        );
    }

    #[test]
    fn typing_has_no_place_and_never_its_text() {
        let typed = started(
            &send(common::input::Action::TypeText(common::TypeText {
                text: "hunter2".into(),
            })),
            none,
        );
        assert_eq!(typed.gesture(), Gesture::Type);
        assert_eq!(typed.target, None);
        assert!(!format!("{typed:?}").contains("hunter2"));
    }

    #[test]
    fn an_action_on_an_element_points_at_it_from_its_snapshot() {
        let elements = Elements::default();
        let snapshot = agent::AxSnapshot {
            snapshot_id: 7,
            nodes: vec![
                node("", "AXWindow", Some((0.0, 0.0, 800.0, 600.0))),
                node("Equals", "AXButton", Some((300.0, 400.0, 40.0, 30.0))),
                node("", "AXGroup", None),
            ],
            ..agent::AxSnapshot::default()
        };
        elements.saw(&dev(), &snapshot);
        let act = |snapshot_id, node_index| {
            started(
                &Operation::ActOnElement(daemon::ActOnElement {
                    name: "dev".into(),
                    action: Some(agent::PerformAxAction {
                        snapshot_id,
                        node_index,
                        action: Some(agent::perform_ax_action::Action::Press(agent::Empty {})),
                    }),
                }),
                |snapshot, index| elements.find(&dev(), snapshot, index),
            )
        };
        let pressed = act(7, 1);
        assert_eq!(pressed.gesture(), Gesture::Act);
        let Some(Target::Element(target)) = pressed.target else {
            panic!("no element: {pressed:?}");
        };
        assert_eq!(target.label, "Equals");
        assert_eq!(
            target.frame.and_then(|frame| frame.origin),
            Some(point(300.0, 400.0))
        );
        // An element with no frame, an index past the end, or a snapshot not
        // kept has no place: the call is still shown.
        for (snapshot, index) in [(7, 2), (7, 9), (8, 1)] {
            let call = act(snapshot, index);
            assert_eq!(call.gesture(), Gesture::Act);
            assert_eq!(call.target, None);
        }
        elements.forget(&dev());
        assert_eq!(act(7, 1).target, None);
    }

    #[test]
    fn setting_a_fields_value_is_typing_into_it() {
        let set = |action| {
            acting(Some(&agent::PerformAxAction {
                snapshot_id: 7,
                node_index: 1,
                action: Some(action),
            }))
        };
        use agent::perform_ax_action::Action;
        assert_eq!(
            set(Action::SetValue("ada@example.com".into())),
            Gesture::Type
        );
        assert_eq!(set(Action::Press(agent::Empty {})), Gesture::Act);
        assert_eq!(acting(None), Gesture::Act);
    }

    #[test]
    fn a_mark_on_an_element_is_drawn_on_its_frame_and_a_bad_one_not_at_all() {
        let elements = Elements::default();
        let snapshot = agent::AxSnapshot {
            snapshot_id: 7,
            nodes: vec![node("Equals", "AXButton", Some((300.0, 400.0, 40.0, 30.0)))],
            ..agent::AxSnapshot::default()
        };
        elements.saw(&dev(), &snapshot);
        let mark = |at, note: &str| {
            started(
                &Operation::MarkScreen(daemon::MarkScreen {
                    name: "dev".into(),
                    shape: daemon::MarkShape::Box.into(),
                    at: Some(at),
                    from: None,
                    note: note.into(),
                    lasts: Some(daemon::mark_screen::Lasts::ForMs(5000)),
                }),
                |snapshot, index| elements.find(&dev(), snapshot, index),
            )
        };
        let element = |node_index| {
            daemon::mark_screen::At::Element(daemon::ElementOf {
                snapshot_id: 7,
                node_index,
            })
        };
        let marked = mark(element(0), "  the  answer ");
        assert_eq!(marked.gesture(), Gesture::Mark);
        // The pointer doesn't go there: a mark has no target of its own.
        assert_eq!(marked.target, None);
        let drawn = marked.mark.unwrap();
        assert_eq!(drawn.note, "the answer");
        let Some(daemon::screen_mark::At::Area(area)) = drawn.at else {
            panic!("no area: {drawn:?}");
        };
        assert_eq!(area.label, "Equals");
        // An element not kept, or a note too long: nothing to draw.
        assert_eq!(mark(element(5), "").mark, None);
        assert_eq!(mark(element(0), &"x".repeat(201)).mark, None);
    }

    #[test]
    fn an_element_is_named_by_title_then_description_then_role() {
        let named = |node: &agent::AxNode| element(node).map(|target| target.label);
        let frame = Some((0.0, 0.0, 10.0, 10.0));
        assert_eq!(
            named(&node("Save", "AXButton", frame)).as_deref(),
            Some("Save")
        );
        let described = agent::AxNode {
            description: "close button".into(),
            ..node(" ", "AXButton", frame)
        };
        assert_eq!(named(&described).as_deref(), Some("close button"));
        assert_eq!(
            named(&node("", "AXButton", frame)).as_deref(),
            Some("Button")
        );
        assert_eq!(
            named(&node("Empty", "AXButton", Some((0.0, 0.0, 0.0, 10.0)))),
            None
        );
    }

    #[test]
    fn a_password_field_is_shown_without_its_value() {
        let text = |value: &str| {
            Some(agent::AxValue {
                value: Some(agent::ax_value::Value::Text(value.into())),
            })
        };
        let snapshot = agent::AxSnapshot {
            snapshot_id: 2,
            nodes: vec![
                agent::AxNode {
                    subrole: "AXSecureTextField".into(),
                    value: text("hunter2"),
                    ..node("Password", "AXTextField", Some((0.0, 0.0, 200.0, 24.0)))
                },
                agent::AxNode {
                    value: text("ada@example.com"),
                    ..node("Email", "AXTextField", Some((0.0, 30.0, 200.0, 24.0)))
                },
            ],
            ..agent::AxSnapshot::default()
        };
        let call = ended(Traced::Done, Some(&snapshot), None);
        assert!(!format!("{call:?}").contains("hunter2"));
        let seen = call.seen.unwrap();
        assert_eq!(seen.nodes[0].value, None);
        assert_eq!(seen.nodes[0].title, "Password", "the field itself is kept");
        assert_eq!(seen.nodes[1].value, snapshot.nodes[1].value);
        assert_eq!(seen.snapshot_id, 2);
        assert_eq!(ended(Traced::Done, None, None).seen, None);
    }

    #[test]
    fn only_the_latest_snapshots_are_kept() {
        let elements = Elements::default();
        for id in 0..10 {
            elements.saw(
                &dev(),
                &agent::AxSnapshot {
                    snapshot_id: id,
                    nodes: vec![node("a", "AXButton", Some((0.0, 0.0, 1.0, 1.0)))],
                    ..agent::AxSnapshot::default()
                },
            );
        }
        assert!(elements.find(&dev(), 9, 0).is_some());
        assert!(elements.find(&dev(), 6, 0).is_some());
        assert!(elements.find(&dev(), 5, 0).is_none());
    }

    #[test]
    fn a_new_console_is_told_each_machine_s_recent_calls_first_with_only_the_latest_snapshot() {
        use daemon::activity::Phase;
        use std::time::Duration;
        let watchers = Watchers::default();
        let t0 = doppel_host::capabilities::SystemClock.now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let call = |request: u64, phase: Phase| daemon::Activity {
            session: 1,
            request,
            machine: "dev".into(),
            client: "agent".into(),
            caption: format!("call {request}"),
            phase: Some(phase),
        };
        let look = || {
            Phase::Started(daemon::CallStarted {
                why: String::new(),
                mark: None,
                gesture: Gesture::Look.into(),
                target: None,
            })
        };
        let seen = |id: u64| agent::AxSnapshot {
            snapshot_id: id,
            ..agent::AxSnapshot::default()
        };
        watchers.publish(&call(1, look()), at(0));
        watchers.publish(
            &call(1, Phase::Ended(ended(Traced::Done, Some(&seen(1)), None))),
            at(100),
        );
        watchers.publish(&call(2, look()), at(200));
        watchers.publish(
            &call(2, Phase::Ended(ended(Traced::Done, Some(&seen(2)), None))),
            at(300),
        );
        // Names find the call that kept their snapshot; names of one no
        // longer kept are dropped.
        let named = |snapshot: u64| {
            call(
                9,
                Phase::Named(daemon::ElementRefs {
                    snapshot,
                    generation: 4,
                    nodes: vec![2, 7],
                }),
            )
        };
        watchers.publish(&named(2), at(310));
        watchers.publish(&named(1), at(320));
        watchers.publish(&call(3, look()), at(400));
        let (_, console) = watchers.watch(at(1000));
        watchers.publish(
            &call(3, Phase::Ended(ended(Traced::Done, None, None))),
            at(1100),
        );
        let told: Vec<daemon::Activity> = console.try_iter().collect();
        let recent: Vec<(u64, u64, Option<u64>, Option<u64>)> = told[..3]
            .iter()
            .map(|activity| {
                let Some(Phase::Recent(recent)) = &activity.phase else {
                    panic!("{activity:?}");
                };
                (
                    activity.request,
                    recent.started_ms_ago,
                    recent.ended_ms_ago,
                    recent
                        .ended
                        .as_ref()
                        .and_then(|ended| ended.seen.as_ref())
                        .map(|seen| seen.snapshot_id),
                )
            })
            .collect();
        // Oldest first; only the latest snapshot kept; a call under way has no end yet.
        assert_eq!(
            recent,
            vec![
                (1, 1000, Some(900), None),
                (2, 800, Some(700), Some(2)),
                (3, 600, None, None)
            ]
        );
        let names: Vec<Option<u32>> = told[..3]
            .iter()
            .map(|activity| match &activity.phase {
                Some(Phase::Recent(recent)) => recent
                    .ended
                    .as_ref()
                    .and_then(|ended| ended.refs.as_ref())
                    .map(|refs| refs.generation),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec![None, Some(4), None]);
        assert_eq!(told[0].caption, "call 1");
        // Then what happens live: the third call's end.
        assert!(matches!(told[3].phase, Some(Phase::Ended(_))));
        assert_eq!(told.len(), 4);
    }

    #[test]
    fn a_new_console_is_told_only_the_latest_calls_of_a_machine() {
        let watchers = Watchers::default();
        let now = doppel_host::capabilities::SystemClock.now();
        for request in 0..RECENT as u64 + 10 {
            watchers.publish(
                &daemon::Activity {
                    request,
                    machine: "dev".into(),
                    phase: Some(daemon::activity::Phase::Started(
                        daemon::CallStarted::default(),
                    )),
                    ..daemon::Activity::default()
                },
                now,
            );
        }
        let told: Vec<u64> = watchers
            .watch(now)
            .1
            .try_iter()
            .map(|activity| activity.request)
            .collect();
        assert_eq!(told.len(), RECENT);
        assert_eq!(told.first(), Some(&10));
    }

    #[test]
    fn a_console_that_stops_watching_is_dropped_and_a_slow_one_misses_events() {
        let watchers = Watchers::default();
        let now = doppel_host::capabilities::SystemClock.now();
        let (_, gone) = watchers.watch(now);
        let (_, slow) = watchers.watch(now);
        drop(gone);
        let activity = daemon::Activity::default();
        for _ in 0..BACKLOG + 5 {
            watchers.publish(&activity, now);
        }
        assert_eq!(watchers.watching().senders.len(), 1);
        assert_eq!(slow.try_iter().count(), BACKLOG);
    }

    #[test]
    fn an_unwatched_console_gets_what_was_sent_then_its_stream_ends() {
        let watchers = Watchers::default();
        let now = doppel_host::capabilities::SystemClock.now();
        let (id, console) = watchers.watch(now);
        let (_, other) = watchers.watch(now);
        watchers.publish(&daemon::Activity::default(), now);
        watchers.unwatch(id);
        watchers.publish(&daemon::Activity::default(), now);
        assert!(console.recv().is_ok());
        assert!(console.recv().is_err(), "nothing after unwatch");
        assert_eq!(other.try_iter().count(), 2);
    }

    #[test]
    fn a_text_read_says_where_what_it_looked_for_was_found() {
        let line = |text: &str, x: f64, y: f64| vmm::TextLine {
            text: text.into(),
            frame: Some(common::Rect {
                origin: Some(point(x, y)),
                size: Some(common::Size {
                    width: 120.0,
                    height: 20.0,
                }),
            }),
            confidence: 1.0,
            words: Vec::new(),
        };
        let text = vmm::RecognizedText {
            display_points: Some(common::Size {
                width: 1280.0,
                height: 800.0,
            }),
            lines: vec![line("Save as", 10.0, 10.0), line("Cancel", 10.0, 40.0)],
            accurate_failed: String::new(),
        };
        let read = read(&text, Some("cancel"));
        assert_eq!(read.query.as_deref(), Some("cancel"));
        assert_eq!(read.text.as_ref().map(|text| text.lines.len()), Some(2));
        let found: Vec<(f64, f64)> = read
            .found
            .iter()
            .map(|rect| {
                let origin = rect.origin.unwrap_or_default();
                (origin.x, origin.y)
            })
            .collect();
        assert_eq!(found, vec![(10.0, 40.0)]);
        // Nothing looked for, or nothing like it: all the text, nothing found.
        assert!(super::read(&text, None).found.is_empty());
        assert!(super::read(&text, Some("Delete")).found.is_empty());
    }

    fn session_call(machine: Option<MachineName>, guest: Guest) -> SessionCall {
        SessionCall {
            session: crate::scheduler::Scheduler::default().open("agent".into()),
            request: 4,
            client: "agent".into(),
            caption: "click Save".into(),
            acted: machine.as_ref().map(|_| daemon::CallStarted::default()),
            machine,
            looked_for: None,
            guest,
        }
    }

    fn ended_event<'a>(
        call: &'a SessionCall,
        began: Option<Instant>,
        seen: Option<&'a agent::AxSnapshot>,
    ) -> CallEvent<'a> {
        let now = doppel_host::capabilities::SystemClock.now();
        CallEvent::Ended {
            call,
            ran_from: now,
            began,
            ended: now,
            waited: None,
            answered: crate::call_events::Answered {
                traced: Traced::Done,
                seen,
                read: None,
            },
        }
    }

    fn phases(console: &Receiver<daemon::Activity>) -> Vec<&'static str> {
        console
            .try_iter()
            .map(|activity| match activity.phase {
                Some(daemon::activity::Phase::Started(_)) => "started",
                Some(daemon::activity::Phase::Ended(_)) => "ended",
                Some(daemon::activity::Phase::Named(_)) => "named",
                Some(daemon::activity::Phase::Recent(_)) => "recent",
                None => "none",
            })
            .collect()
    }

    /// Consoles are told a call started at its first turn, or as it ends
    /// without one; a call without a machine isn't shown.
    #[test]
    fn consoles_are_told_a_call_starts_once_and_how_it_ended() {
        let watchers = Watchers::default();
        let now = doppel_host::capabilities::SystemClock.now();
        let (_, console) = watchers.watch(now);
        let call = session_call(Some(dev()), Guest::Kept);
        watchers.on_call(&CallEvent::Running { call: &call });
        assert_eq!(phases(&console), Vec::<&str>::new());
        watchers.on_call(&CallEvent::TurnBegan {
            call: &call,
            at: now,
        });
        watchers.on_call(&ended_event(&call, Some(now), None));
        assert_eq!(phases(&console), ["started", "ended"]);
        watchers.on_call(&ended_event(&call, None, None));
        assert_eq!(
            phases(&console),
            ["started", "ended"],
            "no turn: told at its end"
        );
        let nowhere = session_call(None, Guest::Kept);
        watchers.on_call(&CallEvent::TurnBegan {
            call: &nowhere,
            at: now,
        });
        watchers.on_call(&ended_event(&nowhere, None, None));
        assert_eq!(phases(&console), Vec::<&str>::new());
    }

    #[test]
    fn the_element_cache_keeps_what_calls_saw_and_forgets_on_a_restart() {
        let elements = Elements::default();
        let snapshot = agent::AxSnapshot {
            snapshot_id: 3,
            nodes: vec![node("Save", "AXButton", Some((0.0, 0.0, 10.0, 10.0)))],
            ..agent::AxSnapshot::default()
        };
        let kept = session_call(Some(dev()), Guest::Kept);
        elements.on_call(&ended_event(&kept, None, Some(&snapshot)));
        assert!(elements.find(&dev(), 3, 0).is_some());
        let restarted = session_call(Some(dev()), Guest::MayRestart);
        elements.on_call(&ended_event(&restarted, None, None));
        assert!(elements.find(&dev(), 3, 0).is_none());
    }

    #[test]
    fn a_deleted_machine_s_calls_are_not_told_to_a_new_console() {
        use daemon::activity::Phase;
        let watchers = Watchers::default();
        let now = doppel_host::capabilities::SystemClock.now();
        let started = |machine: &str| daemon::Activity {
            session: 1,
            request: 1,
            machine: machine.into(),
            client: "agent".into(),
            caption: "call".into(),
            phase: Some(Phase::Started(daemon::CallStarted {
                why: String::new(),
                mark: None,
                gesture: Gesture::Look.into(),
                target: None,
            })),
        };
        watchers.publish(&started("dev"), now);
        watchers.publish(&started("other"), now);
        watchers.forget(&dev());
        let (_, console) = watchers.watch(now);
        let told: Vec<String> = console
            .try_iter()
            .map(|activity| activity.machine)
            .collect();
        assert_eq!(told, ["other"]);
    }
}
