//! Schedules the daemon's requests (calls): decides when each may run,
//! from the sessions, a person's control of a machine, and each machine's
//! line of calls.
//!
//! A session is one connection that opened one (such as an agent's
//! `doppel mcp`); it ends when that connection closes, and with the daemon.
//! Its calls wait while it's paused, or while a person at a viewer has the
//! machine they name; when the person hands it back, each session that used
//! the machine is owed a note.
//!
//! A machine runs one call at a time, in arrival order, so a click can't
//! land in the middle of a checkpoint; other machines aren't held up. A
//! connection's calls take their places in the order the connection read
//! them, even when the later one asks first.
//!
//! [`Scheduler`] is a pure state machine: each change is a method that
//! returns the [`Action`]s it leads to, such as letting a call run. It never
//! blocks, sleeps or reads a clock. [`Waiting`] (in `waiting.rs`) is the
//! blocking side: request threads wait in it for the scheduler's answers.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{Duration, Instant};

use doppel_machine::machine::MachineName;

#[cfg(test)]
mod tests;
// The blocking side; only it uses locks and channels.
mod waiting;

pub(crate) mod control;
pub(crate) mod session;

use control::{Control, ControlAction, ControlError, ControlEvent};
use session::{SessionAction, SessionEnd, SessionError, SessionEvent, SessionState};

pub(crate) use waiting::{AgentSessions, Begun, Call, Turn, Waiting};

/// An open session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct SessionId(u64);

impl SessionId {
    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "session {}", self.0)
    }
}

/// One client connection: what holds a machine's control, and what orders
/// its calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ConnectionId(u64);

/// One request being answered, from when its connection read it until it
/// is answered. Later calls have greater ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct CallId(u64);

/// A note a session is owed: a person had control of `machine` for `held`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HandedBack {
    pub(crate) machine: MachineName,
    pub(crate) held: Duration,
}

/// A session's call may act.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cleared {
    /// The note it's owed for its machine.
    pub(crate) note: Option<HandedBack>,
    /// Whether it had to wait: the session was paused, or a person had the
    /// machine.
    pub(crate) held_up: HeldUp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeldUp {
    No,
    Yes,
}

/// The call was cancelled before what it waited for came.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Cancelled;

/// Why a session can't be paused or resumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PauseError {
    #[error("no session {0} is open")]
    NoSuchSession(u64),
    #[error(transparent)]
    Session(#[from] SessionError),
}

/// One session, as others see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionView {
    pub(crate) id: SessionId,
    pub(crate) client: String,
    /// In first-use order.
    pub(crate) machines: Vec<MachineName>,
    pub(crate) answered: u64,
    /// Captions of the calls running or waiting, oldest first.
    pub(crate) running: Vec<String>,
    pub(crate) state: SessionState,
}

/// An input to the scheduler: what a call asks of it, or tells it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    /// To act: its session isn't paused, and no person has its machine.
    Act(CallId),
    /// `machine`'s turn. A call may take several in its run, one at a time.
    Turn(CallId, MachineName),
    /// It is done with `machine`'s turn.
    EndTurn(CallId, MachineName),
    /// Its client cancelled it: what it waits for, and asks for from now
    /// on, is refused.
    Cancel(CallId),
    /// It was answered: its places in lines go, and its turns end.
    Finish(CallId),
}

/// What the waiting side carries out after a change.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use = "a call waiting for its wake never gets it otherwise"]
pub(crate) enum Action {
    /// The call that waited to act may.
    Clear(CallId, Cleared),
    /// The call that waited for `machine`'s turn has it.
    Grant(CallId, MachineName),
    /// What the call waited for won't come: it was cancelled.
    Refuse(CallId),
    /// A person had `machine` from `from` to `to`, and `session` used it:
    /// its recording shows that.
    PersonHeld {
        session: SessionId,
        machine: MachineName,
        from: Instant,
        to: Instant,
    },
    /// The session ended: what it recorded is completed.
    Finish(SessionId),
}

/// Who may act. See the module documentation.
#[derive(Debug, Default)]
pub(crate) struct Scheduler {
    next_session: u64,
    next_connection: u64,
    next_call: u64,
    sessions: BTreeMap<SessionId, Session>,
    /// Machines a person controls; the others are the agents'.
    control: HashMap<MachineName, Control<ConnectionId>>,
    /// Only machines with a call in line, or holding the turn.
    lines: HashMap<MachineName, Line>,
    calls: BTreeMap<CallId, CallState>,
}

#[derive(Debug)]
struct Session {
    client: String,
    machines: Vec<MachineName>,
    answered: u64,
    state: SessionState,
    /// Notes owed, by machine: how long a person last had it.
    owed: HashMap<MachineName, Duration>,
}

#[derive(Debug)]
struct CallState {
    connection: ConnectionId,
    /// The machine whose line it takes its place in as it arrives.
    reserves: Option<MachineName>,
    /// Its session, with its machine and caption, once it joined one.
    joined: Option<Joined>,
    waits: Waits,
    standing: Standing,
    /// The machines whose turn it has.
    holding: Vec<MachineName>,
}

#[derive(Debug)]
struct Joined {
    session: SessionId,
    machine: Option<MachineName>,
    caption: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Waits {
    Nothing,
    /// To act ([`Event::Act`]).
    ToAct,
    /// For this machine's turn.
    Turn(MachineName),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standing {
    Live,
    Cancelled,
}

/// One machine's line.
#[derive(Debug, Default)]
struct Line {
    holder: Option<CallId>,
    /// Waiting for the turn, in the order they'll get it.
    queue: VecDeque<CallId>,
    /// Calls that named the machine, in arrival order, until they join the
    /// queue or end. One joins only once its connection's earlier ones did.
    reserved: Vec<CallId>,
}

impl Line {
    fn is_idle(&self) -> bool {
        self.holder.is_none() && self.queue.is_empty() && self.reserved.is_empty()
    }
}

impl Scheduler {
    /// Opens a session for `client`, in its own words.
    pub(crate) fn open(&mut self, client: String) -> SessionId {
        self.next_session += 1;
        let id = SessionId(self.next_session);
        self.sessions.insert(
            id,
            Session {
                client,
                machines: Vec::new(),
                answered: 0,
                state: SessionState::Active,
                owed: HashMap::new(),
            },
        );
        id
    }

    /// Forgets a session that failed to open: it never ran a call.
    pub(crate) fn discard(&mut self, id: SessionId) {
        self.sessions.remove(&id);
    }

    /// Ends the session, for `end`. Its calls no longer wait for it.
    pub(crate) fn close(&mut self, id: SessionId, end: SessionEnd) -> Vec<Action> {
        let Some(session) = self.sessions.remove(&id) else {
            return Vec::new();
        };
        let ended = match session.state.on(SessionEvent::End(end)) {
            Ok((_, ended)) => ended,
            // Removed from the table as it ends: it can't have ended before.
            Err(_) => vec![SessionAction::Finish],
        };
        let mut actions = Vec::new();
        for action in ended {
            match action {
                SessionAction::Hold => {}
                SessionAction::Release => self.clear_waiting(&mut actions),
                SessionAction::Finish => actions.push(Action::Finish(id)),
            }
        }
        actions
    }

    /// A new connection's id.
    pub(crate) fn connection(&mut self) -> ConnectionId {
        self.next_connection += 1;
        ConnectionId(self.next_connection)
    }

    /// A request `connection` read: it takes its place in `reserves`'s
    /// line now, for when it asks for the turn.
    pub(crate) fn arrive(
        &mut self,
        connection: ConnectionId,
        reserves: Option<MachineName>,
    ) -> CallId {
        self.next_call += 1;
        let id = CallId(self.next_call);
        if let Some(machine) = &reserves {
            self.lines
                .entry(machine.clone())
                .or_default()
                .reserved
                .push(id);
        }
        self.calls.insert(
            id,
            CallState {
                connection,
                reserves,
                joined: None,
                waits: Waits::Nothing,
                standing: Standing::Live,
                holding: Vec::new(),
            },
        );
        id
    }

    /// The call is one of `session`'s, on `machine`: the session shows it
    /// while it runs, and counts it once answered.
    pub(crate) fn join(
        &mut self,
        call: CallId,
        session: SessionId,
        machine: Option<MachineName>,
        caption: String,
    ) {
        let Some(entry) = self.calls.get_mut(&call) else {
            return;
        };
        if let (Some(open), Some(machine)) = (self.sessions.get_mut(&session), &machine)
            && !open.machines.contains(machine)
        {
            open.machines.push(machine.clone());
        }
        entry.joined = Some(Joined {
            session,
            machine,
            caption,
        });
    }

    pub(crate) fn on(&mut self, event: Event) -> Vec<Action> {
        let mut actions = Vec::new();
        match event {
            Event::Act(call) => self.act(call, &mut actions),
            Event::Turn(call, machine) => self.turn(call, machine, &mut actions),
            Event::EndTurn(call, machine) => self.end_turn(call, &machine, &mut actions),
            Event::Cancel(call) => self.cancel(call, &mut actions),
            Event::Finish(call) => self.finish(call, &mut actions),
        }
        actions
    }

    /// Pauses or resumes session `id`, as a client names it.
    pub(crate) fn session(
        &mut self,
        id: u64,
        event: SessionEvent,
    ) -> Result<Vec<Action>, PauseError> {
        let session = self
            .sessions
            .get_mut(&SessionId(id))
            .ok_or(PauseError::NoSuchSession(id))?;
        let (next, changes) = session.state.on(event)?;
        session.state = next;
        let mut actions = Vec::new();
        for change in changes {
            match change {
                // Its calls ask the state when they ask to act.
                SessionAction::Hold => {}
                SessionAction::Release => self.clear_waiting(&mut actions),
                SessionAction::Finish => actions.push(Action::Finish(SessionId(id))),
            }
        }
        Ok(actions)
    }

    /// A person takes, hands back or leaves `machine`.
    pub(crate) fn control(
        &mut self,
        machine: &MachineName,
        event: ControlEvent<ConnectionId>,
    ) -> Result<Vec<Action>, ControlError<ConnectionId>> {
        let at = match event {
            ControlEvent::Take { at, .. }
            | ControlEvent::HandBack { at, .. }
            | ControlEvent::Left { at, .. } => at,
        };
        let current = self
            .control
            .get(machine)
            .copied()
            .unwrap_or(Control::Agents);
        let (next, changes) = current.on(event)?;
        match next {
            Control::Agents => self.control.remove(machine),
            Control::Person { .. } => self.control.insert(machine.clone(), next),
        };
        let mut actions = Vec::new();
        for change in changes {
            match change {
                // Calls on it ask the control when they ask to act.
                ControlAction::Hold => {}
                ControlAction::Release { held } => {
                    for (id, session) in &mut self.sessions {
                        if session.machines.contains(machine) {
                            session.owed.insert(machine.clone(), held);
                            actions.push(Action::PersonHeld {
                                session: *id,
                                machine: machine.clone(),
                                from: at.checked_sub(held).unwrap_or(at),
                                to: at,
                            });
                        }
                    }
                    self.clear_waiting(&mut actions);
                }
            }
        }
        Ok(actions)
    }

    /// Connection `by` closed: the machines it controlled go back.
    pub(crate) fn left(&mut self, by: ConnectionId, at: Instant) -> Vec<Action> {
        let held: Vec<MachineName> = self.control.keys().cloned().collect();
        let mut actions = Vec::new();
        for machine in held {
            match self.control(&machine, ControlEvent::Left { holder: by, at }) {
                Ok(more) => actions.extend(more),
                // Leaving is never refused.
                Err(
                    ControlError::NotHeld
                    | ControlError::NotTheHolder { .. }
                    | ControlError::Held { .. },
                ) => {}
            }
        }
        actions
    }

    /// Who drives the session, in its own words.
    pub(crate) fn client(&self, id: SessionId) -> Option<&str> {
        self.sessions
            .get(&id)
            .map(|session| session.client.as_str())
    }

    /// Whether connection `by` has control of `machine`.
    pub(crate) fn holds(&self, by: ConnectionId, machine: &MachineName) -> bool {
        match self.control.get(machine) {
            Some(Control::Person { holder, .. }) => *holder == by,
            Some(Control::Agents) | None => false,
        }
    }

    pub(crate) fn list(&self) -> Vec<SessionView> {
        self.sessions
            .iter()
            .map(|(id, session)| SessionView {
                id: *id,
                client: session.client.clone(),
                machines: session.machines.clone(),
                answered: session.answered,
                running: self
                    .calls
                    .values()
                    .filter_map(|call| call.joined.as_ref())
                    .filter(|joined| joined.session == *id)
                    .map(|joined| joined.caption.clone())
                    .collect(),
                state: session.state,
            })
            .collect()
    }

    fn act(&mut self, id: CallId, actions: &mut Vec<Action>) {
        let Some(call) = self.calls.get(&id) else {
            return;
        };
        match call.standing {
            Standing::Cancelled => actions.push(Action::Refuse(id)),
            Standing::Live => match self.cleared(id, HeldUp::No) {
                Some(cleared) => actions.push(Action::Clear(id, cleared)),
                None => {
                    if let Some(call) = self.calls.get_mut(&id) {
                        call.waits = Waits::ToAct;
                    }
                }
            },
        }
    }

    /// What call `id` is told if it may act now: its note, taken.
    fn cleared(&mut self, id: CallId, held_up: HeldUp) -> Option<Cleared> {
        let joined = self.calls.get(&id)?.joined.as_ref();
        let session = joined.map(|joined| joined.session);
        let machine = joined.and_then(|joined| joined.machine.as_ref());
        let session_may = session
            .and_then(|session| self.sessions.get(&session))
            .is_none_or(|session| session.state.may_act());
        let machine_may = machine
            .and_then(|machine| self.control.get(machine))
            .is_none_or(Control::agents_may_act);
        if !(session_may && machine_may) {
            return None;
        }
        let note = machine.zip(session).and_then(|(machine, session)| {
            let held = self.sessions.get_mut(&session)?.owed.remove(machine)?;
            Some(HandedBack {
                machine: machine.clone(),
                held,
            })
        });
        Some(Cleared { note, held_up })
    }

    /// Clears the calls waiting to act that now may.
    fn clear_waiting(&mut self, actions: &mut Vec<Action>) {
        let waiting: Vec<CallId> = self
            .calls
            .iter()
            .filter(|(_, call)| call.waits == Waits::ToAct)
            .map(|(id, _)| *id)
            .collect();
        for id in waiting {
            if let Some(cleared) = self.cleared(id, HeldUp::Yes) {
                if let Some(call) = self.calls.get_mut(&id) {
                    call.waits = Waits::Nothing;
                }
                actions.push(Action::Clear(id, cleared));
            }
        }
    }

    fn turn(&mut self, id: CallId, machine: MachineName, actions: &mut Vec<Action>) {
        let Some(call) = self.calls.get_mut(&id) else {
            return;
        };
        match call.standing {
            Standing::Cancelled => {
                actions.push(Action::Refuse(id));
                return;
            }
            Standing::Live => {}
        }
        call.waits = Waits::Turn(machine.clone());
        let connection = call.connection;
        let line = self.lines.entry(machine.clone()).or_default();
        match line.reserved.iter().position(|reserved| *reserved == id) {
            // Its place was kept: it's next once its connection's earlier
            // ones went ahead.
            Some(_) => self.admit(&machine, connection),
            None => line.queue.push_back(id),
        }
        self.serve(&machine, actions);
    }

    /// Moves `connection`'s reserved calls on `machine` into the queue, in
    /// arrival order, while the earliest of them asks for the turn.
    fn admit(&mut self, machine: &MachineName, connection: ConnectionId) {
        let Some(line) = self.lines.get_mut(machine) else {
            return;
        };
        loop {
            let Some(index) = line.reserved.iter().position(|reserved| {
                self.calls
                    .get(reserved)
                    .is_some_and(|call| call.connection == connection)
            }) else {
                return;
            };
            let first = line.reserved[index];
            let asks = self
                .calls
                .get(&first)
                .is_some_and(|call| call.waits == Waits::Turn(machine.clone()));
            if !asks {
                return;
            }
            line.reserved.remove(index);
            line.queue.push_back(first);
        }
    }

    /// Gives `machine`'s turn to the next in line, if no one has it.
    fn serve(&mut self, machine: &MachineName, actions: &mut Vec<Action>) {
        let Some(line) = self.lines.get_mut(machine) else {
            return;
        };
        if line.holder.is_none()
            && let Some(next) = line.queue.pop_front()
        {
            line.holder = Some(next);
            if let Some(call) = self.calls.get_mut(&next) {
                call.waits = Waits::Nothing;
                call.holding.push(machine.clone());
            }
            actions.push(Action::Grant(next, machine.clone()));
        }
        if line.is_idle() {
            self.lines.remove(machine);
        }
    }

    fn end_turn(&mut self, id: CallId, machine: &MachineName, actions: &mut Vec<Action>) {
        let Some(line) = self.lines.get_mut(machine) else {
            return;
        };
        if line.holder != Some(id) {
            return;
        }
        line.holder = None;
        if let Some(call) = self.calls.get_mut(&id) {
            call.holding.retain(|held| held != machine);
        }
        self.serve(machine, actions);
    }

    fn cancel(&mut self, id: CallId, actions: &mut Vec<Action>) {
        let Some(call) = self.calls.get_mut(&id) else {
            return;
        };
        call.standing = Standing::Cancelled;
        match std::mem::replace(&mut call.waits, Waits::Nothing) {
            Waits::Nothing => {}
            Waits::ToAct => actions.push(Action::Refuse(id)),
            Waits::Turn(machine) => {
                self.leave_line(id, &machine, actions);
                actions.push(Action::Refuse(id));
            }
        }
    }

    /// Call `id` gives up its place in `machine`'s line, waiting or kept.
    fn leave_line(&mut self, id: CallId, machine: &MachineName, actions: &mut Vec<Action>) {
        let Some(line) = self.lines.get_mut(machine) else {
            return;
        };
        line.queue.retain(|queued| *queued != id);
        let was_reserved = line.reserved.contains(&id);
        line.reserved.retain(|reserved| *reserved != id);
        if was_reserved && let Some(call) = self.calls.get(&id) {
            // Its connection's later calls no longer wait for it.
            let connection = call.connection;
            self.admit(machine, connection);
        }
        self.serve(machine, actions);
    }

    fn finish(&mut self, id: CallId, actions: &mut Vec<Action>) {
        let Some(call) = self.calls.get_mut(&id) else {
            return;
        };
        call.standing = Standing::Cancelled;
        let waits = std::mem::replace(&mut call.waits, Waits::Nothing);
        let reserves = call.reserves.clone();
        let holding = std::mem::take(&mut call.holding);
        if let Waits::Turn(machine) = &waits {
            self.leave_line(id, machine, actions);
        }
        if let Some(machine) = &reserves {
            self.leave_line(id, machine, actions);
        }
        for machine in &holding {
            self.end_turn(id, machine, actions);
        }
        if let Some(call) = self.calls.remove(&id)
            && let Some(joined) = call.joined
            && let Some(session) = self.sessions.get_mut(&joined.session)
        {
            session.answered += 1;
        }
    }
}
