//! One step of a procedure carried out in a guest through its agent: a file
//! to write, or a program that must exit 0. Procedures plan their steps as
//! data; [`super`] runs them in order.

use std::fmt;

use doppel_desktop::exec::{ExecOutput, ExecRequest, Termination};
use doppel_desktop::files::WriteRequest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestAction {
    Write(WriteRequest),
    /// Must exit 0.
    Run(ExecRequest),
}

/// A step and its name `N`, which says what it does in progress and errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestStep<N> {
    pub name: N,
    pub action: GuestAction,
}

/// A program that didn't exit 0: how it ended, and its stderr, trimmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunFailed {
    pub termination: Termination,
    pub stderr: String,
}

impl RunFailed {
    /// # Errors
    /// [`RunFailed`] unless `output` exited 0.
    pub fn judge(output: &ExecOutput) -> Result<(), Self> {
        match output.termination {
            Termination::Exited(0) => Ok(()),
            termination @ (Termination::Exited(_)
            | Termination::Signaled(_)
            | Termination::TimedOut) => Err(Self {
                termination,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            }),
        }
    }
}

/// How it ended, then `: <stderr>` when there was any.
impl fmt::Display for RunFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.termination)?;
        if !self.stderr.is_empty() {
            write!(f, ": {}", self.stderr)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doppel_desktop::exec::Completeness;

    fn output(termination: Termination, stderr: &str) -> ExecOutput {
        ExecOutput {
            termination,
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
            completeness: Completeness::Full,
        }
    }

    #[test]
    fn only_exit_zero_succeeds_and_a_failure_keeps_its_stderr() {
        assert_eq!(
            RunFailed::judge(&output(Termination::Exited(0), "noise")),
            Ok(())
        );
        let failed =
            RunFailed::judge(&output(Termination::Exited(1), "  no such file\n")).unwrap_err();
        assert_eq!(failed.stderr, "no such file");
        assert_eq!(failed.to_string(), "exited with status 1: no such file");
        let quiet = RunFailed::judge(&output(Termination::TimedOut, "")).unwrap_err();
        assert_eq!(quiet.to_string(), Termination::TimedOut.to_string());
    }
}
