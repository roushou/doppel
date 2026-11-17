//! A session's recordings, as a viewer replays them: which there are, and
//! for one, its videos with where each sits in the session, and its trace
//! of calls. Names that aren't a recording's or a video's never parse.

use std::fmt;
use std::str::FromStr;

use crate::geometry::Size;
use crate::machine::MachineName;

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
    /// saw: the daemon reads it back by that line.
    pub seen: Option<u32>,
    /// What the session's client named the elements of that snapshot.
    pub refs: Option<Box<crate::ax::refs::SnapshotRefs>>,
    /// What it read off the screen by text recognition.
    pub read: Option<Box<crate::activity::TextRead>>,
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
}
