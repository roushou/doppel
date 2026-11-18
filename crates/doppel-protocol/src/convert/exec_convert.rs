use std::time::Duration;

use doppel_desktop::exec::{Completeness, ExecOutput, ExecRequest, Termination};

use super::ParseError;
use crate::pb::agent::v1 as agent;

impl agent::Exec {
    /// The request, its timeout in whole milliseconds: below one is
    /// dropped ([`ExecRequest`] bounds it above).
    #[must_use]
    pub fn in_millis(request: &ExecRequest) -> Self {
        Self {
            program: request.program().as_str().to_owned(),
            arguments: request.arguments().to_vec(),
            stdin: request.stdin().to_vec().into(),
            timeout_ms: u32::try_from(request.timeout().as_millis()).unwrap_or(u32::MAX),
            max_output_bytes: request.max_output(),
            working_directory: request
                .working_directory()
                .map(|d| d.as_str().to_owned())
                .unwrap_or_default(),
        }
    }
}

impl TryFrom<agent::ExecResult> for ExecOutput {
    type Error = ParseError;
    fn try_from(wire: agent::ExecResult) -> Result<Self, ParseError> {
        use agent::exec_result::Termination as Wire;
        let termination = match wire.termination.ok_or(ParseError::Missing("termination"))? {
            Wire::ExitCode(code) => Termination::Exited(code),
            Wire::Signal(signal) => Termination::Signaled(signal),
            Wire::TimedOut(_) => Termination::TimedOut,
        };
        let completeness = match agent::OutputCompleteness::try_from(wire.completeness) {
            Ok(agent::OutputCompleteness::Full) => Completeness::Full,
            Ok(agent::OutputCompleteness::Truncated) => Completeness::Truncated,
            Ok(agent::OutputCompleteness::Unspecified) | Err(_) => {
                Completeness::Unreported(wire.completeness)
            }
        };
        Ok(Self {
            termination,
            stdout: wire.stdout.to_vec(),
            stderr: wire.stderr.to_vec(),
            completeness,
        })
    }
}

impl From<&ExecOutput> for agent::ExecResult {
    fn from(output: &ExecOutput) -> Self {
        use agent::exec_result::Termination as Wire;
        Self {
            termination: Some(match output.termination {
                Termination::Exited(code) => Wire::ExitCode(code),
                Termination::Signaled(signal) => Wire::Signal(signal),
                Termination::TimedOut => Wire::TimedOut(agent::Empty {}),
            }),
            stdout: output.stdout.clone().into(),
            stderr: output.stderr.clone().into(),
            completeness: match output.completeness {
                Completeness::Full => agent::OutputCompleteness::Full as i32,
                Completeness::Truncated => agent::OutputCompleteness::Truncated as i32,
                Completeness::Unreported(raw) => raw,
            },
        }
    }
}

impl TryFrom<agent::Exec> for ExecRequest {
    type Error = ParseError;
    fn try_from(wire: agent::Exec) -> Result<Self, ParseError> {
        let invalid = |field, error: &dyn std::fmt::Display| ParseError::Invalid {
            field,
            reason: error.to_string(),
        };
        let request = Self::new(
            wire.program
                .parse()
                .map_err(|error| invalid("program", &error))?,
            wire.arguments,
            Duration::from_millis(u64::from(wire.timeout_ms)),
            wire.max_output_bytes,
        )
        .map_err(|error| invalid("exec", &error))?
        .with_stdin(wire.stdin.to_vec());
        Ok(if wire.working_directory.is_empty() {
            request
        } else {
            request.in_directory(
                wire.working_directory
                    .parse()
                    .map_err(|error| invalid("working_directory", &error))?,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_results_round_trip() {
        let request = ExecRequest::new(
            "/bin/echo".parse().unwrap(),
            vec!["hi".into(), "there".into()],
            Duration::from_millis(1500),
            100,
        )
        .unwrap()
        .with_stdin(b"in".to_vec());
        assert_eq!(
            ExecRequest::try_from(agent::Exec::in_millis(&request)),
            Ok(request.clone())
        );
        let elsewhere = request.in_directory("/tmp".parse().unwrap());
        assert_eq!(
            ExecRequest::try_from(agent::Exec::in_millis(&elsewhere)),
            Ok(elsewhere)
        );
        for output in [
            ExecOutput {
                termination: Termination::Exited(3),
                stdout: b"out".to_vec(),
                stderr: b"err".to_vec(),
                completeness: Completeness::Full,
            },
            ExecOutput {
                termination: Termination::Signaled(9),
                stdout: Vec::new(),
                stderr: Vec::new(),
                completeness: Completeness::Unreported(7),
            },
        ] {
            assert_eq!(
                ExecOutput::try_from(agent::ExecResult::from(&output)),
                Ok(output)
            );
        }
    }

    #[test]
    fn requests_and_results_cross_the_wire() {
        let request = ExecRequest::new(
            "/bin/echo".parse().unwrap(),
            vec!["hi".into()],
            Duration::from_millis(1500),
            100,
        )
        .unwrap()
        .with_stdin(b"in".to_vec())
        .in_directory("/tmp".parse().unwrap());
        let wire = agent::Exec::in_millis(&request);
        assert_eq!(
            (
                wire.program.as_str(),
                wire.timeout_ms,
                wire.working_directory.as_str()
            ),
            ("/bin/echo", 1500, "/tmp")
        );
        assert_eq!(&wire.stdin[..], b"in");

        let out = ExecOutput::try_from(agent::ExecResult {
            termination: Some(agent::exec_result::Termination::TimedOut(agent::Empty {})),
            stdout: b"partial".to_vec().into(),
            stderr: Default::default(),
            completeness: agent::OutputCompleteness::Truncated as i32,
        })
        .unwrap();
        assert_eq!(out.termination, Termination::TimedOut);
        assert_eq!(out.completeness, Completeness::Truncated);
        assert_eq!(
            ExecOutput::try_from(agent::ExecResult::default()),
            Err(ParseError::Missing("termination"))
        );
    }
}
