//! Which macOS restore image to download, and where it goes.
//!
//! `doppel-vmm latest-image` asks Apple's catalog for the newest image this
//! Mac can install and prints `BUILD URL`; this parses that line once. The
//! image is downloaded beside the machines, under its build's name, so an
//! interrupted download resumes into the same file.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// A macOS build, as Apple names it (`26B131`): letters and digits only,
/// so it is safe as a file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacOsBuild(String);

impl FromStr for MacOsBuild {
    type Err = RestoreImageError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if (1..=32).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric()) {
            Ok(Self(s.to_owned()))
        } else {
            Err(RestoreImageError::Build(s.to_owned()))
        }
    }
}

impl fmt::Display for MacOsBuild {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a restore image is: an `https` URL with no whitespace, so it is
/// passed to the downloader as one argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageUrl(String);

impl FromStr for ImageUrl {
    type Err = RestoreImageError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let rest = s.strip_prefix("https://").unwrap_or_default();
        if !rest.is_empty() && !s.chars().any(|c| c.is_whitespace() || c.is_control()) {
            Ok(Self(s.to_owned()))
        } else {
            Err(RestoreImageError::Url(s.to_owned()))
        }
    }
}

impl ImageUrl {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ImageUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RestoreImageError {
    #[error("{0:?} is not a macOS build")]
    Build(String),
    #[error("{0:?} is not an https URL")]
    Url(String),
    #[error("doppel-vmm latest-image printed {0:?}, not `BUILD URL`")]
    Line(String),
}

/// The newest restore image this Mac can install, as the catalog names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestImage {
    pub build: MacOsBuild,
    pub url: ImageUrl,
}

impl FromStr for LatestImage {
    type Err = RestoreImageError;
    /// `BUILD URL`, as `doppel-vmm latest-image` prints it.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let line = s.trim();
        let (build, url) = line
            .split_once(' ')
            .ok_or_else(|| RestoreImageError::Line(line.to_owned()))?;
        Ok(Self {
            build: build.parse()?,
            url: url.parse()?,
        })
    }
}

impl LatestImage {
    /// Where it's downloaded to, in `dir`: complete only once renamed from
    /// [`Self::partial`].
    #[must_use]
    pub fn file(&self, dir: &Path) -> PathBuf {
        dir.join(format!("{}.ipsw", self.build))
    }

    /// Locked while a download into [`Self::partial`] runs, by the
    /// downloader itself: two never write the same file.
    #[must_use]
    pub fn lock(&self, dir: &Path) -> PathBuf {
        dir.join(format!("{}.ipsw.lock", self.build))
    }

    /// Where the download goes while it runs, and resumes from.
    #[must_use]
    pub fn partial(&self, dir: &Path) -> PathBuf {
        dir.join(format!("{}.ipsw.partial", self.build))
    }
}

/// How far a download is: bytes so far, of the total when the server said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Downloaded {
    pub bytes: u64,
    pub of: Option<u64>,
}

impl Downloaded {
    /// The share done, from 0 to 1, when the total is known.
    #[must_use]
    pub fn fraction(self) -> Option<f64> {
        match self.of {
            Some(total) if total > 0 => Some((self.bytes.min(total) as f64) / (total as f64)),
            Some(_) | None => None,
        }
    }
}

impl fmt::Display for Downloaded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const GB: f64 = 1e9;
        let got = self.bytes as f64 / GB;
        match (self.of, self.fraction()) {
            (Some(total), Some(fraction)) => write!(
                f,
                "{got:.1} of {:.1} GB ({:.0}%)",
                total as f64 / GB,
                fraction * 100.0
            ),
            (Some(_) | None, _) => write!(f, "{got:.1} GB"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_vmm_line() {
        let image: LatestImage =
            "26B131 https://updates.cdn-apple.com/x/UniversalMac_27.2.1_26B131_Restore.ipsw\n"
                .parse()
                .unwrap();
        assert_eq!(image.build.to_string(), "26B131");
        assert_eq!(image.file(Path::new("/d")), PathBuf::from("/d/26B131.ipsw"));
        assert_eq!(
            image.partial(Path::new("/d")),
            PathBuf::from("/d/26B131.ipsw.partial")
        );
    }

    #[test]
    fn refuses_what_isnt_a_build_and_an_https_url() {
        for line in [
            "",
            "26B131",
            "26B131 http://example.com/a.ipsw",
            "26B131 https://",
            "../x https://example.com/a.ipsw",
            "26B131 https://example.com/a b.ipsw",
        ] {
            assert!(line.parse::<LatestImage>().is_err(), "{line:?}");
        }
    }

    #[test]
    fn progress_is_a_share_only_with_a_total() {
        let half = Downloaded {
            bytes: 9_000_000_000,
            of: Some(18_000_000_000),
        };
        assert_eq!(half.fraction(), Some(0.5));
        assert_eq!(half.to_string(), "9.0 of 18.0 GB (50%)");
        let unknown = Downloaded {
            bytes: 1_500_000_000,
            of: None,
        };
        assert_eq!(unknown.fraction(), None);
        assert_eq!(unknown.to_string(), "1.5 GB");
    }
}
