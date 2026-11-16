//! What the guest agent reports about its session.

use std::fmt;

use crate::geometry::Size;

/// A vsock port in the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VsockPort(u32);

impl VsockPort {
    /// Where `doppel-agent` listens.
    pub const AGENT: Self = Self(7400);

    #[must_use]
    pub const fn new(port: u32) -> Self {
        Self(port)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// A macOS privacy permission the agent needs for some operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    /// Needed to capture the screen.
    ScreenRecording,
    /// Needed to post input events (and, later, to read AX trees).
    Accessibility,
    /// Reported by a newer agent.
    Unrecognized(i32),
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ScreenRecording => f.write_str("Screen Recording"),
            Self::Accessibility => f.write_str("Accessibility"),
            Self::Unrecognized(raw) => write!(f, "unrecognized permission {raw}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    /// The agent's GUI session is logged in and in front.
    Active,
    /// Screen locked, or another session is in front.
    Locked,
    /// The agent couldn't tell.
    Unreported,
    Unrecognized(i32),
}

impl fmt::Display for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Active => f.write_str("active"),
            Self::Locked => f.write_str("locked"),
            Self::Unreported => f.write_str("unreported"),
            Self::Unrecognized(raw) => write!(f, "unrecognized ({raw})"),
        }
    }
}

/// A process in the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProcessId(i32);

impl ProcessId {
    #[must_use]
    pub const fn new(raw: i32) -> Self {
        Self(raw)
    }
    #[must_use]
    pub const fn get(self) -> i32 {
        self.0
    }
}

impl fmt::Display for ProcessId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// An app's bundle identifier, as macOS reports it (`com.apple.TextEdit`).
/// Never empty: a process without one has none.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BundleId(String);

impl BundleId {
    /// `None` for an empty id, which a process without one reports.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Option<Self> {
        let id = id.into();
        (!id.is_empty()).then_some(Self(id))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for BundleId {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BundleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningApp {
    pub pid: ProcessId,
    pub bundle_id: Option<BundleId>,
    pub name: String,
}

/// What the agent pushes, unasked, to a subscribed session. Hints that the
/// guest changed, coalesced and droppable: a reader looks again (takes a
/// snapshot) rather than trusting them as a record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestEvent {
    /// Another app came to the front; `None` when none is.
    FrontmostAppChanged(Option<RunningApp>),
    /// This app's accessibility tree changed (one event may stand for many).
    UiChanged(ProcessId),
    PermissionsChanged(Vec<Permission>),
    /// A kind this build doesn't know, from a newer agent.
    Unrecognized,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentStatus {
    pub granted: Vec<Permission>,
    pub session: Session,
    pub display: Size,
    pub frontmost: Option<RunningApp>,
}

impl AgentStatus {
    /// The permissions the agent needs and doesn't have.
    #[must_use]
    pub fn missing(&self) -> Vec<Permission> {
        [Permission::ScreenRecording, Permission::Accessibility]
            .into_iter()
            .filter(|p| !self.granted.contains(p))
            .collect()
    }
}

/// The guest agent, as the daemon last found it.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentReport {
    Connected(AgentStatus),
    /// Why it couldn't be asked.
    Unreachable(String),
    /// The VM isn't running, so there is no agent to ask.
    NotRunning,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundle_id_is_never_empty() {
        assert_eq!(BundleId::new(""), None);
        assert_eq!(
            BundleId::new("com.apple.TextEdit").as_deref(),
            Some("com.apple.TextEdit")
        );
    }

    #[test]
    fn missing_ignores_unrecognized_grants() {
        let status = AgentStatus {
            granted: vec![Permission::Accessibility, Permission::Unrecognized(9)],
            session: Session::Active,
            display: Size::new(1.0, 1.0).unwrap(),
            frontmost: None,
        };
        assert_eq!(status.missing(), vec![Permission::ScreenRecording]);
    }
}
