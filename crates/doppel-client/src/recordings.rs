//! The daemon's recordings, as replay needs them: which there are, and for
//! one, its videos with where each sits in the session and its trace.
//! Parsed here, once: names that aren't a recording's or a video's, and
//! lines that can't be read, never reach the caller as if they were.

use doppel_machine::geometry::Size;
use std::fmt;
use std::str::FromStr;

use doppel_machine::machine::MachineName;
use doppel_protocol::pb::daemon::v1 as daemon;

/// A recording's name, as the daemon lists it (`2026-11-14T06-15-42Z-s1`).
/// Only ASCII letters, digits and `-`: safe as a single path component.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RecordingId(String);

/// A video's file name in its recording's folder (`live-001.mp4`): a
/// machine's name, a dash, digits, `.mp4`. Safe as a single path component.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VideoFile(String);

/// A name that isn't a recording's or a video's.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} doesn't name a recording or one of its videos")]
pub struct NotARecording(String);

impl FromStr for RecordingId {
    type Err = NotARecording;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let valid = !text.is_empty()
            && text.len() <= 64
            && !text.starts_with('-')
            && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if valid {
            Ok(Self(text.to_owned()))
        } else {
            Err(NotARecording(text.to_owned()))
        }
    }
}

impl FromStr for VideoFile {
    type Err = NotARecording;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let wrong = || NotARecording(text.to_owned());
        let stem = text.strip_suffix(".mp4").ok_or_else(wrong)?;
        let (machine, number) = stem.rsplit_once('-').ok_or_else(wrong)?;
        if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            return Err(wrong());
        }
        machine.parse::<MachineName>().map_err(|_| wrong())?;
        Ok(Self(text.to_owned()))
    }
}

impl RecordingId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl VideoFile {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RecordingId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for VideoFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A recording, as listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingSummary {
    pub id: RecordingId,
    /// When its session started, in seconds since 1970 (UTC).
    pub started_unix_seconds: i64,
    pub session: u64,
    /// The machines it has videos of, in the order they started.
    pub machines: Vec<MachineName>,
    pub videos: u32,
    /// Lines in its trace.
    pub calls: u32,
    /// When its last traced call ended, in ms from the session's start.
    pub traced_ms: u64,
}

/// One recording, opened.
#[derive(Debug, Clone, PartialEq)]
pub struct Recording {
    pub id: RecordingId,
    /// In the order they started.
    pub videos: Vec<RecordingVideo>,
    /// In the order the trace file has them.
    pub trace: Vec<TraceLine>,
}

/// A video of one machine's run in a recording.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordingVideo {
    pub file: VideoFile,
    pub machine: MachineName,
    /// When in the session it starts, in ms; `None` for a video recorded
    /// before that was kept, which can't be lined up with the trace.
    pub start_ms: Option<u64>,
    /// Its display's size in points, the space trace targets are in.
    pub points: Option<Size>,
}

/// A line of a trace.
#[derive(Debug, Clone, PartialEq)]
pub enum TraceLine {
    Call(TraceCall),
    /// A line that can't be read, as it is.
    Unreadable(String),
}

/// A request, or a stretch a person had control, as the trace has it.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceCall {
    /// From the session's start.
    pub start_ms: u64,
    pub end_ms: u64,
    /// The machine it named, if it named one.
    pub machine: Option<MachineName>,
    /// Its caption.
    pub call: String,
    pub outcome: TraceOutcome,
    pub error: Option<String>,
    /// How long it waited for a person to hand back, or a resume, first.
    pub waited_ms: Option<u64>,
    /// How long it then waited in its machine's line.
    pub queued_ms: Option<u64>,
    pub by: TraceActor,
    /// What it did on the screen and where, when the trace kept it.
    pub acted: Option<Box<crate::activity::OnScreen>>,
    /// Its line in the trace, from 0, when the trace kept the snapshot it
    /// saw: [`crate::Client::seen_snapshot`] reads it.
    pub seen: Option<u32>,
    /// What the session's client named the elements of that snapshot.
    pub refs: Option<Box<doppel_machine::ax::refs::SnapshotRefs>>,
    /// What it read off the screen by text recognition.
    pub read: Option<Box<crate::TextRead>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceOutcome {
    Done,
    Failed,
    TimedOut,
    Refused,
    /// One this build doesn't know, as sent.
    Unknown(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceActor {
    /// One of the session's own requests.
    Session,
    /// A person at a viewer, who had control.
    Person,
    /// One this build doesn't know, as sent.
    Unknown(i32),
}

impl RecordingSummary {
    /// `None` for an entry whose id or machines aren't names.
    pub(crate) fn from_wire(summary: daemon::RecordingSummary) -> Option<Self> {
        Some(Self {
            id: summary.id.parse().ok()?,
            started_unix_seconds: summary.started_unix_seconds,
            session: summary.session,
            machines: summary
                .machines
                .iter()
                .map(|name| name.parse().ok())
                .collect::<Option<_>>()?,
            videos: summary.videos,
            calls: summary.calls,
            traced_ms: summary.traced_ms,
        })
    }
}

impl Recording {
    /// `None` when its id isn't one; videos whose names aren't are left
    /// out, and trace lines that can't be read are kept as they are.
    pub(crate) fn from_wire(recording: daemon::Recording) -> Option<Self> {
        Some(Self {
            id: recording.id.parse().ok()?,
            videos: recording
                .videos
                .into_iter()
                .filter_map(|video| {
                    Some(RecordingVideo {
                        file: video.file.parse().ok()?,
                        machine: video.machine.parse().ok()?,
                        start_ms: video.start_ms,
                        points: video
                            .display_points
                            .and_then(|size| Size::try_from(size).ok()),
                    })
                })
                .collect(),
            trace: recording
                .trace
                .into_iter()
                .enumerate()
                .map(|(line, traced)| TraceLine::from_wire(traced, u32::try_from(line).ok()))
                .collect(),
        })
    }
}

impl TraceLine {
    /// Line `index` of a trace.
    fn from_wire(line: daemon::TraceLine, index: Option<u32>) -> Self {
        use daemon::trace_line::Line;
        match line.line {
            Some(Line::Call(call)) => {
                let machine = match call.machine {
                    Some(raw) => match raw.parse() {
                        Ok(name) => Some(name),
                        Err(_) => return Self::Unreadable(format!("a call on {raw:?}")),
                    },
                    None => None,
                };
                Self::Call(TraceCall {
                    start_ms: call.start_ms,
                    end_ms: call.end_ms,
                    machine,
                    call: call.call,
                    outcome: match daemon::TraceOutcome::try_from(call.outcome) {
                        Ok(daemon::TraceOutcome::Done) => TraceOutcome::Done,
                        Ok(daemon::TraceOutcome::Failed) => TraceOutcome::Failed,
                        Ok(daemon::TraceOutcome::TimedOut) => TraceOutcome::TimedOut,
                        Ok(daemon::TraceOutcome::Refused) => TraceOutcome::Refused,
                        Ok(daemon::TraceOutcome::Unspecified) | Err(_) => {
                            TraceOutcome::Unknown(call.outcome)
                        }
                    },
                    error: call.error,
                    waited_ms: call.waited_ms,
                    queued_ms: call.queued_ms,
                    by: match daemon::TraceActor::try_from(call.by) {
                        Ok(daemon::TraceActor::Session) => TraceActor::Session,
                        Ok(daemon::TraceActor::Person) => TraceActor::Person,
                        Ok(daemon::TraceActor::Unspecified) | Err(_) => {
                            TraceActor::Unknown(call.by)
                        }
                    },
                    acted: call
                        .acted
                        .map(|acted| Box::new(crate::activity::OnScreen::from_wire(acted))),
                    seen: index.filter(|_| call.seen),
                    refs: call
                        .refs
                        .and_then(|refs| refs.try_into().ok())
                        .map(Box::new),
                    read: call.read.and_then(crate::TextRead::from_wire).map(Box::new),
                })
            }
            Some(Line::Unreadable(raw)) => Self::Unreadable(raw),
            None => Self::Unreadable(String::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_single_path_components() {
        assert!("2026-11-14T06-15-42Z-s1".parse::<RecordingId>().is_ok());
        for bad in ["", "..", "a/b", "-x", "a b"] {
            assert!(bad.parse::<RecordingId>().is_err(), "{bad:?}");
        }
        assert!("live-001.mp4".parse::<VideoFile>().is_ok());
        for bad in [
            "live.mp4",
            "../x-001.mp4",
            "live-001.mov",
            "Live-001.mp4",
            "live-.mp4",
        ] {
            assert!(bad.parse::<VideoFile>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_recording_keeps_what_it_can_read() {
        let recording = Recording::from_wire(daemon::Recording {
            id: "2026-11-14T06-15-42Z-s1".into(),
            videos: vec![
                daemon::RecordingVideo {
                    file: "live-001.mp4".into(),
                    machine: "live".into(),
                    start_ms: Some(250),
                    display_points: Some(doppel_protocol::pb::common::v1::Size {
                        width: 1280.0,
                        height: 800.0,
                    }),
                },
                daemon::RecordingVideo {
                    file: "../etc".into(),
                    machine: "live".into(),
                    start_ms: None,
                    display_points: None,
                },
            ],
            trace: vec![
                daemon::TraceLine {
                    line: Some(daemon::trace_line::Line::Call(daemon::TraceCall {
                        start_ms: 10,
                        end_ms: 20,
                        machine: Some("live".into()),
                        call: "click".into(),
                        outcome: 99,
                        by: daemon::TraceActor::Person as i32,
                        ..daemon::TraceCall::default()
                    })),
                },
                daemon::TraceLine { line: None },
            ],
        })
        .unwrap();
        assert_eq!(recording.videos.len(), 1, "a bad file name is left out");
        assert_eq!(recording.videos[0].points, Size::new(1280.0, 800.0).ok());
        let TraceLine::Call(call) = &recording.trace[0] else {
            panic!("a call");
        };
        assert_eq!(call.outcome, TraceOutcome::Unknown(99));
        assert_eq!(call.by, TraceActor::Person);
        assert_eq!(recording.trace[1], TraceLine::Unreadable(String::new()));
    }
}
