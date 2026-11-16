//! Reading and writing guest files over the agent channel.

use std::fmt;

use crate::guest_path::GuestPath;

/// Bytes per call either way. Frames are at most 64 MiB; larger files belong
/// on a shared directory.
pub const MAX_FILE_BYTES: u32 = 32 << 20;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FileError {
    #[error(
        "at most {MAX_FILE_BYTES} bytes per call (got {0}); use a shared directory for larger files"
    )]
    TooLarge(u64),
    #[error("a size limit must be at least 1 byte")]
    ZeroLimit,
    #[error("mode {0:o} has bits outside 0o777 (no setuid, setgid or sticky bits)")]
    Mode(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadRequest {
    pub path: GuestPath,
    max_bytes: u32,
}

impl ReadRequest {
    /// # Errors
    /// [`FileError`] for a zero or oversized limit.
    pub fn new(path: GuestPath, max_bytes: u32) -> Result<Self, FileError> {
        match max_bytes {
            0 => Err(FileError::ZeroLimit),
            n if n > MAX_FILE_BYTES => Err(FileError::TooLarge(u64::from(n))),
            n => Ok(Self { path, max_bytes: n }),
        }
    }

    #[must_use]
    pub const fn max_bytes(&self) -> u32 {
        self.max_bytes
    }
}

/// Permission bits for a written file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMode(u32);

impl FileMode {
    pub const DEFAULT: Self = Self(0o644);
    pub const EXECUTABLE: Self = Self(0o755);

    /// # Errors
    /// [`FileError::Mode`] for bits outside 0o777.
    pub const fn new(bits: u32) -> Result<Self, FileError> {
        if bits & !0o777 == 0 {
            Ok(Self(bits))
        } else {
            Err(FileError::Mode(bits))
        }
    }

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WritePolicy {
    /// Fail if the path exists.
    CreateNew,
    /// Create, or replace an existing file.
    Replace,
}

#[derive(Clone, PartialEq, Eq)]
pub struct WriteRequest {
    pub path: GuestPath,
    data: Vec<u8>,
    pub mode: FileMode,
    pub policy: WritePolicy,
}

impl fmt::Debug for WriteRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WriteRequest")
            .field("path", &self.path)
            .field("bytes", &self.data.len())
            .field("mode", &format_args!("{:o}", self.mode.bits()))
            .field("policy", &self.policy)
            .finish()
    }
}

impl WriteRequest {
    /// # Errors
    /// [`FileError::TooLarge`] past [`MAX_FILE_BYTES`].
    pub fn new(
        path: GuestPath,
        data: Vec<u8>,
        mode: FileMode,
        policy: WritePolicy,
    ) -> Result<Self, FileError> {
        let len = u64::try_from(data.len()).unwrap_or(u64::MAX);
        if len > u64::from(MAX_FILE_BYTES) {
            return Err(FileError::TooLarge(len));
        }
        Ok(Self {
            path,
            data,
            mode,
            policy,
        })
    }

    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path() -> GuestPath {
        "/tmp/x".parse().unwrap()
    }

    #[test]
    fn limits_and_modes_are_checked() {
        assert_eq!(ReadRequest::new(path(), 0), Err(FileError::ZeroLimit));
        assert!(ReadRequest::new(path(), MAX_FILE_BYTES + 1).is_err());
        assert!(ReadRequest::new(path(), MAX_FILE_BYTES).is_ok());
        assert_eq!(FileMode::new(0o4755), Err(FileError::Mode(0o4755)));
        assert_eq!(FileMode::new(0o600).unwrap().bits(), 0o600);
        let big = vec![0u8; MAX_FILE_BYTES as usize + 1];
        assert!(matches!(
            WriteRequest::new(path(), big, FileMode::DEFAULT, WritePolicy::Replace),
            Err(FileError::TooLarge(_))
        ));
    }

    #[test]
    fn debug_shows_sizes_not_contents() {
        let w = WriteRequest::new(
            path(),
            b"secret".to_vec(),
            FileMode::DEFAULT,
            WritePolicy::CreateNew,
        )
        .unwrap();
        let shown = format!("{w:?}");
        assert!(
            !shown.contains("secret") && shown.contains("bytes: 6") && shown.contains("644"),
            "{shown}"
        );
    }
}
