//! The daemon's recordings in the console's words, and where their videos
//! are on this Mac. The client has already parsed the names; the console's
//! names coming back are parsed again here, on the way in, before they
//! become a path.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use doppel_console::{DaemonError, RecordingId, RecordingSummary, VideoName, answers};

use super::port::local;

/// Where the daemon's recordings are, for replay to read their videos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Videos {
    /// In the store of this Mac's daemon.
    Here,
    /// On the Mac a forwarded daemon runs on: out of the console's reach.
    Elsewhere,
}

impl Videos {
    /// Where `file` of recording `id` is, under the store's `data_root`.
    ///
    /// # Errors
    /// [`DaemonError::Refused`]: the videos are on another Mac, or a name
    /// isn't a recording's or a video's.
    pub(crate) fn path(
        self,
        data_root: impl FnOnce() -> Result<PathBuf, DaemonError>,
        id: &RecordingId,
        file: &VideoName,
    ) -> Result<PathBuf, DaemonError> {
        match self {
            Self::Here => {
                let refused = |error: doppel_client::NotARecording| {
                    DaemonError::Refused(error.to_string())
                };
                let id: doppel_client::RecordingId = id.as_str().parse().map_err(refused)?;
                let file: doppel_client::VideoFile = file.as_str().parse().map_err(refused)?;
                Ok(Self::under(&data_root()?, &id, &file))
            }
            Self::Elsewhere => Err(DaemonError::Refused(
                "its video is on the Mac the daemon runs on: replay it in the web console, or in `doppel view` there".into(),
            )),
        }
    }

    fn under(
        data_root: &Path,
        id: &doppel_client::RecordingId,
        file: &doppel_client::VideoFile,
    ) -> PathBuf {
        data_root
            .join("recordings")
            .join(id.as_str())
            .join(file.as_str())
    }
}

pub(super) fn summary(summary: doppel_client::RecordingSummary) -> RecordingSummary {
    let started = u64::try_from(summary.started_unix_seconds)
        .map_or(SystemTime::UNIX_EPOCH, |seconds| {
            SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
        });
    answers::summary(summary, local(started))
}
