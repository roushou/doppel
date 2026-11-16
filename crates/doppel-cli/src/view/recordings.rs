//! The daemon's recordings in the viewer's words, and where their videos
//! are on this Mac. The client has already parsed the names; the viewer's
//! names coming back are parsed again here, on the way in, before they
//! become a path.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use doppel_viewer::{
    Actor, DaemonError, Recording, RecordingId, RecordingSummary, TraceCall, TraceLine,
    TraceOutcome, Video, VideoName,
};

use super::port::{local, on_screen};

/// Where the daemon's recordings are, for replay to read their videos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Videos {
    /// In the store of this Mac's daemon.
    Here,
    /// On the Mac a forwarded daemon runs on: out of the viewer's reach.
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
                "its video is on the Mac the daemon runs on: replay it in the browser viewer, or in `doppel view` there".into(),
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
    RecordingSummary {
        id: RecordingId::new(summary.id.to_string()),
        started: local(started),
        session: summary.session,
        machines: summary.machines,
        videos: summary.videos,
        calls: summary.calls,
        traced_ms: summary.traced_ms,
    }
}

pub(super) fn recording(recording: doppel_client::Recording) -> Recording {
    Recording {
        id: RecordingId::new(recording.id.to_string()),
        videos: recording
            .videos
            .into_iter()
            .map(|video| Video {
                file: VideoName::new(video.file.to_string()),
                machine: video.machine,
                start_ms: video.start_ms,
                points: video.points,
            })
            .collect(),
        trace: recording.trace.into_iter().map(line).collect(),
    }
}

fn line(line: doppel_client::TraceLine) -> TraceLine {
    match line {
        doppel_client::TraceLine::Call(call) => {
            let (acted, callout) = match call.acted {
                Some(acted) => {
                    let (gesture, target, callout) = on_screen(*acted);
                    (Some((gesture, target)), callout)
                }
                None => (None, None),
            };
            TraceLine::Call(Box::new(TraceCall {
                start_ms: call.start_ms,
                end_ms: call.end_ms,
                machine: call.machine,
                call: call.call,
                outcome: match call.outcome {
                    doppel_client::TraceOutcome::Done => TraceOutcome::Done,
                    doppel_client::TraceOutcome::Failed => TraceOutcome::Failed,
                    doppel_client::TraceOutcome::TimedOut => TraceOutcome::TimedOut,
                    doppel_client::TraceOutcome::Refused => TraceOutcome::Refused,
                    doppel_client::TraceOutcome::Unknown(raw) => TraceOutcome::Unknown(raw),
                },
                error: call.error,
                waited_ms: call.waited_ms,
                queued_ms: call.queued_ms,
                by: match call.by {
                    doppel_client::TraceActor::Session => Actor::Session,
                    doppel_client::TraceActor::Person => Actor::Person,
                    doppel_client::TraceActor::Unknown(raw) => Actor::Unknown(raw),
                },
                acted,
                callout,
                seen: call.seen,
                refs: call.refs.map(|refs| std::sync::Arc::new(*refs)),
                read: call
                    .read
                    .map(|read| std::sync::Arc::new(super::port::text_read(&read))),
            }))
        }
        doppel_client::TraceLine::Unreadable(raw) => TraceLine::Unreadable(raw),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> Result<PathBuf, DaemonError> {
        Ok(PathBuf::from("/store"))
    }

    #[test]
    fn a_video_is_found_in_its_recordings_folder_here_only() {
        let id = RecordingId::new("2026-11-14T06-15-42Z-s1".into());
        let file = VideoName::new("live-001.mp4".into());
        assert_eq!(
            Videos::Here.path(root, &id, &file),
            Ok(PathBuf::from(
                "/store/recordings/2026-11-14T06-15-42Z-s1/live-001.mp4"
            ))
        );
        assert!(matches!(
            Videos::Elsewhere.path(root, &id, &file),
            Err(DaemonError::Refused(_))
        ));
    }

    #[test]
    fn names_that_would_leave_the_folder_are_refused() {
        let file = VideoName::new("live-001.mp4".into());
        for bad in ["..", "a/b", ""] {
            let id = RecordingId::new(bad.into());
            assert!(
                matches!(
                    Videos::Here.path(root, &id, &file),
                    Err(DaemonError::Refused(_))
                ),
                "{bad:?}"
            );
        }
        let id = RecordingId::new("r".into());
        let file = VideoName::new("../../etc/x-1.mp4".into());
        assert!(Videos::Here.path(root, &id, &file).is_err());
    }

    #[test]
    fn a_traced_call_keeps_who_made_it_and_what_it_did() {
        let TraceLine::Call(call) =
            line(doppel_client::TraceLine::Call(doppel_client::TraceCall {
                start_ms: 10,
                end_ms: 20,
                machine: None,
                call: "click OK".into(),
                outcome: doppel_client::TraceOutcome::Unknown(9),
                error: None,
                waited_ms: Some(5),
                queued_ms: None,
                seen: None,
                refs: None,
                read: None,
                by: doppel_client::TraceActor::Person,
                acted: Some(Box::new(doppel_client::OnScreen {
                    gesture: doppel_client::Gesture::Click,
                    target: Some(doppel_client::Target::Point(
                        doppel_machine::geometry::Point::new(1.0, 2.0).unwrap(),
                    )),
                    callout: None,
                })),
            }))
        else {
            panic!("a call");
        };
        assert_eq!(call.outcome, TraceOutcome::Unknown(9));
        assert_eq!(call.by, Actor::Person);
        assert_eq!(
            call.acted,
            Some((
                doppel_viewer::Gesture::Click,
                doppel_viewer::Target::Point(
                    doppel_machine::geometry::Point::new(1.0, 2.0).unwrap()
                )
            ))
        );
    }
}
