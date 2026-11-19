//! The account macOS creates by itself on a new machine's first boot
//! (Virtualization's guest provisioning, macOS 27 hosts and guests): logged
//! in automatically, with Remote Login on, so no one goes through Setup
//! Assistant and the host can reach the guest before any agent runs.

use std::fmt;
use std::str::FromStr;

use crate::guest_user::GuestUserName;
use doppel_machine::login::LoginPassword;

/// A person's full name for a macOS account: one line of printable text,
/// at most [`FullName::MAX_LEN`] characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullName(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FullNameError {
    #[error("the full name is empty")]
    Empty,
    #[error("the full name is longer than {} characters", FullName::MAX_LEN)]
    TooLong,
    #[error("the full name has a control character (a newline, a tab…)")]
    Control,
}

impl FullName {
    pub const MAX_LEN: usize = 128;

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for FullName {
    type Err = FullNameError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.trim().is_empty() {
            Err(FullNameError::Empty)
        } else if s.chars().count() > Self::MAX_LEN {
            Err(FullNameError::TooLong)
        } else if s.chars().any(char::is_control) {
            Err(FullNameError::Control)
        } else {
            Ok(Self(s.to_owned()))
        }
    }
}

impl fmt::Display for FullName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// How a new machine's first account comes to exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Setup {
    /// A person goes through Setup Assistant on the machine's display.
    Assistant,
    /// macOS creates this account on the first boot, which follows the install.
    Provision(Account),
}

/// The account a first boot creates. Its password is stored as the
/// machine's login password, so `golden prepare` and `login` use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub full_name: FullName,
    pub user: GuestUserName,
    pub password: LoginPassword,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_names() {
        assert_eq!(
            "Doppel Agent".parse::<FullName>().map(|n| n.to_string()),
            Ok("Doppel Agent".to_owned())
        );
        assert_eq!(
            "Zoë Ångström".parse::<FullName>().map(|n| n.to_string()),
            Ok("Zoë Ångström".to_owned())
        );
        assert_eq!("".parse::<FullName>(), Err(FullNameError::Empty));
        assert_eq!("   ".parse::<FullName>(), Err(FullNameError::Empty));
        assert_eq!("a\nb".parse::<FullName>(), Err(FullNameError::Control));
        assert_eq!("a\tb".parse::<FullName>(), Err(FullNameError::Control));
        assert_eq!(
            "é".repeat(128)
                .parse::<FullName>()
                .map(|n| n.as_str().chars().count()),
            Ok(128)
        );
        assert_eq!(
            "é".repeat(129).parse::<FullName>(),
            Err(FullNameError::TooLong)
        );
    }
}
