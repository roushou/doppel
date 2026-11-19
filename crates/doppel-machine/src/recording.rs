//! A session's recordings, as a console replays them: which there are, and
//! for one, its videos with where each sits in the session, and its trace
//! of calls. Names that aren't a recording's or a video's never parse.

use std::fmt;
use std::str::FromStr;

use crate::activity::SessionId;
use crate::machine::MachineName;
use doppel_desktop::geometry::Size;

/// A recording's name: the folder `recording` makes for a session, when the
/// session started (UTC, to the second) and its id, as
/// `2026-11-14T06-15-42Z-s1`. Its text is safe as a single path component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RecordingId {
    /// Seconds since 1970-01-01 UTC.
    started: u64,
    session: SessionId,
}

/// A name that isn't a recording's or a video's.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} doesn't name a recording or one of its videos")]
pub struct NotARecording(String);

impl RecordingId {
    #[must_use]
    pub const fn new(started_unix_seconds: u64, session: SessionId) -> Self {
        Self {
            started: started_unix_seconds,
            session,
        }
    }

    #[must_use]
    pub const fn started_unix_seconds(self) -> u64 {
        self.started
    }

    #[must_use]
    pub const fn session(self) -> SessionId {
        self.session
    }
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's
/// `days_from_civil`), for years from 1970.
fn days_from_civil(year: u64, month: u64, day: u64) -> u64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year / 400;
    let yoe = year - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of a day since 1970-01-01 (Howard Hinnant's
/// `civil_from_days`).
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

impl fmt::Display for RecordingId {
    /// `YYYY-MM-DDTHH-MM-SSZ-sN`: sorts by time, and has no colon, which
    /// Finder shows as a slash.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (days, of_day) = (self.started / 86_400, self.started % 86_400);
        let (year, month, day) = civil_from_days(days);
        write!(
            f,
            "{year:04}-{month:02}-{day:02}T{:02}-{:02}-{:02}Z-s{}",
            of_day / 3_600,
            of_day / 60 % 60,
            of_day % 60,
            self.session.get()
        )
    }
}

impl FromStr for RecordingId {
    type Err = NotARecording;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let wrong = || NotARecording(text.to_owned());
        // 2026-11-14T06-15-42Z-s1
        let (stamp, session) = text.split_once("Z-s").ok_or_else(wrong)?;
        let digits = |part: &str, len: usize| -> Result<u64, NotARecording> {
            if part.len() == len && part.bytes().all(|b| b.is_ascii_digit()) {
                part.parse().map_err(|_| wrong())
            } else {
                Err(wrong())
            }
        };
        let (date, time) = stamp.split_once('T').ok_or_else(wrong)?;
        let date: Vec<&str> = date.split('-').collect();
        let time: Vec<&str> = time.split('-').collect();
        let ([year, month, day], [hour, minute, second]) = (date.as_slice(), time.as_slice())
        else {
            return Err(wrong());
        };
        let (year, month, day) = (digits(year, 4)?, digits(month, 2)?, digits(day, 2)?);
        let (hour, minute, second) = (digits(hour, 2)?, digits(minute, 2)?, digits(second, 2)?);
        let in_range = year >= 1970
            && (1..=12).contains(&month)
            && (1..=31).contains(&day)
            && hour < 24
            && minute < 60
            && second < 60;
        if !in_range || session.is_empty() || !session.bytes().all(|b| b.is_ascii_digit()) {
            return Err(wrong());
        }
        let id = Self {
            started: days_from_civil(year, month, day) * 86_400
                + hour * 3_600
                + minute * 60
                + second,
            session: SessionId::from(session.parse::<u64>().map_err(|_| wrong())?),
        };
        // A day past its month's end (Feb 30) would name another date.
        if id.to_string() == text {
            Ok(id)
        } else {
            Err(wrong())
        }
    }
}

/// A recording's video: `{machine}-{NNN}.mp4`, the machine's NNNth VMM run
/// in the session. Its text is safe as a single path component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFile {
    machine: MachineName,
    number: u16,
}

impl VideoFile {
    pub fn new(machine: MachineName, number: u16) -> Result<Self, NotARecording> {
        if (1..=999).contains(&number) {
            Ok(Self { machine, number })
        } else {
            Err(NotARecording(format!("{machine}-{number}.mp4")))
        }
    }

    #[must_use]
    pub const fn machine(&self) -> &MachineName {
        &self.machine
    }
}

impl VideoFile {
    /// Its name without the extension (`live-001`), as the video chips
    /// show it.
    #[must_use]
    pub fn stem(&self) -> String {
        format!("{}-{:03}", self.machine, self.number)
    }
}

impl fmt::Display for VideoFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{:03}.mp4", self.machine, self.number)
    }
}

impl FromStr for VideoFile {
    type Err = NotARecording;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let wrong = || NotARecording(text.to_owned());
        let stem = text.strip_suffix(".mp4").ok_or_else(wrong)?;
        let (machine, number) = stem.rsplit_once('-').ok_or_else(wrong)?;
        if number.len() != 3 || !number.bytes().all(|b| b.is_ascii_digit()) {
            return Err(wrong());
        }
        let machine: MachineName = machine.parse().map_err(|_| wrong())?;
        Self::new(machine, number.parse().map_err(|_| wrong())?)
    }
}

/// A recording, as listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingSummary {
    pub id: RecordingId,
    /// When its session started, in seconds since 1970 (UTC).
    pub started_unix_seconds: i64,
    pub session: SessionId,
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
    pub refs: Option<Box<doppel_desktop::ax::refs::SnapshotRefs>>,
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
    /// A person at a console, who had control.
    Person,
    /// One this build doesn't know, as sent.
    Unknown(i32),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recording_s_name_reads_back_as_written() {
        let id = RecordingId::new(1_794_636_942, SessionId::from(1));
        assert_eq!(id.to_string(), "2026-11-14T06-15-42Z-s1");
        assert_eq!("2026-11-14T06-15-42Z-s1".parse::<RecordingId>(), Ok(id));
        let leap = RecordingId::new(951_782_400, SessionId::from(12));
        assert_eq!(leap.to_string(), "2000-02-29T00-00-00Z-s12");
        assert_eq!(leap.to_string().parse::<RecordingId>(), Ok(leap));
    }

    #[test]
    fn a_name_that_isn_t_a_recording_s_is_refused() {
        for text in [
            "",
            "..",
            "notes",
            "2026-11-14T06-15-42Z",
            "2026-11-14T06-15-42Z-s",
            "2026-11-14T06-15-42Z-s1/..",
            "2026-11-14T06-15-42Z-s-1",
            "2026-02-30T00-00-00Z-s1",
            "2026-13-01T00-00-00Z-s1",
            "2026-11-14T24-00-00Z-s1",
            "26-11-14T06-15-42Z-s1",
        ] {
            assert!(text.parse::<RecordingId>().is_err(), "{text:?}");
        }
    }

    #[test]
    fn a_video_s_name_is_its_machine_and_run() {
        let video: VideoFile = "live-live-cp-4-001.mp4".parse().unwrap();
        assert_eq!(video.machine().as_str(), "live-live-cp-4");
        assert_eq!(video.to_string(), "live-live-cp-4-001.mp4");
        for text in [
            "live-001.vtt",
            "live-1.mp4",
            "live-000.mp4",
            "-001.mp4",
            "../live-001.mp4",
            "live/x-001.mp4",
            "trace.jsonl",
        ] {
            assert!(text.parse::<VideoFile>().is_err(), "{text:?}");
        }
    }
}
