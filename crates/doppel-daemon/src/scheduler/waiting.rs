//! The blocking side of the [`Scheduler`]: it keeps the scheduler behind
//! one lock, makes a request's thread wait until the scheduler lets it run,
//! and carries out the scheduler's other effects (noting a person's control
//! in a recording). A cancelled call is woken by its [`Cancel`]'s hook, so
//! nothing polls.
//!
//! It also keeps each machine's agent [`Session`], which the call whose
//! turn it is gets to use.

use std::collections::HashMap;
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Instant;

use crate::scheduler::control::{ControlError, ControlEvent};
use crate::scheduler::session::{SessionEnd, SessionEvent};
use doppel_machine::machine::MachineName;
use doppel_protocol::transport::{Cancel, Hook};

use super::{
    Action, CallId, Cancelled, Cleared, ConnectionId, Event, PauseError, Scheduler, SessionId,
    SessionView,
};
use crate::call_events::SessionCall;
use crate::recording::Recordings;
use crate::session::Session;

/// The scheduler with its waiting threads, shared by every connection.
#[derive(Debug, Default)]
pub(crate) struct Waiting {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    scheduler: Scheduler,
    /// The threads waiting for the scheduler's answer, by call.
    waiters: HashMap<CallId, Waiter>,
    recordings: HashMap<SessionId, Arc<Recordings>>,
}

/// A thread waiting for the scheduler's answer to an event.
#[derive(Debug)]
enum Waiter {
    Act(SyncSender<Result<Cleared, Cancelled>>),
    Turn(SyncSender<Result<(), Cancelled>>),
}

/// The scheduler's effects carried out once the lock is released.
#[derive(Debug, Default)]
#[must_use = "what the scheduler decided is lost otherwise"]
struct Effects {
    /// Recordings to note a person's control in.
    held: Vec<(Arc<Recordings>, MachineName, Instant, Instant)>,
    /// Recordings of sessions that ended.
    finished: Vec<Arc<Recordings>>,
}

impl Effects {
    /// Writes files: called without the lock.
    fn carry_out(self) -> Vec<Arc<Recordings>> {
        for (recordings, machine, from, to) in self.held {
            recordings.person(&machine, from, to);
        }
        self.finished
    }
}

impl State {
    /// Wakes the threads the actions are for; what touches files is left
    /// for after the lock.
    fn apply(&mut self, actions: Vec<Action>) -> Effects {
        let mut effects = Effects::default();
        for action in actions {
            match action {
                Action::Clear(call, cleared) => self.wake(call, Answer::Cleared(cleared)),
                Action::Grant(call, _) => self.wake(call, Answer::Granted),
                Action::Refuse(call) => self.wake(call, Answer::Refused),
                Action::PersonHeld {
                    session,
                    machine,
                    from,
                    to,
                } => {
                    if let Some(recordings) = self.recordings.get(&session) {
                        effects
                            .held
                            .push((Arc::clone(recordings), machine, from, to));
                    }
                }
                Action::Finish(session) => {
                    if let Some(recordings) = self.recordings.remove(&session) {
                        effects.finished.push(recordings);
                    }
                }
            }
        }
        effects
    }

    /// Wakes `call`'s thread with the answer to what it waits for. The
    /// scheduler answers each event in kind; a thread given the wrong kind (its
    /// sender dropped) takes it as cancelled.
    fn wake(&mut self, call: CallId, answer: Answer) {
        // A gone thread no longer waits; a full channel already has its
        // answer.
        let _ = match (self.waiters.remove(&call), answer) {
            (Some(Waiter::Act(sender)), Answer::Cleared(cleared)) => {
                sender.try_send(Ok(cleared)).is_ok()
            }
            (Some(Waiter::Act(sender)), Answer::Refused) => sender.try_send(Err(Cancelled)).is_ok(),
            (Some(Waiter::Turn(sender)), Answer::Granted) => sender.try_send(Ok(())).is_ok(),
            (Some(Waiter::Turn(sender)), Answer::Refused) => {
                sender.try_send(Err(Cancelled)).is_ok()
            }
            (Some(Waiter::Act(_)), Answer::Granted)
            | (Some(Waiter::Turn(_)), Answer::Cleared(_))
            | (None, Answer::Cleared(_) | Answer::Granted | Answer::Refused) => false,
        };
    }
}

/// The scheduler's answer to a waiting thread.
#[derive(Debug)]
enum Answer {
    Cleared(Cleared),
    Granted,
    Refused,
}

impl Waiting {
    /// Plain data, consistent after any panic: each change completes under
    /// the lock.
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes `step`, carrying out what follows.
    fn handle(&self, event: Event) {
        let effects = {
            let mut state = self.state();
            let actions = state.scheduler.on(event);
            state.apply(actions)
        };
        let _finished = effects.carry_out();
    }

    fn wait(&self, call: CallId, waiter: Waiter, event: Event) {
        let effects = {
            let mut state = self.state();
            state.waiters.insert(call, waiter);
            let actions = state.scheduler.on(event);
            state.apply(actions)
        };
        let _finished = effects.carry_out();
    }

    /// A request `connection` read, as it reads it: it takes its place in
    /// `reserves`'s line now, for when it asks for the turn. Cancelling
    /// `cancel` wakes it from any wait.
    pub(crate) fn arrive(
        self: &Arc<Self>,
        connection: ConnectionId,
        reserves: Option<MachineName>,
        cancel: Cancel,
    ) -> Call {
        let id = self.state().scheduler.arrive(connection, reserves);
        // Outside the lock: it runs at once when already cancelled.
        let hook = cancel.on_cancel({
            let waiting = Arc::clone(self);
            move || waiting.handle(Event::Cancel(id))
        });
        Call {
            waiting: Arc::clone(self),
            id,
            cancel,
            _hook: hook,
            began: OnceLock::new(),
            session_call: OnceLock::new(),
        }
    }

    /// A call outside any connection's order: one the daemon makes itself.
    pub(crate) fn alone(self: &Arc<Self>) -> Call {
        let connection = self.connection();
        self.arrive(connection, None, Cancel::never())
    }

    pub(crate) fn connection(&self) -> ConnectionId {
        self.state().scheduler.connection()
    }

    pub(crate) fn open(&self, client: String) -> SessionId {
        self.state().scheduler.open(client)
    }

    /// Records session `id` from now on.
    pub(crate) fn record(&self, id: SessionId, recordings: Recordings) {
        self.state().recordings.insert(id, Arc::new(recordings));
    }

    pub(crate) fn recordings(&self, id: SessionId) -> Option<Arc<Recordings>> {
        self.state().recordings.get(&id).cloned()
    }

    /// Forgets a session that failed to open: it never ran a call, and
    /// records nothing.
    pub(crate) fn discard(&self, id: SessionId) {
        let mut state = self.state();
        state.scheduler.discard(id);
        state.recordings.remove(&id);
    }

    /// Ends session `id`, for `end`; its recordings are handed back to be
    /// completed.
    pub(crate) fn close(&self, id: SessionId, end: SessionEnd) -> Option<Arc<Recordings>> {
        let effects = {
            let mut state = self.state();
            let actions = state.scheduler.close(id, end);
            state.apply(actions)
        };
        effects.carry_out().pop()
    }

    pub(crate) fn client(&self, id: SessionId) -> Option<String> {
        self.state().scheduler.client(id).map(str::to_owned)
    }

    pub(crate) fn list(&self) -> Vec<SessionView> {
        self.state().scheduler.list()
    }

    pub(crate) fn holds(&self, by: ConnectionId, machine: &MachineName) -> bool {
        self.state().scheduler.holds(by, machine)
    }

    pub(crate) fn pause(&self, id: u64) -> Result<(), PauseError> {
        self.session_event(id, SessionEvent::Pause)
    }

    pub(crate) fn resume(&self, id: u64) -> Result<(), PauseError> {
        self.session_event(id, SessionEvent::Resume)
    }

    fn session_event(&self, id: u64, event: SessionEvent) -> Result<(), PauseError> {
        let effects = {
            let mut state = self.state();
            let actions = state.scheduler.session(id, event)?;
            state.apply(actions)
        };
        let _finished = effects.carry_out();
        Ok(())
    }

    /// A person on connection `by` takes control of `machine`.
    pub(crate) fn take_control(
        &self,
        by: ConnectionId,
        machine: &MachineName,
        at: Instant,
    ) -> Result<(), ControlError<ConnectionId>> {
        self.control(machine, ControlEvent::Take { by, at })
    }

    pub(crate) fn hand_back(
        &self,
        by: ConnectionId,
        machine: &MachineName,
        at: Instant,
    ) -> Result<(), ControlError<ConnectionId>> {
        self.control(machine, ControlEvent::HandBack { by, at })
    }

    fn control(
        &self,
        machine: &MachineName,
        event: ControlEvent<ConnectionId>,
    ) -> Result<(), ControlError<ConnectionId>> {
        let effects = {
            let mut state = self.state();
            let actions = state.scheduler.control(machine, event)?;
            state.apply(actions)
        };
        let _finished = effects.carry_out();
        Ok(())
    }

    /// Connection `by` closed: the machines it controlled go back.
    pub(crate) fn left(&self, by: ConnectionId, at: Instant) {
        let effects = {
            let mut state = self.state();
            let actions = state.scheduler.left(by, at);
            state.apply(actions)
        };
        let _finished = effects.carry_out();
    }
}

/// One request being answered: how to stop it, and its place on the
/// scheduler. Dropping it tells the scheduler it was answered.
#[derive(Debug)]
#[must_use = "dropping the call ends its places in lines"]
pub(crate) struct Call {
    waiting: Arc<Waiting>,
    id: CallId,
    cancel: Cancel,
    _hook: Hook,
    /// When it first got a machine's turn.
    began: OnceLock<Instant>,
    /// The session call it is, once cleared to run: its first turn is told
    /// as an event of it.
    session_call: OnceLock<Arc<SessionCall>>,
}

impl Call {
    pub(crate) const fn cancel(&self) -> &Cancel {
        &self.cancel
    }

    /// The call is one of `session`'s, on `machine`.
    pub(crate) fn join(&self, session: SessionId, machine: Option<MachineName>, caption: String) {
        self.waiting
            .state()
            .scheduler
            .join(self.id, session, machine, caption);
    }

    /// Waits until the call may act: its session isn't paused, and no
    /// person has its machine.
    pub(crate) fn act(&self) -> Result<Cleared, Cancelled> {
        let (sender, answer) = mpsc::sync_channel(1);
        self.waiting
            .wait(self.id, Waiter::Act(sender), Event::Act(self.id));
        answer.recv().unwrap_or(Err(Cancelled))
    }

    /// Waits for `machine`'s turn: in the connection's order when it's the
    /// machine the request named, at the end of the line otherwise (such
    /// as a fork the request starts). The turn's first time is kept.
    pub(crate) fn turn<'s, C>(
        &'s self,
        machine: &MachineName,
        agent_sessions: &'s AgentSessions<C>,
        now: impl FnOnce() -> Instant,
    ) -> Result<(Turn<'s, C>, Begun), Cancelled> {
        let (sender, answer) = mpsc::sync_channel(1);
        self.waiting.wait(
            self.id,
            Waiter::Turn(sender),
            Event::Turn(self.id, machine.clone()),
        );
        answer.recv().unwrap_or(Err(Cancelled))?;
        let at = now();
        let begun = if self.began.set(at).is_ok() {
            Begun::First(at, self.session_call.get().cloned())
        } else {
            Begun::Again
        };
        Ok((
            Turn {
                call: self,
                agent_sessions,
                machine: machine.clone(),
                session: agent_sessions.take(machine),
            },
            begun,
        ))
    }

    /// When it first got a machine's turn, if it has.
    pub(crate) fn began(&self) -> Option<Instant> {
        self.began.get().copied()
    }

    /// The call is `session_call`: its first turn is told as its event.
    pub(crate) fn is_session_call(&self, session_call: Arc<SessionCall>) {
        // Set once, as it is cleared to run.
        let _ = self.session_call.set(session_call);
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        self.waiting.handle(Event::Finish(self.id));
    }
}

/// Whether a turn was its call's first.
#[derive(Debug)]
pub(crate) enum Begun {
    /// At this time, by this session call, if it is one.
    First(Instant, Option<Arc<SessionCall>>),
    Again,
}

/// Each machine's agent session, while no turn holds it.
#[derive(Debug)]
pub(crate) struct AgentSessions<C>(Mutex<HashMap<MachineName, Session<C>>>);

impl<C> Default for AgentSessions<C> {
    fn default() -> Self {
        Self(Mutex::new(HashMap::new()))
    }
}

impl<C> AgentSessions<C> {
    fn lock(&self) -> MutexGuard<'_, HashMap<MachineName, Session<C>>> {
        // Sessions move in and out whole: consistent after any panic.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn take(&self, machine: &MachineName) -> Session<C> {
        self.lock().remove(machine).unwrap_or_default()
    }

    fn put(&self, machine: MachineName, session: Session<C>) {
        self.lock().insert(machine, session);
    }
}

/// A machine's turn: it lasts until dropped, and holds the agent session.
#[derive(Debug)]
#[must_use = "the turn ends when dropped"]
pub(crate) struct Turn<'s, C> {
    call: &'s Call,
    agent_sessions: &'s AgentSessions<C>,
    machine: MachineName,
    session: Session<C>,
}

impl<C> Turn<'_, C> {
    pub(crate) const fn session(&mut self) -> &mut Session<C> {
        &mut self.session
    }
}

impl<C> Drop for Turn<'_, C> {
    fn drop(&mut self) {
        // Back before the next turn takes it.
        self.agent_sessions
            .put(self.machine.clone(), std::mem::take(&mut self.session));
        self.call
            .waiting
            .handle(Event::EndTurn(self.call.id, self.machine.clone()));
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use doppel_host::capabilities::{Clock as _, SystemClock};

    use super::*;

    fn dev() -> MachineName {
        "dev".parse().unwrap()
    }

    /// Waiter threads are woken by the scheduler: by the turn passing on, and
    /// by a cancel, with nothing polling.
    #[test]
    fn a_waiting_call_wakes_when_its_turn_comes_or_it_is_cancelled() {
        let waiting = Arc::new(Waiting::default());
        let agent_sessions = AgentSessions::<SystemClock>::default();
        let holder = waiting.alone();
        let (held, begun) = holder
            .turn(&dev(), &agent_sessions, || SystemClock.now())
            .unwrap();
        assert!(matches!(begun, Begun::First(_, None)));
        let cancel = Cancel::new();
        let cancelled = waiting.arrive(waiting.connection(), None, cancel.clone());
        let next = waiting.alone();
        std::thread::scope(|scope| {
            let cancelled = scope.spawn(|| {
                cancelled
                    .turn(&dev(), &agent_sessions, || SystemClock.now())
                    .map(|_| ())
            });
            let next = scope.spawn(|| {
                next.turn(&dev(), &agent_sessions, || SystemClock.now())
                    .map(|(_, begun)| matches!(begun, Begun::First(..)))
            });
            std::thread::sleep(Duration::from_millis(50));
            assert!(!cancelled.is_finished() && !next.is_finished());
            cancel.cancel();
            assert_eq!(cancelled.join().unwrap(), Err(Cancelled));
            assert!(!next.is_finished(), "the turn is still held");
            drop(held);
            assert_eq!(next.join().unwrap(), Ok(true));
        });
        // The holder's later turn is not its first.
        let (_, again) = holder
            .turn(&dev(), &agent_sessions, || SystemClock.now())
            .unwrap();
        assert!(matches!(again, Begun::Again));
    }
}
