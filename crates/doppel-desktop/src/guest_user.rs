//! The guest's user: the macOS account a machine's desktop logs in as, which
//! provisioning creates and every later procedure acts for.

use std::str::FromStr;
use std::time::Duration;

use crate::exec::{ExecError, ExecOutput, ExecRequest, Termination};
use crate::guest_path::GuestPathError;

/// A macOS account short name: 1–64 ASCII letters, digits, `_`, `.` or `-`,
/// not starting with `-`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestUserName(String);

/// Why there's no user name: the text isn't one, or the probe didn't give one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GuestUserError {
    #[error("{0:?} is not an account short name")]
    NotAName(String),
    #[error("{0}")]
    Ended(Termination),
    #[error("not UTF-8")]
    NotUtf8,
    #[error(transparent)]
    Path(#[from] GuestPathError),
    #[error(transparent)]
    Exec(#[from] ExecError),
}

impl FromStr for GuestUserName {
    type Err = GuestUserError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let valid = (1..=64).contains(&s.len())
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
            && !s.starts_with('-');
        if valid {
            Ok(Self(s.to_owned()))
        } else {
            Err(GuestUserError::NotAName(s.to_owned()))
        }
    }
}

impl GuestUserName {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Prints the user's short name.
    ///
    /// # Errors
    /// Never in practice: the request is fixed.
    pub fn probe() -> Result<ExecRequest, GuestUserError> {
        Ok(ExecRequest::new(
            "/usr/bin/id".parse()?,
            vec!["-un".into()],
            Duration::from_secs(10),
            1024,
        )?)
    }

    /// # Errors
    /// [`GuestUserError`] unless `id -un` exited 0 with a short name.
    pub fn parse(output: &ExecOutput) -> Result<Self, GuestUserError> {
        if output.termination != Termination::Exited(0) {
            return Err(GuestUserError::Ended(output.termination));
        }
        std::str::from_utf8(&output.stdout)
            .map_err(|_| GuestUserError::NotUtf8)?
            .trim()
            .parse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::Completeness;

    #[test]
    fn user_names_parse_from_the_probe() {
        let out = |code, text: &str| ExecOutput {
            termination: Termination::Exited(code),
            stdout: text.into(),
            stderr: Vec::new(),
            completeness: Completeness::Full,
        };
        assert_eq!(
            GuestUserName::parse(&out(0, "doppelsandbox\n"))
                .unwrap()
                .as_str(),
            "doppelsandbox"
        );
        for bad in [
            out(1, "x\n"),
            out(0, ""),
            out(0, "a b\n"),
            out(0, "-x\n"),
            out(0, "a;rm\n"),
        ] {
            assert!(GuestUserName::parse(&bad).is_err(), "{bad:?}");
        }
    }
}
