//! A machine's report, parsed once into what the status bar shows: whether
//! its guest agent answers, the app in front, the display's size, and how
//! many checkpoints it keeps.

use doppel_desktop::agent::Session as GuestSession;

use doppel_desktop::agent::AgentReport;
use doppel_machine::vm::MachineReport;

/// Whether someone is logged in to the guest and its screen unlocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Login {
    Active,
    Locked,
    /// The agent couldn't tell, or said something this build doesn't know.
    Unknown,
}

/// The guest's display, in points. Both sides are positive.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Display {
    width: f64,
    height: f64,
}

impl Display {
    /// `None` unless both sides are positive (and finite).
    fn of(width: f64, height: f64) -> Option<Self> {
        (width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0)
            .then_some(Self { width, height })
    }

    /// As the status bar shows it: "1440×900 pt".
    pub fn words(self) -> String {
        format!("{}×{} pt", self.width.round(), self.height.round())
    }
}

/// The guest agent, as the daemon last found it.
#[derive(Debug, Clone, PartialEq)]
pub enum Guest {
    Answering {
        login: Login,
        /// The app in front, by name; `None` when the agent doesn't say.
        front: Option<String>,
        /// `None` when the agent doesn't say.
        display: Option<Display>,
    },
    Unreachable {
        why: String,
    },
    NotRunning,
    Unknown,
}

/// What the status bar shows of a machine's report.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub guest: Guest,
    pub checkpoints: usize,
}

impl Report {
    pub fn parse(report: &MachineReport) -> Self {
        let guest = match &report.agent {
            AgentReport::Connected(status) => Guest::Answering {
                login: match status.session {
                    GuestSession::Active => Login::Active,
                    GuestSession::Locked => Login::Locked,
                    GuestSession::Unreported | GuestSession::Unrecognized(_) => Login::Unknown,
                },
                front: status
                    .frontmost
                    .as_ref()
                    .map(|app| app.name.trim())
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned),
                display: Display::of(status.display.width(), status.display.height()),
            },
            AgentReport::Unreachable(why) => Guest::Unreachable { why: why.clone() },
            AgentReport::NotRunning => Guest::NotRunning,
        };
        Self {
            guest,
            checkpoints: report.checkpoints.len(),
        }
    }
}

/// Calls an agent started on a machine in the last minute, from the start
/// times on its timeline (ms on the timeline's clock).
pub fn calls_per_minute(starts: impl IntoIterator<Item = f64>, now: f64) -> usize {
    starts
        .into_iter()
        .filter(|&at| at <= now && now - at < 60_000.0)
        .count()
}

#[cfg(test)]
mod tests {
    use doppel_desktop::agent::{AgentStatus, ProcessId, RunningApp};
    use doppel_desktop::geometry::Size;
    use doppel_machine::vm::{NextStart, RunState, Status};

    use super::*;

    fn connected(session: GuestSession, front: Option<&str>, size: (f64, f64)) -> MachineReport {
        MachineReport {
            status: Status::Live(RunState::Running),
            agent: AgentReport::Connected(AgentStatus {
                granted: Vec::new(),
                session,
                display: Size::new(size.0, size.1).unwrap(),
                frontmost: front.map(|name| RunningApp {
                    pid: ProcessId::new(1),
                    bundle_id: None,
                    name: name.to_owned(),
                }),
            }),
            checkpoints: vec!["one".parse().unwrap(), "two".parse().unwrap()],
            incomplete: Vec::new(),
        }
    }

    #[test]
    fn an_answering_agent_says_its_login_front_app_and_display() {
        let report = Report::parse(&connected(
            GuestSession::Active,
            Some("Finder"),
            (1440.0, 900.0),
        ));
        assert_eq!(report.checkpoints, 2);
        let Guest::Answering {
            login,
            front,
            display,
        } = report.guest
        else {
            panic!("{:?}", report.guest);
        };
        assert_eq!(login, Login::Active);
        assert_eq!(front.as_deref(), Some("Finder"));
        assert_eq!(display.map(Display::words).as_deref(), Some("1440×900 pt"));
    }

    #[test]
    fn what_the_agent_leaves_out_stays_out() {
        let report = Report::parse(&connected(GuestSession::Locked, Some(""), (0.0, 900.0)));
        assert_eq!(
            report.guest,
            Guest::Answering {
                login: Login::Locked,
                front: None,
                display: None,
            }
        );
        let report = Report::parse(&connected(GuestSession::Unrecognized(9), None, (1.0, 0.0)));
        assert_eq!(
            report.guest,
            Guest::Answering {
                login: Login::Unknown,
                front: None,
                display: None,
            }
        );
        let report = Report::parse(&connected(GuestSession::Unreported, None, (10.0, 10.0)));
        assert!(matches!(
            report.guest,
            Guest::Answering {
                login: Login::Unknown,
                ..
            }
        ));
    }

    #[test]
    fn an_agent_that_cant_be_asked_says_why() {
        let parse = |agent| {
            Report::parse(&MachineReport {
                status: Status::Stopped(NextStart::ColdBoot),
                agent,
                checkpoints: Vec::new(),
                incomplete: Vec::new(),
            })
        };
        assert_eq!(
            parse(AgentReport::Unreachable("no vsock".into())),
            Report {
                guest: Guest::Unreachable {
                    why: "no vsock".into()
                },
                checkpoints: 0,
            }
        );
        assert_eq!(parse(AgentReport::NotRunning).guest, Guest::NotRunning);
    }

    #[test]
    fn the_pace_counts_calls_started_in_the_last_minute() {
        let now = 100_000.0;
        assert_eq!(
            calls_per_minute([30_000.0, 40_000.0, 40_001.0, 99_999.0, 100_000.0], now),
            3
        );
        // A start after now (a clock that went back) doesn't count.
        assert_eq!(calls_per_minute([100_001.0], now), 0);
        assert_eq!(calls_per_minute([], now), 0);
    }
}
