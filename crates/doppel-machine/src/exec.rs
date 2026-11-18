//! Running a program in the guest: a validated request and its typed outcome.

use std::fmt;
use std::time::Duration;

use crate::guest_path::GuestPath;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExecError {
    #[error("timeout must be between 1 ms and {max} s (got {0:?})", max = ExecRequest::MAX_TIMEOUT.as_secs())]
    Timeout(Duration),
    #[error("max output must be between 1 byte and {max} bytes (got {0})", max = ExecRequest::MAX_OUTPUT)]
    OutputLimit(u32),
}

/// A program to run, with no shell unless the program is one.
///
/// `stdin` may carry secrets (a `sudo -S` password), so `Debug` shows only its length.
#[derive(Clone, PartialEq, Eq)]
pub struct ExecRequest {
    program: GuestPath,
    arguments: Vec<String>,
    stdin: Vec<u8>,
    working_directory: Option<GuestPath>,
    timeout: Duration,
    max_output: u32,
}

impl fmt::Debug for ExecRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecRequest")
            .field("program", &self.program)
            .field("arguments", &self.arguments)
            .field("stdin_bytes", &self.stdin.len())
            .field("working_directory", &self.working_directory)
            .field("timeout", &self.timeout)
            .field("max_output", &self.max_output)
            .finish()
    }
}

impl ExecRequest {
    /// The agent counts timeouts in whole milliseconds.
    pub const MIN_TIMEOUT: Duration = Duration::from_millis(1);
    pub const MAX_TIMEOUT: Duration = Duration::from_secs(600);
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
    /// Per stream. Frames are at most 64 MiB; leave room for both streams.
    pub const MAX_OUTPUT: u32 = 16 << 20;
    pub const DEFAULT_OUTPUT: u32 = 64 << 10;

    /// # Errors
    /// [`ExecError`] for a timeout or output limit out of range.
    pub fn new(
        program: GuestPath,
        arguments: Vec<String>,
        timeout: Duration,
        max_output: u32,
    ) -> Result<Self, ExecError> {
        if timeout < Self::MIN_TIMEOUT || timeout > Self::MAX_TIMEOUT {
            return Err(ExecError::Timeout(timeout));
        }
        if max_output == 0 || max_output > Self::MAX_OUTPUT {
            return Err(ExecError::OutputLimit(max_output));
        }
        Ok(Self {
            program,
            arguments,
            stdin: Vec::new(),
            working_directory: None,
            timeout,
            max_output,
        })
    }

    #[must_use]
    pub fn with_stdin(mut self, stdin: Vec<u8>) -> Self {
        self.stdin = stdin;
        self
    }

    #[must_use]
    pub fn in_directory(mut self, directory: GuestPath) -> Self {
        self.working_directory = Some(directory);
        self
    }

    #[must_use]
    pub const fn program(&self) -> &GuestPath {
        &self.program
    }
    #[must_use]
    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }
    #[must_use]
    pub fn stdin(&self) -> &[u8] {
        &self.stdin
    }
    #[must_use]
    pub const fn working_directory(&self) -> Option<&GuestPath> {
        self.working_directory.as_ref()
    }
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }
    #[must_use]
    pub const fn max_output(&self) -> u32 {
        self.max_output
    }
}

/// How the program ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Termination {
    Exited(i32),
    Signaled(i32),
    /// The agent stopped it at the timeout.
    TimedOut,
}

impl fmt::Display for Termination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exited(code) => write!(f, "exited with status {code}"),
            Self::Signaled(signal) => write!(f, "killed by signal {signal}"),
            Self::TimedOut => f.write_str("timed out and was stopped"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completeness {
    Full,
    /// Output beyond the limit was dropped.
    Truncated,
    Unreported(i32),
}

#[derive(Clone, PartialEq, Eq)]
pub struct ExecOutput {
    pub termination: Termination,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub completeness: Completeness,
}

impl fmt::Debug for ExecOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecOutput")
            .field("termination", &self.termination)
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_bytes", &self.stderr.len())
            .field("completeness", &self.completeness)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sw_vers() -> GuestPath {
        "/usr/bin/sw_vers".parse().unwrap()
    }

    #[test]
    fn limits_are_checked() {
        assert!(ExecRequest::new(sw_vers(), vec![], Duration::ZERO, 1).is_err());
        // The agent counts whole milliseconds: less would be sent as none.
        assert!(ExecRequest::new(sw_vers(), vec![], Duration::from_micros(500), 1).is_err());
        assert!(ExecRequest::new(sw_vers(), vec![], ExecRequest::MIN_TIMEOUT, 1).is_ok());
        assert!(ExecRequest::new(sw_vers(), vec![], Duration::from_secs(601), 1).is_err());
        assert!(ExecRequest::new(sw_vers(), vec![], Duration::from_secs(1), 0).is_err());
        assert!(
            ExecRequest::new(
                sw_vers(),
                vec![],
                Duration::from_secs(1),
                ExecRequest::MAX_OUTPUT + 1
            )
            .is_err()
        );
        assert!(
            ExecRequest::new(
                sw_vers(),
                vec![],
                ExecRequest::MAX_TIMEOUT,
                ExecRequest::MAX_OUTPUT
            )
            .is_ok()
        );
    }

    #[test]
    fn debug_never_shows_stdin() {
        let request = ExecRequest::new(
            "/usr/bin/sudo".parse().unwrap(),
            vec!["-S".into()],
            Duration::from_secs(5),
            10,
        )
        .unwrap()
        .with_stdin(b"hunter2\n".to_vec());
        let shown = format!("{request:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("stdin_bytes: 8"), "{shown}");
    }
}
