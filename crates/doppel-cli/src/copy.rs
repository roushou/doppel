//! `doppel cp`'s two sides, as written on its command line.

use std::path::PathBuf;
use std::str::FromStr;

use doppel_machine::guest_path::{GuestPath, GuestPathError};
use doppel_machine::machine::{MachineName, SpecError};

/// One side of a copy, as written on the command line: `machine:/abs/path`
/// for a guest file, anything else for a host file (write `./a:b` for a host
/// path that contains a colon).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyEnd {
    Host(PathBuf),
    Guest {
        machine: MachineName,
        path: GuestPath,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CopyEndError {
    #[error("empty path")]
    Empty,
    #[error("machine in {0:?}: {1}")]
    Machine(String, SpecError),
    #[error("guest path in {0:?}: {1}")]
    Path(String, GuestPathError),
}

impl FromStr for CopyEnd {
    type Err = CopyEndError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(CopyEndError::Empty);
        }
        match s.split_once(':') {
            // A colon after a slash is part of a host path (`./a:b`, `/x/a:b`).
            Some((machine, path)) if !machine.contains('/') => Ok(Self::Guest {
                machine: machine
                    .parse()
                    .map_err(|e| CopyEndError::Machine(s.to_owned(), e))?,
                path: path
                    .parse()
                    .map_err(|e| CopyEndError::Path(s.to_owned(), e))?,
            }),
            Some(_) | None => Ok(Self::Host(PathBuf::from(s))),
        }
    }
}

/// A copy between the host and one guest: exactly one side is a guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Copy {
    Upload {
        from: PathBuf,
        machine: MachineName,
        to: GuestPath,
    },
    Download {
        machine: MachineName,
        from: GuestPath,
        to: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CopyError {
    #[error("both sides are host paths; one must be machine:/path")]
    BothHost,
    #[error("both sides are guest paths; copy through the host in two steps")]
    BothGuest,
}

impl Copy {
    /// # Errors
    /// [`CopyError`] unless exactly one side is in a guest.
    pub fn between(source: CopyEnd, destination: CopyEnd) -> Result<Self, CopyError> {
        match (source, destination) {
            (CopyEnd::Host(from), CopyEnd::Guest { machine, path }) => Ok(Self::Upload {
                from,
                machine,
                to: path,
            }),
            (CopyEnd::Guest { machine, path }, CopyEnd::Host(to)) => Ok(Self::Download {
                machine,
                from: path,
                to,
            }),
            (CopyEnd::Host(_), CopyEnd::Host(_)) => Err(CopyError::BothHost),
            (CopyEnd::Guest { .. }, CopyEnd::Guest { .. }) => Err(CopyError::BothGuest),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_ends_parse_guest_and_host_sides() {
        assert_eq!(
            "dev:/tmp/a:b".parse::<CopyEnd>().unwrap(),
            CopyEnd::Guest {
                machine: "dev".parse().unwrap(),
                path: "/tmp/a:b".parse().unwrap()
            }
        );
        for host in ["notes.txt", "./a:b", "/Users/x/a:b", "dir/file"] {
            assert_eq!(
                host.parse::<CopyEnd>().unwrap(),
                CopyEnd::Host(host.into()),
                "{host}"
            );
        }
        assert!(matches!(
            "dev:tmp/a".parse::<CopyEnd>(),
            Err(CopyEndError::Path(..))
        ));
        assert!(matches!(
            "dev:/tmp/../a".parse::<CopyEnd>(),
            Err(CopyEndError::Path(..))
        ));
        assert!(matches!(
            ":/tmp/a".parse::<CopyEnd>(),
            Err(CopyEndError::Machine(..))
        ));
        assert_eq!("".parse::<CopyEnd>(), Err(CopyEndError::Empty));
    }

    #[test]
    fn a_copy_has_exactly_one_guest_side() {
        let guest = || "dev:/tmp/a".parse::<CopyEnd>().unwrap();
        let host = || CopyEnd::Host("a".into());
        assert!(matches!(
            Copy::between(host(), guest()),
            Ok(Copy::Upload { .. })
        ));
        assert!(matches!(
            Copy::between(guest(), host()),
            Ok(Copy::Download { .. })
        ));
        assert_eq!(Copy::between(host(), host()), Err(CopyError::BothHost));
        assert_eq!(Copy::between(guest(), guest()), Err(CopyError::BothGuest));
    }
}
