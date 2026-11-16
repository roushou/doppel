//! Paths inside the guest, as the host names them.

use std::fmt;
use std::str::FromStr;

/// An absolute guest path: starts with `/`, has no NUL, no `.` or `..`
/// components and no empty components (so `/a//b` and `/a/` are refused).
/// The root `/` itself is valid. Not resolved against the guest's
/// filesystem: symlinks are the guest's business.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct GuestPath(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GuestPathError {
    #[error("guest paths must be absolute (start with /): {0:?}")]
    Relative(String),
    #[error("guest paths can't contain NUL")]
    Nul,
    #[error("guest paths can't contain `.`, `..` or empty components: {0:?}")]
    NotNormal(String),
}

impl FromStr for GuestPath {
    type Err = GuestPathError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.contains('\0') {
            return Err(GuestPathError::Nul);
        }
        let Some(rest) = s.strip_prefix('/') else {
            return Err(GuestPathError::Relative(s.to_owned()));
        };
        if !rest.is_empty()
            && rest
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(GuestPathError::NotNormal(s.to_owned()));
        }
        Ok(Self(s.to_owned()))
    }
}

impl GuestPath {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `self` followed by a relative path; the result is checked like any other.
    ///
    /// # Errors
    /// [`GuestPathError`] if `relative` makes it non-normal (`..`, `//`, …).
    pub fn join(&self, relative: &str) -> Result<Self, GuestPathError> {
        let base = self.0.trim_end_matches('/');
        format!("{base}/{relative}").parse()
    }
}

impl fmt::Display for GuestPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for GuestPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GuestPath({:?})", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_checks_the_result() {
        let home: GuestPath = "/Users/a b".parse().unwrap();
        assert_eq!(
            home.join("Library/x").unwrap().as_str(),
            "/Users/a b/Library/x"
        );
        let root: GuestPath = "/".parse().unwrap();
        assert_eq!(root.join("tmp").unwrap().as_str(), "/tmp");
        assert!(home.join("../etc").is_err());
        assert!(home.join("a//b").is_err());
        assert!(home.join("").is_err());
    }

    #[test]
    fn absolute_normal_paths_only() {
        for good in [
            "/",
            "/usr/bin/sw_vers",
            "/Users/x/My Files/a.txt",
            "/a/.hidden",
        ] {
            assert_eq!(good.parse::<GuestPath>().unwrap().as_str(), good);
        }
        assert!(matches!(
            "usr/bin".parse::<GuestPath>(),
            Err(GuestPathError::Relative(_))
        ));
        assert!(matches!(
            "".parse::<GuestPath>(),
            Err(GuestPathError::Relative(_))
        ));
        assert_eq!("/a\0b".parse::<GuestPath>(), Err(GuestPathError::Nul));
        for bad in ["/a/../b", "/a/./b", "/a//b", "/a/", "/.."] {
            assert!(
                matches!(bad.parse::<GuestPath>(), Err(GuestPathError::NotNormal(_))),
                "{bad}"
            );
        }
    }
}
