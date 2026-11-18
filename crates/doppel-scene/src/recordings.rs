//! A recording, as replay needs it: each video with where it sits in the
//! session, the trace's calls, and the arithmetic between a video's
//! playhead and the session's clock. Pure.

use doppel_desktop::geometry::Size;
use std::fmt;

use doppel_machine::machine::MachineName;

use crate::timeline::{Call, CallKey, Gesture, Moment, Seen, Target};

/// A recording's name, as the daemon lists it (`2026-11-14T06-15-42Z-s1`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RecordingId(String);

impl RecordingId {
    #[must_use]
    pub const fn new(name: String) -> Self {
        Self(name)
    }

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

/// A video's file name in its recording (`live-001.mp4`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VideoName(String);

impl VideoName {
    #[must_use]
    pub const fn new(name: String) -> Self {
        Self(name)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The name without its extension, as the video chips show it.
    pub fn stem(&self) -> &str {
        self.0.strip_suffix(".mp4").unwrap_or(&self.0)
    }
}

/// A recording, as listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingSummary {
    pub id: RecordingId,
    /// When its session started, on this Mac's clock.
    pub started: LocalTime,
    /// The session it recorded: its agent's hue.
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
    pub videos: Vec<Video>,
    /// In the order the trace file has them; see [`Recording::in_start_order`].
    pub trace: Vec<TraceLine>,
}

/// A video of one machine's run.
#[derive(Debug, Clone, PartialEq)]
pub struct Video {
    pub file: VideoName,
    pub machine: MachineName,
    /// When in the session it starts, in ms; `None` when that wasn't kept.
    pub start_ms: Option<u64>,
    /// Its display's size in points, the space targets are in.
    pub points: Option<Size>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TraceLine {
    Call(Box<TraceCall>),
    /// A line that can't be read, as it is.
    Unreadable(String),
}

/// A request, or a stretch a person had control, as the trace has it.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceCall {
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
    pub by: Actor,
    /// What it did on the screen and where, when the trace kept it.
    pub acted: Option<(Gesture, Target)>,
    /// What a mark call showed the people watching.
    pub callout: Option<std::sync::Arc<crate::timeline::Callout>>,
    /// Why the session's client made it, when it said.
    pub why: Option<String>,
    /// Its line in the trace, from 0, when the trace kept the snapshot it
    /// saw: [`crate::Daemon::seen_snapshot`] reads it.
    pub seen: Option<u32>,
    /// What the agent named the elements of that snapshot.
    pub refs: Option<std::sync::Arc<doppel_desktop::ax::refs::SnapshotRefs>>,
    /// What it read off the screen by text recognition.
    pub read: Option<std::sync::Arc<crate::timeline::TextRead>>,
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

/// Who a trace line is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    /// One of the session's own requests.
    Session,
    /// A person at a console, who had control.
    Person,
    Unknown(i32),
}

/// Long enough to read a call that took no time, as the captions show it.
const MIN_SHOWN_MS: u64 = 1000;

/// Where to seek to see a moment: a video, and seconds into it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seek {
    pub video: usize,
    pub seconds: f64,
}

impl TraceOutcome {
    /// What the trace's badge says; `None` when it went well.
    pub fn words(self) -> Option<String> {
        match self {
            Self::Done => None,
            Self::Failed => Some("failed".into()),
            Self::TimedOut => Some("timed out".into()),
            Self::Refused => Some("refused".into()),
            Self::Unknown(raw) => Some(format!("outcome {raw}")),
        }
    }
}

impl Recording {
    /// Calls by when they started (a stable sort: ties keep the file's
    /// order), then the lines that can't be read, in the file's order.
    #[must_use]
    pub fn in_start_order(mut self) -> Self {
        self.trace.sort_by_key(|line| match line {
            TraceLine::Call(call) => (0, call.start_ms),
            TraceLine::Unreadable(_) => (1, 0),
        });
        self
    }

    /// Whether any video can be lined up with the trace.
    pub fn aligned(&self) -> bool {
        self.videos.iter().any(|video| video.start_ms.is_some())
    }

    pub fn calls(&self) -> impl Iterator<Item = &TraceCall> {
        self.trace.iter().filter_map(|line| match line {
            TraceLine::Call(call) => Some(&**call),
            TraceLine::Unreadable(_) => None,
        })
    }

    /// How many machines its videos show.
    pub fn machine_count(&self) -> usize {
        let mut machines: Vec<&MachineName> =
            self.videos.iter().map(|video| &video.machine).collect();
        machines.sort();
        machines.dedup();
        machines.len()
    }

    /// The session's length as traced, at least a second.
    pub fn traced_ms(&self) -> u64 {
        self.calls()
            .map(|call| call.end_ms)
            .max()
            .unwrap_or(0)
            .max(1000)
    }

    /// The trace's lines under the session's time `at` on `machine`:
    /// calls on it, or on none, by their index.
    pub fn calls_at(&self, machine: &MachineName, at: f64) -> Vec<usize> {
        self.trace
            .iter()
            .enumerate()
            .filter_map(|(index, line)| match line {
                TraceLine::Call(call)
                    if call.machine.as_ref().is_none_or(|named| named == machine) =>
                {
                    let end = call.end_ms.max(call.start_ms + MIN_SHOWN_MS);
                    (call.start_ms as f64 <= at && at < end as f64).then_some(index)
                }
                TraceLine::Call(_) | TraceLine::Unreadable(_) => None,
            })
            .collect()
    }

    /// Where to seek to see the session's time `at` on `machine` (`None`:
    /// any machine).
    pub fn seek_at(&self, machine: Option<&MachineName>, at: f64) -> Option<Seek> {
        let mut best: Option<(u64, Seek)> = None;
        for (index, video) in self.videos.iter().enumerate() {
            let Some(start) = video.start_ms else {
                continue;
            };
            if start as f64 > at || machine.is_some_and(|machine| &video.machine != machine) {
                continue;
            }
            if best.is_none_or(|(best_start, _)| start >= best_start) {
                best = Some((
                    start,
                    Seek {
                        video: index,
                        seconds: (at - start as f64) / 1000.0,
                    },
                ));
            }
        }
        best.map(|(_, seek)| seek)
    }
}

impl RecordingSummary {
    /// Whether it has a video of `machine`; `None` asks for any.
    pub fn shows(&self, machine: Option<&MachineName>) -> bool {
        machine.is_none_or(|machine| self.machines.contains(machine))
    }

    /// The machines any of `listed` has videos of, by name.
    pub fn machines_of(listed: &[Self]) -> Vec<MachineName> {
        let mut machines: Vec<MachineName> = listed
            .iter()
            .flat_map(|summary| summary.machines.iter().cloned())
            .collect();
        machines.sort();
        machines.dedup();
        machines
    }
}

/// A session's length: seconds, then minutes and seconds.
pub fn length(ms: u64) -> String {
    let seconds = (ms + 500) / 1000;
    if seconds < 60 {
        format!("{seconds} s")
    } else {
        format!("{} min {} s", seconds / 60, seconds % 60)
    }
}

/// A time in the session, as the trace shows it: `m:ss.s`.
pub fn clock(ms: f64) -> String {
    let tenths = (ms.max(0.0) / 100.0).round() as u64;
    let minutes = tenths / 600;
    let rest = tenths % 600;
    format!("{minutes}:{:02}.{}", rest / 10, rest % 10)
}

/// Seconds, to the tenth: how long a call took or waited.
pub fn seconds(ms: u64) -> String {
    format!("{:.1} s", ms as f64 / 1000.0)
}

/// A local date and time, to the second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl LocalTime {
    /// Its time of day, as a row of the library shows it: `16:20`.
    pub fn time_of_day(&self) -> String {
        format!("{:02}:{:02}", self.hour, self.minute)
    }

    /// Days since 1970-01-01 of its date (the proleptic Gregorian calendar).
    fn day_number(&self) -> i64 {
        let (month, day) = (i64::from(self.month), i64::from(self.day));
        let year = i64::from(self.year) - i64::from(month <= 2);
        let era = year.div_euclid(400);
        let of_era = year - era * 400;
        let of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
        let of_era_day = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
        era * 146_097 + of_era_day - 719_468
    }
}

/// Which day a group of recordings is, relative to today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Day {
    Today,
    Yesterday,
    /// Another day: its date.
    On {
        year: i32,
        month: u8,
        day: u8,
        weekday: u8,
    },
}

impl Day {
    /// Its heading: `Today`, `Yesterday`, `Thu Nov 12`.
    pub fn name(self) -> String {
        const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        match self {
            Self::Today => "Today".into(),
            Self::Yesterday => "Yesterday".into(),
            Self::On {
                month,
                day,
                weekday,
                ..
            } => format!(
                "{} {} {day}",
                WEEKDAYS.get(usize::from(weekday)).unwrap_or(&""),
                MONTHS
                    .get(usize::from(month.saturating_sub(1)))
                    .unwrap_or(&"")
            ),
        }
    }
}

/// Recordings started on one day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayGroup<'a> {
    pub day: Day,
    pub recordings: Vec<&'a RecordingSummary>,
}

/// `listed` by the day each started, newest day and newest recording first.
pub fn by_day<'a>(
    listed: impl IntoIterator<Item = &'a RecordingSummary>,
    today: LocalTime,
) -> Vec<DayGroup<'a>> {
    let today = today.day_number();
    let mut days: Vec<(i64, Vec<&'a RecordingSummary>)> = Vec::new();
    for summary in listed {
        let number = summary.started.day_number();
        match days.iter_mut().find(|(day, _)| *day == number) {
            Some((_, group)) => group.push(summary),
            None => days.push((number, vec![summary])),
        }
    }
    days.sort_by(|(a, _), (b, _)| b.cmp(a));
    days.into_iter()
        .map(|(number, mut recordings)| {
            recordings.sort_by_key(|summary| {
                let at = summary.started;
                std::cmp::Reverse((at.hour, at.minute, at.second))
            });
            let day = match today - number {
                0 => Day::Today,
                1 => Day::Yesterday,
                _ => {
                    let first = recordings.first().map(|summary| summary.started);
                    let (year, month, day) =
                        first.map_or((0, 0, 0), |at| (at.year, at.month, at.day));
                    Day::On {
                        year,
                        month,
                        day,
                        // 1970-01-01 was a Thursday.
                        weekday: u8::try_from((number + 4).rem_euclid(7)).unwrap_or(0),
                    }
                }
            };
            DayGroup { day, recordings }
        })
        .collect()
}

/// A recording's calls on `machine` (and those naming none) as moments,
/// timed from the session's start.
pub fn from_trace(recording: &Recording, machine: &MachineName) -> Vec<Moment> {
    recording
        .calls()
        .filter(|call| call.machine.as_ref().is_none_or(|named| named == machine))
        .enumerate()
        .map(|(index, traced)| {
            let (gesture, target) = traced
                .acted
                .clone()
                .unwrap_or((Gesture::Other, Target::Nowhere));
            let callout = traced.callout.clone();
            Moment {
                start_ms: traced.start_ms as f64,
                end_ms: Some(traced.end_ms as f64),
                call: Call {
                    key: CallKey {
                        session: 0,
                        request: index as u64,
                    },
                    machine: machine.clone(),
                    client: String::new(),
                    caption: traced.call.clone(),
                    gesture,
                    target,
                    callout,
                    why: traced.why.clone(),
                },
                failed: match traced.outcome {
                    TraceOutcome::Done => None,
                    TraceOutcome::Failed
                    | TraceOutcome::TimedOut
                    | TraceOutcome::Refused
                    | TraceOutcome::Unknown(_) => Some(
                        traced
                            .error
                            .clone()
                            .or_else(|| traced.outcome.words())
                            .unwrap_or_default(),
                    ),
                },
                person: traced.by == Actor::Person,
                seen: traced.seen.map(Seen::Recorded),
                refs: traced.refs.clone(),
                read: traced.read.clone(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(raw: &str) -> MachineName {
        raw.parse().unwrap()
    }

    fn call(start_ms: u64, end_ms: u64, machine: Option<&str>, text: &str) -> TraceLine {
        TraceLine::Call(Box::new(TraceCall {
            why: None,
            start_ms,
            end_ms,
            machine: machine.map(name),
            call: text.into(),
            outcome: TraceOutcome::Done,
            error: None,
            waited_ms: None,
            queued_ms: None,
            seen: None,
            refs: None,
            read: None,
            by: Actor::Session,
            acted: None,
            callout: None,
        }))
    }

    fn video(file: &str, machine: &str, start_ms: Option<u64>) -> Video {
        Video {
            file: VideoName::new(file.into()),
            machine: name(machine),
            start_ms,
            points: None,
        }
    }

    fn recording(videos: Vec<Video>, trace: Vec<TraceLine>) -> Recording {
        Recording {
            id: RecordingId::new("r".into()),
            videos,
            trace,
        }
    }

    #[test]
    fn calls_are_listed_by_when_they_started() {
        let sorted = recording(
            Vec::new(),
            vec![
                call(110, 111, None, "status"),
                TraceLine::Unreadable("?".into()),
                call(4017, 4018, None, "type"),
                call(2013, 2014, None, "wait"),
                call(4017, 4018, None, "key"),
            ],
        )
        .in_start_order();
        let words: Vec<&str> = sorted
            .trace
            .iter()
            .map(|line| match line {
                TraceLine::Call(call) => call.call.as_str(),
                TraceLine::Unreadable(raw) => raw.as_str(),
            })
            .collect();
        assert_eq!(words, ["status", "wait", "type", "key", "?"]);
    }

    #[test]
    fn the_calls_under_the_playhead_are_on_the_videos_machine() {
        let trace = recording(
            Vec::new(),
            vec![
                call(100, 120, Some("dev"), "x"),
                call(500, 9000, Some("dev"), "x"),
                call(600, 700, Some("web"), "x"),
                call(800, 900, None, "x"),
                TraceLine::Unreadable(String::new()),
            ],
        );
        let dev = name("dev");
        // A quick call stays under the playhead for a second.
        assert_eq!(trace.calls_at(&dev, 110.0), [0]);
        assert_eq!(trace.calls_at(&dev, 850.0), [0, 1, 3]);
        assert_eq!(trace.calls_at(&dev, 1100.0), [1, 3]);
        assert_eq!(trace.calls_at(&dev, 1800.0), [1]);
        assert_eq!(trace.calls_at(&name("web"), 650.0), [2]);
        assert!(trace.calls_at(&dev, 9500.0).is_empty());
    }

    #[test]
    fn a_call_is_seen_in_the_latest_video_of_its_machine_that_had_started() {
        let rec = recording(
            vec![
                video("dev-001.mp4", "dev", Some(100)),
                video("web-001.mp4", "web", Some(200)),
                video("dev-002.mp4", "dev", Some(5000)),
                video("dev-003.mp4", "dev", None),
            ],
            Vec::new(),
        );
        let seek = |line: TraceLine| match line {
            TraceLine::Call(call) => rec.seek_at(call.machine.as_ref(), call.start_ms as f64),
            TraceLine::Unreadable(_) => None,
        };
        assert_eq!(
            seek(call(1100, 1200, Some("dev"), "x")),
            Some(Seek {
                video: 0,
                seconds: 1.0
            })
        );
        assert_eq!(
            seek(call(6000, 6100, Some("dev"), "x")).map(|s| s.video),
            Some(2)
        );
        assert_eq!(
            seek(call(1200, 1300, Some("web"), "x")).map(|s| s.video),
            Some(1)
        );
        // A call that named no machine: whichever video was showing.
        let none = seek(call(300, 400, None, "x")).unwrap();
        assert_eq!(none.video, 1);
        assert!((none.seconds - 0.1).abs() < 1e-9);
        assert_eq!(seek(call(50, 60, Some("dev"), "x")), None);
        assert_eq!(seek(call(1000, 1100, Some("mail"), "x")), None);
    }

    #[test]
    fn times_read_as_the_web_console_shows_them() {
        assert_eq!(clock(0.0), "0:00.0");
        assert_eq!(clock(4017.0), "0:04.0");
        assert_eq!(clock(61_250.0), "1:01.3");
        assert_eq!(clock(599_960.0), "10:00.0");
        assert_eq!(length(400), "0 s");
        assert_eq!(length(59_400), "59 s");
        assert_eq!(length(125_000), "2 min 5 s");
        assert_eq!(seconds(1250), "1.2 s");
    }

    fn at(year: i32, month: u8, day: u8, hour: u8) -> LocalTime {
        LocalTime {
            year,
            month,
            day,
            hour,
            minute: 0,
            second: 0,
        }
    }

    fn started(id: &str, when: LocalTime) -> RecordingSummary {
        RecordingSummary {
            id: RecordingId::new(id.into()),
            started: when,
            session: 1,
            machines: Vec::new(),
            videos: 1,
            calls: 1,
            traced_ms: 1000,
        }
    }

    #[test]
    fn the_library_groups_by_day_newest_first() {
        let listed = [
            started("a", at(2026, 11, 15, 9)),
            started("b", at(2026, 11, 12, 12)),
            started("c", at(2026, 11, 15, 16)),
            started("d", at(2026, 11, 14, 23)),
        ];
        let groups = by_day(&listed, at(2026, 11, 15, 22));
        let days: Vec<Day> = groups.iter().map(|group| group.day).collect();
        assert_eq!(days[..2], [Day::Today, Day::Yesterday]);
        assert_eq!(days[2].name(), "Thu Nov 12");
        let today: Vec<&str> = groups[0]
            .recordings
            .iter()
            .map(|summary| summary.id.as_str())
            .collect();
        assert_eq!(today, ["c", "a"]);
    }

    #[test]
    fn yesterday_is_known_across_a_month_and_a_year() {
        let listed = [started("a", at(2026, 11, 30, 8))];
        assert_eq!(by_day(&listed, at(2026, 12, 1, 8))[0].day, Day::Yesterday);
        let listed = [started("a", at(2025, 12, 31, 8))];
        assert_eq!(by_day(&listed, at(2026, 1, 1, 8))[0].day, Day::Yesterday);
        assert!(by_day(&[], at(2026, 1, 1, 8)).is_empty());
    }

    #[test]
    fn a_summary_knows_its_machines() {
        let summary = |machines: &[&str], calls| RecordingSummary {
            id: RecordingId::new("r".into()),
            started: LocalTime {
                year: 2026,
                month: 11,
                day: 14,
                hour: 6,
                minute: 15,
                second: 42,
            },
            session: 1,
            machines: machines.iter().map(|raw| name(raw)).collect(),
            videos: 1,
            calls,
            traced_ms: 3000,
        };
        let listed = [summary(&["web", "dev"], 1), summary(&["dev"], 1)];
        assert_eq!(
            RecordingSummary::machines_of(&listed),
            [name("dev"), name("web")]
        );
        assert!(listed[1].shows(None));
        assert!(!listed[1].shows(Some(&name("web"))));
    }

    #[test]
    fn a_video_chip_drops_the_extension() {
        assert_eq!(VideoName::new("live-001.mp4".into()).stem(), "live-001");
    }
}
