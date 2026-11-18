use doppel_desktop::files::{FileMode, ReadRequest, WritePolicy, WriteRequest};

use super::ParseError;
use crate::pb::agent::v1 as agent;

impl ParseError {
    fn file(field: &'static str, error: &dyn std::fmt::Display) -> Self {
        Self::Invalid {
            field,
            reason: error.to_string(),
        }
    }
}

impl TryFrom<agent::ReadFile> for ReadRequest {
    type Error = ParseError;
    fn try_from(wire: agent::ReadFile) -> Result<Self, ParseError> {
        let path = wire
            .path
            .parse()
            .map_err(|error| ParseError::file("path", &error))?;
        Self::new(path, wire.max_bytes).map_err(|error| ParseError::file("max_bytes", &error))
    }
}

impl TryFrom<agent::WriteFile> for WriteRequest {
    type Error = ParseError;
    fn try_from(wire: agent::WriteFile) -> Result<Self, ParseError> {
        let path = wire
            .path
            .parse()
            .map_err(|error| ParseError::file("path", &error))?;
        // 0 means the default, as the agent reads it.
        let mode = FileMode::new(if wire.mode == 0 { 0o644 } else { wire.mode })
            .map_err(|error| ParseError::file("mode", &error))?;
        let policy = match agent::WritePolicy::try_from(wire.policy) {
            Ok(agent::WritePolicy::CreateNew) => WritePolicy::CreateNew,
            Ok(agent::WritePolicy::Replace) => WritePolicy::Replace,
            Ok(agent::WritePolicy::Unspecified) | Err(_) => {
                return Err(ParseError::UnknownEnum {
                    field: "policy",
                    value: wire.policy,
                });
            }
        };
        Self::new(path, wire.data.to_vec(), mode, policy)
            .map_err(|error| ParseError::file("data", &error))
    }
}

impl From<&ReadRequest> for agent::ReadFile {
    fn from(request: &ReadRequest) -> Self {
        Self {
            path: request.path.as_str().to_owned(),
            max_bytes: request.max_bytes(),
        }
    }
}

impl From<&WriteRequest> for agent::WriteFile {
    fn from(request: &WriteRequest) -> Self {
        Self {
            path: request.path.as_str().to_owned(),
            data: request.data().to_vec().into(),
            mode: request.mode.bits(),
            policy: match request.policy {
                WritePolicy::CreateNew => agent::WritePolicy::CreateNew,
                WritePolicy::Replace => agent::WritePolicy::Replace,
            } as i32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip_and_bad_ones_are_refused() {
        let read = ReadRequest::new("/tmp/a".parse().unwrap(), 10).unwrap();
        assert_eq!(
            ReadRequest::try_from(agent::ReadFile::from(&read)),
            Ok(read)
        );
        let write = WriteRequest::new(
            "/tmp/b".parse().unwrap(),
            b"data".to_vec(),
            FileMode::new(0o755).unwrap(),
            WritePolicy::Replace,
        )
        .unwrap();
        assert_eq!(
            WriteRequest::try_from(agent::WriteFile::from(&write)),
            Ok(write)
        );
        assert!(
            ReadRequest::try_from(agent::ReadFile {
                path: "relative".into(),
                max_bytes: 1,
            })
            .is_err()
        );
        assert!(matches!(
            WriteRequest::try_from(agent::WriteFile {
                path: "/tmp/c".into(),
                ..Default::default()
            }),
            Err(ParseError::UnknownEnum {
                field: "policy",
                ..
            })
        ));
    }
}
