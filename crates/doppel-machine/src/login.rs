//! Getting a guest to an agent-ready desktop without a person: the stored
//! login password and the state machine that decides when to type it.
//!
//! The password is typed only when the agent reports the session locked,
//! re-checked just before typing, and at most once per run: a wrong password
//! never gets retried into an account lockout. With no agent there is no
//! evidence of a login screen, so nothing is typed: the password could land in
//! whatever has focus. A cold boot reaches the desktop through auto-login
//! (golden images), and the machine only waits for the agent.

use std::fmt;
use std::time::{Duration, Instant};

use crate::machine::Entropy;
use crate::screen_probe::HostScreen;
use doppel_desktop::agent::Session;

/// The guest user's login password. No `Display`, and `Debug` is redacted:
/// it reaches the guest only through [`LoginPassword::keystrokes`].
#[derive(Clone, PartialEq, Eq)]
pub struct LoginPassword(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PasswordError {
    #[error("the password is empty")]
    Empty,
    #[error("the password isn't UTF-8")]
    NotUtf8,
    #[error("the password has more than one line")]
    Multiline,
    /// Never says which character: that would reveal part of the password.
    #[error(
        "the password has a character the host keyboard can't type (it types printable US ASCII only)"
    )]
    Untypeable,
    #[error(
        "the password file is readable or writable by others (mode {0:o}); it must be 600 (or stricter)"
    )]
    Exposed(u32),
}

impl LoginPassword {
    /// The longest password accepted, in bytes.
    pub const MAX_LEN: usize = 256;

    /// The length of a generated password: 24 of 62 characters, about 143 bits.
    pub const GENERATED_LEN: usize = 24;

    /// A random password of letters and digits: typeable by the host
    /// keyboard and safe in any shell or field.
    ///
    /// # Errors
    /// The entropy source failed.
    pub fn generate<E: Entropy>(entropy: &mut E) -> Result<Self, E::Error> {
        const ALPHABET: &[u8; 62] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        // Rejection sampling: bytes past the last whole multiple of 62 would
        // favour the first characters.
        const LIMIT: u8 = 248;
        let mut password = String::with_capacity(Self::GENERATED_LEN);
        let mut byte = [0u8; 1];
        while password.len() < Self::GENERATED_LEN {
            entropy.fill(&mut byte)?;
            if byte[0] < LIMIT {
                password.push(char::from(ALPHABET[usize::from(byte[0] % 62)]));
            }
        }
        Ok(Self(password))
    }

    /// Parses a password file's bytes: one line, its final newline dropped.
    ///
    /// # Errors
    /// [`PasswordError`]: empty, not UTF-8, several lines, or a character
    /// the host keyboard path can't type (printable US ASCII only).
    pub fn parse(bytes: &[u8]) -> Result<Self, PasswordError> {
        let text = std::str::from_utf8(bytes).map_err(|_| PasswordError::NotUtf8)?;
        let line = text
            .strip_suffix("\r\n")
            .or_else(|| text.strip_suffix('\n'))
            .unwrap_or(text);
        if line.contains(['\n', '\r']) {
            return Err(PasswordError::Multiline);
        }
        if line.is_empty() {
            return Err(PasswordError::Empty);
        }
        if line.len() > Self::MAX_LEN || !line.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
            return Err(PasswordError::Untypeable);
        }
        Ok(Self(line.to_owned()))
    }

    /// The file's permission bits must keep everyone else out.
    ///
    /// # Errors
    /// [`PasswordError::Exposed`] for any group or other bit.
    pub const fn check_file_mode(mode: u32) -> Result<(), PasswordError> {
        if mode & 0o077 == 0 {
            Ok(())
        } else {
            Err(PasswordError::Exposed(mode & 0o7777))
        }
    }

    /// The text to type at the lock screen. The only way to read it.
    #[must_use]
    pub fn keystrokes(&self) -> &str {
        &self.0
    }

    /// The bytes for the password file.
    #[must_use]
    pub fn file_contents(&self) -> Vec<u8> {
        format!("{}\n", self.0).into_bytes()
    }
}

impl fmt::Debug for LoginPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LoginPassword(<redacted>)")
    }
}

/// Whether a password is available to type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Credentials {
    Stored,
    None,
}

/// What a probe saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    Session(Session),
    /// The agent didn't answer; the reason, for the outcome.
    AgentUnreachable(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoginPolicy {
    /// From the start to giving up while the agent is unreachable or the
    /// session state unreported.
    pub agent_wait: Duration,
    /// From typing to giving up on an active session.
    pub unlock_wait: Duration,
    pub poll: Duration,
    /// After waking the display, before checking again and typing: the
    /// lock screen's password field needs a moment to appear.
    pub settle: Duration,
}

impl LoginPolicy {
    pub const DEFAULT: Self = Self {
        agent_wait: Duration::from_secs(120),
        unlock_wait: Duration::from_secs(20),
        poll: Duration::from_millis(500),
        settle: Duration::from_millis(800),
    };
}

/// How a run ended. Only the first two are ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginOutcome {
    /// The session was already active: nothing typed.
    AlreadyActive { after: Duration },
    /// The password was typed and the session became active.
    Unlocked { after: Duration },
    /// Locked, and no password is stored.
    NeedsPassword,
    /// Typed once; the session stayed locked. Not retried.
    PasswordNotAccepted,
    /// The agent never answered with a session state. Nothing was typed.
    NoAgent { waited: Duration, last: String },
    /// Typed, then the agent stopped answering: whether it unlocked is unknown.
    Unknown { last: String },
}

impl LoginOutcome {
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        match self {
            Self::AlreadyActive { .. } | Self::Unlocked { .. } => true,
            Self::NeedsPassword
            | Self::PasswordNotAccepted
            | Self::NoAgent { .. }
            | Self::Unknown { .. } => false,
        }
    }
}

/// A desktop waited for: how the login went and, once the session is
/// active, whether screenshots show it as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Desktop {
    pub login: LoginOutcome,
    /// `None` until the login is ready: nothing to check before.
    pub screen: Option<HostScreen>,
}

impl Desktop {
    /// Whether the session is active. An unconfirmed screen doesn't change
    /// that: screenshots may lag a few seconds, and the screen says so.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        self.login.is_ready()
    }
}

impl fmt::Display for Desktop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.screen {
            Some(screen) => write!(f, "{}; {screen}", self.login),
            None => write!(f, "{}", self.login),
        }
    }
}

impl fmt::Display for LoginOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyActive { after } => {
                write!(f, "session active after {} ms; nothing typed", after.as_millis())
            }
            Self::Unlocked { after } => {
                write!(f, "unlocked {} ms after typing the password", after.as_millis())
            }
            Self::NeedsPassword => f.write_str(
                "the guest is locked and no password is stored (doppel set-password)",
            ),
            Self::PasswordNotAccepted => f.write_str(
                "typed the stored password once; the guest stayed locked, so it wasn't retried. Check it with doppel set-password",
            ),
            Self::NoAgent { waited, last } => write!(
                f,
                "no session state from the guest agent after {} s ({last}); nothing was typed. Is the agent installed and auto-login on?",
                waited.as_secs()
            ),
            Self::Unknown { last } => write!(
                f,
                "typed the password, then the agent stopped answering ({last}); whether the guest unlocked is unknown"
            ),
        }
    }
}

/// What the shell does next.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub enum Action {
    /// Ask the agent for the session state, no earlier than `at`.
    ProbeAt(Instant),
    /// Wake the display (the agent can, while locked), then report `Woke`.
    Wake,
    /// Type the password and Return through the host keyboard, then report `Typed`.
    TypePassword,
    Finish(LoginOutcome),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Observed {
        observation: Observation,
        now: Instant,
    },
    Woke {
        now: Instant,
    },
    Typed {
        now: Instant,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Login {
    Probing {
        started: Instant,
    },
    Waking {
        started: Instant,
    },
    /// Woken; one more look before typing.
    Confirming {
        started: Instant,
    },
    Typing {
        started: Instant,
    },
    Unlocking {
        started: Instant,
        typed: Instant,
    },
    Done(LoginOutcome),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("login: {event} isn't expected while {state}")]
pub struct IllegalLoginEvent {
    pub state: &'static str,
    pub event: &'static str,
}

/// The login machine with its fixed inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unattended {
    pub policy: LoginPolicy,
    pub credentials: Credentials,
}

impl Unattended {
    pub fn start(self, now: Instant) -> (Login, Vec<Action>) {
        (Login::Probing { started: now }, vec![Action::ProbeAt(now)])
    }

    /// # Errors
    /// [`IllegalLoginEvent`] for an event the state doesn't expect, such as
    /// `Typed` while probing or anything after `Done`.
    pub fn step(
        self,
        state: Login,
        event: Event,
    ) -> Result<(Login, Vec<Action>), IllegalLoginEvent> {
        let illegal = |state: &Login, event: &Event| IllegalLoginEvent {
            state: state.name(),
            event: event.name(),
        };
        let p = self.policy;
        Ok(match (state, event) {
            (
                Login::Probing { started } | Login::Confirming { started },
                Event::Observed {
                    observation: Observation::Session(Session::Active),
                    now,
                },
            ) => Self::finish(LoginOutcome::AlreadyActive {
                after: now.saturating_duration_since(started),
            }),
            (
                Login::Probing { started },
                Event::Observed {
                    observation: Observation::Session(Session::Locked),
                    ..
                },
            ) => match self.credentials {
                Credentials::Stored => (Login::Waking { started }, vec![Action::Wake]),
                Credentials::None => Self::finish(LoginOutcome::NeedsPassword),
            },
            (
                Login::Confirming { started },
                Event::Observed {
                    observation: Observation::Session(Session::Locked),
                    ..
                },
            ) => (Login::Typing { started }, vec![Action::TypePassword]),
            (
                Login::Probing { started } | Login::Confirming { started },
                Event::Observed {
                    observation:
                        observation @ (Observation::AgentUnreachable(_)
                        | Observation::Session(
                            Session::Unreported | Session::Unrecognized(_),
                        )),
                    now,
                },
            ) => {
                let waited = now.saturating_duration_since(started);
                if waited >= p.agent_wait {
                    Self::finish(LoginOutcome::NoAgent {
                        waited,
                        last: Self::describe(&observation),
                    })
                } else {
                    (
                        Login::Probing { started },
                        vec![Action::ProbeAt(now + p.poll)],
                    )
                }
            }
            (Login::Waking { started }, Event::Woke { now }) => (
                Login::Confirming { started },
                vec![Action::ProbeAt(now + p.settle)],
            ),
            (Login::Typing { started }, Event::Typed { now }) => (
                Login::Unlocking {
                    started,
                    typed: now,
                },
                vec![Action::ProbeAt(now + p.poll)],
            ),
            (
                Login::Unlocking { typed, .. },
                Event::Observed {
                    observation: Observation::Session(Session::Active),
                    now,
                },
            ) => Self::finish(LoginOutcome::Unlocked {
                after: now.saturating_duration_since(typed),
            }),
            (Login::Unlocking { started, typed }, Event::Observed { observation, now }) => {
                if now.saturating_duration_since(typed) < p.unlock_wait {
                    (
                        Login::Unlocking { started, typed },
                        vec![Action::ProbeAt(now + p.poll)],
                    )
                } else {
                    Self::finish(match observation {
                        Observation::Session(Session::Locked) => LoginOutcome::PasswordNotAccepted,
                        other @ (Observation::AgentUnreachable(_)
                        | Observation::Session(
                            Session::Active | Session::Unreported | Session::Unrecognized(_),
                        )) => LoginOutcome::Unknown {
                            last: Self::describe(&other),
                        },
                    })
                }
            }
            (
                state @ (Login::Probing { .. }
                | Login::Waking { .. }
                | Login::Confirming { .. }
                | Login::Typing { .. }
                | Login::Unlocking { .. }
                | Login::Done(_)),
                event,
            ) => {
                return Err(illegal(&state, &event));
            }
        })
    }

    fn finish(outcome: LoginOutcome) -> (Login, Vec<Action>) {
        (Login::Done(outcome.clone()), vec![Action::Finish(outcome)])
    }

    fn describe(observation: &Observation) -> String {
        match observation {
            Observation::Session(session) => format!("session {session}"),
            Observation::AgentUnreachable(why) => why.clone(),
        }
    }
}

impl Login {
    const fn name(&self) -> &'static str {
        match self {
            Self::Probing { .. } => "probing",
            Self::Waking { .. } => "waking the display",
            Self::Confirming { .. } => "confirming the lock screen",
            Self::Typing { .. } => "typing",
            Self::Unlocking { .. } => "waiting for the unlock",
            Self::Done(_) => "done",
        }
    }
}

impl Event {
    const fn name(&self) -> &'static str {
        match self {
            Self::Observed { .. } => "an observation",
            Self::Woke { .. } => "woke",
            Self::Typed { .. } => "typed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cycles through the given bytes.
    struct Bytes(Vec<u8>, usize);
    impl Entropy for Bytes {
        type Error = std::convert::Infallible;
        fn fill(&mut self, bytes: &mut [u8]) -> Result<(), Self::Error> {
            for byte in bytes {
                *byte = self.0[self.1 % self.0.len()];
                self.1 += 1;
            }
            Ok(())
        }
    }

    #[test]
    fn a_generated_password_is_letters_and_digits_and_parses_back() {
        let Ok(password) = LoginPassword::generate(&mut Bytes((0..=255).collect(), 0));
        assert_eq!(password.0.len(), LoginPassword::GENERATED_LEN);
        assert!(password.0.bytes().all(|b| b.is_ascii_alphanumeric()));
        assert_eq!(
            LoginPassword::parse(&password.file_contents()),
            Ok(password)
        );
    }

    #[test]
    fn a_generated_password_skips_the_biased_bytes() {
        // 248..=255 would map onto 'A'..'H' a second time: they are drawn again.
        let Ok(password) = LoginPassword::generate(&mut Bytes(vec![255, 248, 61], 0));
        assert_eq!(password.0, "9".repeat(LoginPassword::GENERATED_LEN));
    }

    const MS: Duration = Duration::from_millis(1);

    #[expect(
        clippy::disallowed_methods,
        reason = "an arbitrary anchor: only differences from it are asserted"
    )]
    fn anchor() -> Instant {
        Instant::now()
    }

    fn machine(credentials: Credentials) -> Unattended {
        Unattended {
            policy: LoginPolicy::DEFAULT,
            credentials,
        }
    }

    fn seen(session: Session, now: Instant) -> Event {
        Event::Observed {
            observation: Observation::Session(session),
            now,
        }
    }

    fn unreachable(now: Instant) -> Event {
        Event::Observed {
            observation: Observation::AgentUnreachable("no agent".into()),
            now,
        }
    }

    /// Runs a script of events, returning every action in order.
    fn run(m: Unattended, t0: Instant, events: Vec<Event>) -> (Login, Vec<Action>) {
        let (mut state, mut actions) = m.start(t0);
        for event in events {
            let (next, more) = m.step(state, event).unwrap();
            state = next;
            actions.extend(more);
        }
        (state, actions)
    }

    fn typed_count(actions: &[Action]) -> usize {
        actions
            .iter()
            .filter(|a| **a == Action::TypePassword)
            .count()
    }

    #[test]
    fn passwords_parse_strictly_and_never_show() {
        assert_eq!(
            LoginPassword::parse(b"hunter 2!\n").unwrap().keystrokes(),
            "hunter 2!"
        );
        assert_eq!(LoginPassword::parse(b"p\r\n").unwrap().keystrokes(), "p");
        assert_eq!(LoginPassword::parse(b"").unwrap_err(), PasswordError::Empty);
        assert_eq!(
            LoginPassword::parse(b"\n").unwrap_err(),
            PasswordError::Empty
        );
        assert_eq!(
            LoginPassword::parse(b"a\nb\n").unwrap_err(),
            PasswordError::Multiline
        );
        assert_eq!(
            LoginPassword::parse(&[0xff]).unwrap_err(),
            PasswordError::NotUtf8
        );
        assert_eq!(
            LoginPassword::parse("café".as_bytes()).unwrap_err(),
            PasswordError::Untypeable
        );
        assert_eq!(
            LoginPassword::parse(b"a\tb").unwrap_err(),
            PasswordError::Untypeable
        );
        assert_eq!(
            LoginPassword::parse(&[b'a'; LoginPassword::MAX_LEN + 1]).unwrap_err(),
            PasswordError::Untypeable
        );
        let p = LoginPassword::parse(b"s3cret").unwrap();
        assert_eq!(format!("{p:?}"), "LoginPassword(<redacted>)");
        assert_eq!(LoginPassword::parse(&p.file_contents()).unwrap(), p);
        // Errors never carry the text.
        let error = LoginPassword::parse("s3créť".as_bytes())
            .unwrap_err()
            .to_string();
        assert!(!error.contains("s3") && !error.contains('é'), "{error}");
    }

    #[test]
    fn the_file_must_be_private() {
        assert_eq!(LoginPassword::check_file_mode(0o100_600), Ok(()));
        assert_eq!(LoginPassword::check_file_mode(0o400), Ok(()));
        assert_eq!(
            LoginPassword::check_file_mode(0o640),
            Err(PasswordError::Exposed(0o640))
        );
        assert_eq!(
            LoginPassword::check_file_mode(0o604),
            Err(PasswordError::Exposed(0o604))
        );
    }

    #[test]
    fn an_active_session_types_nothing() {
        let t0 = anchor();
        let (state, actions) = run(
            machine(Credentials::Stored),
            t0,
            vec![seen(Session::Active, t0 + 5 * MS)],
        );
        assert_eq!(
            state,
            Login::Done(LoginOutcome::AlreadyActive { after: 5 * MS })
        );
        assert_eq!(typed_count(&actions), 0);
    }

    #[test]
    fn locked_wakes_confirms_types_once_and_unlocks() {
        let t0 = anchor();
        let p = LoginPolicy::DEFAULT;
        let typed_at = t0 + 2_000 * MS;
        let (state, actions) = run(
            machine(Credentials::Stored),
            t0,
            vec![
                seen(Session::Locked, t0),
                Event::Woke {
                    now: t0 + 1_000 * MS,
                },
                seen(Session::Locked, t0 + 1_900 * MS),
                Event::Typed { now: typed_at },
                seen(Session::Locked, typed_at + 500 * MS),
                seen(Session::Active, typed_at + 1_300 * MS),
            ],
        );
        assert_eq!(
            state,
            Login::Done(LoginOutcome::Unlocked { after: 1_300 * MS })
        );
        assert!(state_is_ready(&state));
        assert_eq!(
            actions,
            [
                Action::ProbeAt(t0),
                Action::Wake,
                Action::ProbeAt(t0 + 1_000 * MS + p.settle),
                Action::TypePassword,
                Action::ProbeAt(typed_at + p.poll),
                Action::ProbeAt(typed_at + 500 * MS + p.poll),
                Action::Finish(LoginOutcome::Unlocked { after: 1_300 * MS }),
            ]
        );
    }

    fn state_is_ready(state: &Login) -> bool {
        matches!(state, Login::Done(outcome) if outcome.is_ready())
    }

    #[test]
    fn a_wrong_password_is_typed_once_and_never_retried() {
        let t0 = anchor();
        let p = LoginPolicy::DEFAULT;
        let typed = t0 + 10 * MS;
        let mut events = vec![
            seen(Session::Locked, t0),
            Event::Woke { now: t0 + 5 * MS },
            seen(Session::Locked, t0 + 8 * MS),
            Event::Typed { now: typed },
        ];
        // Still locked at every poll until the unlock wait runs out.
        let mut at = typed;
        while at.saturating_duration_since(typed) < p.unlock_wait {
            at += p.poll;
            events.push(seen(Session::Locked, at));
        }
        let (state, actions) = run(machine(Credentials::Stored), t0, events);
        assert_eq!(state, Login::Done(LoginOutcome::PasswordNotAccepted));
        assert!(!state_is_ready(&state));
        assert_eq!(typed_count(&actions), 1);
    }

    #[test]
    fn locked_without_a_password_stops() {
        let t0 = anchor();
        let (state, actions) = run(
            machine(Credentials::None),
            t0,
            vec![seen(Session::Locked, t0)],
        );
        assert_eq!(state, Login::Done(LoginOutcome::NeedsPassword));
        assert_eq!(typed_count(&actions), 0);
    }

    #[test]
    fn no_agent_means_nothing_is_typed() {
        let t0 = anchor();
        let p = LoginPolicy::DEFAULT;
        let mut events = Vec::new();
        let mut at = t0;
        while at.saturating_duration_since(t0) < p.agent_wait {
            events.push(unreachable(at));
            at += p.poll;
        }
        events.push(seen(Session::Unreported, at));
        let (state, actions) = run(machine(Credentials::Stored), t0, events);
        let Login::Done(LoginOutcome::NoAgent { waited, last }) = &state else {
            panic!("expected NoAgent, got {state:?}")
        };
        assert!(*waited >= p.agent_wait);
        assert_eq!(last, "session unreported");
        assert_eq!(typed_count(&actions), 0);
        assert!(!actions.contains(&Action::Wake));
    }

    #[test]
    fn the_agent_appearing_later_is_waited_for() {
        // Cold boot with auto-login: unreachable for a while, then active.
        let t0 = anchor();
        let (state, actions) = run(
            machine(Credentials::Stored),
            t0,
            vec![
                unreachable(t0),
                unreachable(t0 + 500 * MS),
                seen(Session::Active, t0 + 30_000 * MS),
            ],
        );
        assert_eq!(
            state,
            Login::Done(LoginOutcome::AlreadyActive { after: 30_000 * MS })
        );
        assert_eq!(typed_count(&actions), 0);
    }

    #[test]
    fn unlocking_elsewhere_while_waking_types_nothing() {
        let t0 = anchor();
        let (state, actions) = run(
            machine(Credentials::Stored),
            t0,
            vec![
                seen(Session::Locked, t0),
                Event::Woke { now: t0 },
                seen(Session::Active, t0 + 900 * MS),
            ],
        );
        assert!(matches!(
            state,
            Login::Done(LoginOutcome::AlreadyActive { .. })
        ));
        assert_eq!(typed_count(&actions), 0);
    }

    #[test]
    fn losing_the_agent_after_typing_is_unknown_not_success() {
        let t0 = anchor();
        let p = LoginPolicy::DEFAULT;
        let typed = t0;
        let (state, _) = run(
            machine(Credentials::Stored),
            t0,
            vec![
                seen(Session::Locked, t0),
                Event::Woke { now: t0 },
                seen(Session::Locked, t0),
                Event::Typed { now: typed },
                unreachable(typed + p.unlock_wait),
            ],
        );
        assert_eq!(
            state,
            Login::Done(LoginOutcome::Unknown {
                last: "no agent".into()
            })
        );
        assert!(!state_is_ready(&state));
    }

    #[test]
    fn unexpected_events_are_typed_errors() {
        let m = machine(Credentials::Stored);
        let t0 = anchor();
        let (probing, _) = m.start(t0);
        let error = m
            .step(probing.clone(), Event::Typed { now: t0 })
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "login: typed isn't expected while probing"
        );
        assert!(m.step(probing, Event::Woke { now: t0 }).is_err());
        let done = Login::Done(LoginOutcome::NeedsPassword);
        assert!(m.step(done, seen(Session::Active, t0)).is_err());
        let typing = Login::Typing { started: t0 };
        assert!(
            m.step(typing, seen(Session::Locked, t0)).is_err(),
            "no second look while typing"
        );
    }
}
