//! The recordings in the store, read back for replay: which folders there
//! are, and for one, its videos with when each starts and its trace,
//! parsed. `recording` writes these folders; this reads them, and parses
//! every name and line it reads, so a folder that isn't a recording, or a
//! line it can't read, is skipped or kept as it is, never guessed at.

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use doppel_machine::machine::MachineName;
pub(crate) use doppel_machine::recording::{RecordingId, VideoFile};
use doppel_protocol::pb::agent::v1 as agent;
use doppel_protocol::pb::daemon::v1 as daemon;
use prost::Message as _;

use crate::recording::{Actor, Outcome, RefsLine, SizeLine, TargetLine, gesture_named};

/// One line of a trace, read back.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TraceLine {
    Call(TraceCall),
    /// A line this build can't read, as it is in the file.
    Unreadable(String),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TraceCall {
    pub(crate) start_ms: u64,
    pub(crate) end_ms: u64,
    pub(crate) machine: Option<String>,
    pub(crate) call: String,
    pub(crate) outcome: Outcome,
    pub(crate) error: Option<String>,
    pub(crate) waited_ms: Option<u64>,
    pub(crate) queued_ms: Option<u64>,
    pub(crate) by: Actor,
    /// What it did on the screen and where, when the trace kept it.
    pub(crate) acted: Option<Box<daemon::CallStarted>>,
    /// The trace kept the snapshot it saw: [`Library::seen`] reads it.
    pub(crate) seen: bool,
    /// What the session named that snapshot's elements, from `refs.jsonl`.
    pub(crate) refs: Option<Box<daemon::ElementRefs>>,
    /// What it read off the screen; one this build can't decode is dropped.
    pub(crate) read: Option<Box<daemon::TextRead>>,
}

/// A trace line as `recording` writes it.
#[derive(serde::Deserialize)]
struct RawCall {
    start_ms: u64,
    end_ms: u64,
    machine: Option<String>,
    call: String,
    outcome: Outcome,
    error: Option<String>,
    waited_ms: Option<u64>,
    queued_ms: Option<u64>,
    /// Absent for the session's own calls.
    by: Option<Actor>,
    gesture: Option<String>,
    /// A target this build can't read is dropped: the call is still shown.
    #[serde(default, deserialize_with = "readable")]
    target: Option<TargetLine>,
    /// Base64 of a protobuf `AxSnapshot`; read only when asked for.
    seen: Option<String>,
    /// Base64 of a protobuf `TextRead`.
    read: Option<String>,
    /// Base64 of a protobuf `ScreenMark`.
    mark: Option<String>,
    /// Why the client made the call, when it said.
    why: Option<String>,
}

/// A value read if it can be, `None` if it can't.
fn readable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

impl TraceLine {
    /// Reads one line of `trace.jsonl`.
    pub(crate) fn parse(line: &str) -> Self {
        let Ok(raw) = serde_json::from_str::<RawCall>(line) else {
            return Self::Unreadable(line.to_owned());
        };
        Self::Call(TraceCall {
            start_ms: raw.start_ms,
            end_ms: raw.end_ms,
            machine: raw.machine,
            call: raw.call,
            outcome: raw.outcome,
            error: raw.error,
            waited_ms: raw.waited_ms,
            queued_ms: raw.queued_ms,
            by: raw.by.unwrap_or(Actor::Session),
            acted: raw.gesture.map(|gesture| {
                Box::new(daemon::CallStarted {
                    gesture: gesture_named(&gesture).into(),
                    target: raw.target.map(TargetLine::target),
                    mark: raw
                        .mark
                        .and_then(|encoded| BASE64.decode(encoded).ok())
                        .and_then(|bytes| daemon::ScreenMark::decode(bytes.as_slice()).ok()),
                    why: raw.why.unwrap_or_default(),
                })
            }),
            seen: raw.seen.is_some(),
            refs: None,
            read: raw
                .read
                .and_then(|encoded| BASE64.decode(encoded).ok())
                .and_then(|bytes| daemon::TextRead::decode(bytes.as_slice()).ok())
                .map(Box::new),
        })
    }

    /// The snapshot a line kept, decoded; `None` for a line with none, or
    /// one this build can't read.
    pub(crate) fn seen(line: &str) -> Option<agent::AxSnapshot> {
        #[derive(serde::Deserialize)]
        struct Seen {
            seen: Option<String>,
        }
        let encoded = serde_json::from_str::<Seen>(line).ok()?.seen?;
        let bytes = BASE64.decode(encoded).ok()?;
        agent::AxSnapshot::decode(bytes.as_slice()).ok()
    }
}

/// A line of `segments.jsonl`: when in the session a video starts.
#[derive(Debug, Clone, PartialEq)]
struct SegmentStart {
    file: VideoFile,
    start_ms: u64,
    points: Option<SizeLine>,
}

#[derive(serde::Deserialize)]
struct RawSegment {
    file: String,
    start_ms: u64,
    #[serde(default, deserialize_with = "readable")]
    points: Option<SizeLine>,
}

impl SegmentStart {
    /// Reads one line of `segments.jsonl`; `None` for one it can't read.
    fn parse(line: &str) -> Option<Self> {
        let raw = serde_json::from_str::<RawSegment>(line).ok()?;
        Some(Self {
            file: raw.file.parse().ok()?,
            start_ms: raw.start_ms,
            points: raw.points,
        })
    }
}

/// A recording's video, with when it starts and its display's size in
/// points, if those were recorded.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Video {
    pub(crate) file: VideoFile,
    pub(crate) start_ms: Option<u64>,
    pub(crate) points: Option<SizeLine>,
}

/// One recording, read back.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Recording {
    pub(crate) id: RecordingId,
    /// In the order they started; those with no start last, by name.
    pub(crate) videos: Vec<Video>,
    pub(crate) trace: Vec<TraceLine>,
}

impl Recording {
    /// A recording from what its folder holds: the video files' names, and
    /// the text of `segments.jsonl`, `trace.jsonl`, and `refs.jsonl`, the
    /// first and last when present.
    fn read(
        id: RecordingId,
        files: &[VideoFile],
        segments: Option<&str>,
        trace: &str,
        refs: Option<&str>,
    ) -> Self {
        let starts: Vec<SegmentStart> = segments
            .unwrap_or_default()
            .lines()
            .filter_map(SegmentStart::parse)
            .collect();
        let mut videos: Vec<Video> = files
            .iter()
            .map(|file| {
                let start = starts.iter().find(|start| &start.file == file);
                Video {
                    file: file.clone(),
                    start_ms: start.map(|start| start.start_ms),
                    points: start.and_then(|start| start.points),
                }
            })
            .collect();
        videos.sort_by(|a, b| match (a.start_ms, b.start_ms) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
        let mut trace: Vec<TraceLine> = trace
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(TraceLine::parse)
            .collect();
        let named = refs
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<RefsLine>(line).ok());
        for named in named {
            let line = usize::try_from(named.line).unwrap_or(usize::MAX);
            if let Some(TraceLine::Call(call)) = trace.get_mut(line)
                && call.seen
            {
                call.refs = Some(Box::new(named.refs()));
            }
        }
        Self { id, videos, trace }
    }

    /// The machines it has videos of, in the order they started.
    pub(crate) fn machines(&self) -> Vec<&MachineName> {
        let mut machines: Vec<&MachineName> = Vec::new();
        for video in &self.videos {
            if !machines.contains(&video.file.machine()) {
                machines.push(video.file.machine());
            }
        }
        machines
    }

    /// When its last traced call ended.
    pub(crate) fn traced_ms(&self) -> u64 {
        self.trace
            .iter()
            .filter_map(|line| match line {
                TraceLine::Call(call) => Some(call.end_ms),
                TraceLine::Unreadable(_) => None,
            })
            .max()
            .unwrap_or(0)
    }
}

/// Why a recording can't be read.
#[derive(Debug, thiserror::Error)]
pub(crate) enum LibraryError {
    #[error("no recording {0}")]
    Missing(RecordingId),
    #[error("recording {id} kept no snapshot on its trace's line {line}")]
    NoSnapshot { id: RecordingId, line: u32 },
    #[error("reading {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// The store's recordings: `recordings/` under its data root.
#[derive(Debug, Clone)]
pub(crate) struct Library {
    dir: PathBuf,
}

impl Library {
    const TRACE: &'static str = "trace.jsonl";
    const SEGMENTS: &'static str = "segments.jsonl";
    const REFS: &'static str = "refs.jsonl";

    pub(crate) fn in_root(root: &Path) -> Self {
        Self {
            dir: root.join("recordings"),
        }
    }

    /// Where recording `id` is, whether or not it exists.
    pub(crate) fn folder(&self, id: RecordingId) -> PathBuf {
        self.dir.join(id.to_string())
    }

    fn io(path: &Path) -> impl FnOnce(std::io::Error) -> LibraryError + '_ {
        move |source| LibraryError::Io {
            path: path.to_owned(),
            source,
        }
    }

    /// Every recording, newest first. Entries that aren't recordings'
    /// folders are skipped, and so is a folder that can't be read.
    pub(crate) fn list(&self) -> Result<Vec<Recording>, LibraryError> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(Self::io(&self.dir)(error)),
        };
        let mut ids: Vec<RecordingId> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
            .collect();
        ids.sort_unstable_by(|a, b| b.cmp(a));
        Ok(ids
            .into_iter()
            .filter_map(|id| self.open(id).ok())
            .collect())
    }

    /// Recording `id`, read.
    pub(crate) fn open(&self, id: RecordingId) -> Result<Recording, LibraryError> {
        let folder = self.folder(id);
        let entries = match fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(LibraryError::Missing(id));
            }
            Err(error) => return Err(Self::io(&folder)(error)),
        };
        let mut files: Vec<VideoFile> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
            .collect();
        files.sort_by_key(ToString::to_string);
        let read = |name: &str| -> Result<Option<String>, LibraryError> {
            let path = folder.join(name);
            match fs::read_to_string(&path) {
                Ok(text) => Ok(Some(text)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(Self::io(&path)(error)),
            }
        };
        let segments = read(Self::SEGMENTS)?;
        let trace = read(Self::TRACE)?.unwrap_or_default();
        let refs = read(Self::REFS)?;
        Ok(Recording::read(
            id,
            &files,
            segments.as_deref(),
            &trace,
            refs.as_deref(),
        ))
    }

    /// The snapshot line `line` (from 0) of recording `id`'s trace kept.
    pub(crate) fn seen(
        &self,
        id: RecordingId,
        line: u32,
    ) -> Result<agent::AxSnapshot, LibraryError> {
        let path = self.folder(id).join(Self::TRACE);
        let trace = fs::read_to_string(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                LibraryError::Missing(id)
            } else {
                Self::io(&path)(error)
            }
        })?;
        trace
            .lines()
            .filter(|text| !text.trim().is_empty())
            .nth(usize::try_from(line).unwrap_or(usize::MAX))
            .and_then(TraceLine::seen)
            .ok_or(LibraryError::NoSnapshot { id, line })
    }

    /// A video of recording `id`, opened to be served, with its length.
    pub(crate) fn video(
        &self,
        id: RecordingId,
        file: &VideoFile,
    ) -> Result<(fs::File, u64), LibraryError> {
        let path = self.folder(id).join(file.to_string());
        let opened = fs::File::open(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                LibraryError::Missing(id)
            } else {
                Self::io(&path)(error)
            }
        })?;
        let len = opened.metadata().map_err(Self::io(&path))?.len();
        Ok((opened, len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_read_onto_the_call_whose_snapshot_they_name() {
        let trace = concat!(
            r#"{"start_ms":0,"end_ms":1,"machine":"dev","call":"snapshot","outcome":"done","seen":""}"#,
            "\n",
            r#"{"start_ms":1,"end_ms":2,"machine":"dev","call":"click","outcome":"done"}"#,
            "\n",
        );
        let refs = concat!(
            r#"{"line":0,"snapshot":6,"generation":2,"nodes":[1,3]}"#,
            "\n",
            // A line that kept no snapshot, one past the end, and garbage.
            r#"{"line":1,"snapshot":6,"generation":2,"nodes":[1]}"#,
            "\n",
            r#"{"line":9,"snapshot":6,"generation":2,"nodes":[1]}"#,
            "\nnot json\n",
        );
        let recording = Recording::read(
            RecordingId::new(0, doppel_machine::activity::SessionId::from(1)),
            &[],
            None,
            trace,
            Some(refs),
        );
        let named: Vec<Option<daemon::ElementRefs>> = recording
            .trace
            .into_iter()
            .map(|line| match line {
                TraceLine::Call(call) => call.refs.map(|refs| *refs),
                TraceLine::Unreadable(_) => None,
            })
            .collect();
        assert_eq!(
            named,
            vec![
                Some(daemon::ElementRefs {
                    snapshot: 6,
                    generation: 2,
                    nodes: vec![1, 3]
                }),
                None
            ]
        );
    }

    #[test]
    fn trace_lines_are_read_and_a_line_it_can_t_read_is_kept() {
        let call = TraceLine::parse(
            r#"{"start_ms":1500,"end_ms":7500,"machine":"dev","call":"a person had control","outcome":"done","by":"person"}"#,
        );
        assert_eq!(
            call,
            TraceLine::Call(TraceCall {
                start_ms: 1_500,
                end_ms: 7_500,
                machine: Some("dev".into()),
                call: "a person had control".into(),
                outcome: Outcome::Done,
                error: None,
                waited_ms: None,
                queued_ms: None,
                by: Actor::Person,
                acted: None,
                seen: false,
                refs: None,
                read: None,
            })
        );
        // What a call did on the screen; a target this build can't read is
        // dropped and the call kept, and a gesture it doesn't know is
        // unspecified.
        let acted = |line: &str| match TraceLine::parse(line) {
            TraceLine::Call(call) => call.acted,
            TraceLine::Unreadable(line) => panic!("{line}"),
        };
        let clicked = acted(
            r#"{"start_ms":0,"end_ms":5,"machine":"dev","call":"c","outcome":"done","gesture":"click","target":{"point":{"x":1.5,"y":2.0}}}"#,
        )
        .unwrap();
        assert_eq!(clicked.gesture(), daemon::Gesture::Click);
        assert!(matches!(
            clicked.target,
            Some(daemon::call_started::Target::Point(_))
        ));
        // A mark comes back as it was drawn; one this build can't decode is
        // dropped and the call kept.
        let drawn = daemon::ScreenMark {
            shape: daemon::MarkShape::Dot.into(),
            at: Some(daemon::screen_mark::At::Point(
                doppel_protocol::pb::common::v1::Point { x: 5.0, y: 6.0 },
            )),
            from: None,
            note: "here".into(),
            lasts: Some(daemon::screen_mark::Lasts::UntilActedMs(3000)),
        };
        let marked = acted(&format!(
            r#"{{"start_ms":0,"end_ms":1,"machine":"dev","call":"mark","outcome":"done","gesture":"mark","mark":"{}"}}"#,
            BASE64.encode(drawn.encode_to_vec())
        ))
        .unwrap();
        assert_eq!(marked.gesture(), daemon::Gesture::Mark);
        assert_eq!(marked.mark, Some(drawn));
        let garbled = acted(
            r#"{"start_ms":0,"end_ms":1,"machine":"dev","call":"mark","outcome":"done","gesture":"mark","mark":"!!"}"#,
        )
        .unwrap();
        assert_eq!(garbled.mark, None);
        let odd = acted(
            r#"{"start_ms":0,"end_ms":5,"machine":"dev","call":"c","outcome":"done","gesture":"juggle","target":{"hologram":{}}}"#,
        )
        .unwrap();
        assert_eq!(
            (odd.gesture(), odd.target),
            (daemon::Gesture::Unspecified, None)
        );
        let refused = TraceLine::parse(
            r#"{"start_ms":1,"end_ms":2,"machine":null,"call":"x","outcome":"refused","error":"bad name","waited_ms":30,"queued_ms":40}"#,
        );
        let TraceLine::Call(refused) = refused else {
            panic!("{refused:?}");
        };
        assert_eq!(
            (
                refused.outcome,
                refused.error.as_deref(),
                refused.waited_ms,
                refused.queued_ms,
                refused.by
            ),
            (
                Outcome::Refused,
                Some("bad name"),
                Some(30),
                Some(40),
                Actor::Session
            )
        );
        for line in [
            "not json",
            r#"{"start_ms":1,"end_ms":2,"machine":"dev","call":"x","outcome":"exploded"}"#,
            r#"{"start_ms":1,"end_ms":2,"machine":"dev","call":"x","outcome":"done","by":"robot"}"#,
        ] {
            assert_eq!(TraceLine::parse(line), TraceLine::Unreadable(line.into()));
        }
    }

    #[test]
    fn videos_line_up_by_their_recorded_starts() {
        let files: Vec<VideoFile> = ["dev-001.mp4", "dev-002.mp4", "web-001.mp4"]
            .iter()
            .map(|name| name.parse().unwrap())
            .collect();
        let segments = "{\"file\":\"dev-001.mp4\",\"machine\":\"dev\",\"start_ms\":98}\n\
                        garbage\n\
                        {\"file\":\"dev-002.mp4\",\"machine\":\"dev\",\"start_ms\":9000,\"points\":{\"width\":1280.0,\"height\":800.0}}\n";
        let trace = "{\"start_ms\":99,\"end_ms\":122,\"machine\":\"dev\",\"call\":\"status\",\"outcome\":\"done\"}\n\n";
        let recording = Recording::read(
            RecordingId::new(0, doppel_machine::activity::SessionId::from(1)),
            &files,
            Some(segments),
            trace,
            None,
        );
        let starts: Vec<(String, Option<u64>)> = recording
            .videos
            .iter()
            .map(|video| (video.file.to_string(), video.start_ms))
            .collect();
        assert_eq!(
            starts,
            vec![
                ("dev-001.mp4".into(), Some(98)),
                ("dev-002.mp4".into(), Some(9_000)),
                ("web-001.mp4".into(), None),
            ]
        );
        assert_eq!(recording.trace.len(), 1);
        assert_eq!(recording.traced_ms(), 122);
        let machines: Vec<&str> = recording.machines().iter().map(|m| m.as_str()).collect();
        assert_eq!(machines, ["dev", "web"]);
        // Points where they were kept: a video's targets are placed by them.
        let points: Vec<Option<SizeLine>> =
            recording.videos.iter().map(|video| video.points).collect();
        assert_eq!(
            points,
            vec![
                None,
                Some(SizeLine {
                    width: 1280.0,
                    height: 800.0
                }),
                None
            ]
        );
        // Recorded before segments.jsonl: no video has a start.
        let older = Recording::read(
            RecordingId::new(0, doppel_machine::activity::SessionId::from(1)),
            &files,
            None,
            trace,
            None,
        );
        assert!(older.videos.iter().all(|video| video.start_ms.is_none()));
    }
}
