//! A session's recordings (`doppel mcp --record`), in the store's
//! `recordings/{start}-s{session}/`: the displays of the machines it
//! drives, recorded while it lasts, and a trace of its requests. Each VMM
//! lifetime is one segment: a video `{machine}-{NNN}.mp4`, and beside it
//! `{machine}-{NNN}.vtt`, WebVTT captions of the session's requests, timed
//! against the video, which players show as subtitles. `trace.jsonl` holds
//! one line per request, timed from the session's start, and
//! `segments.jsonl` one line per segment, saying when in the session its
//! video starts, so a replay can line the two up, and the display's size
//! in points, so it can place what calls acted on. `refs.jsonl` holds what
//! the session named the elements of each snapshot the trace kept, by the
//! trace line that kept it. `segments.jsonl` may be missing, as may a trace
//! line's gesture and a segment's points.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use doppel_desktop::deadline::Deadline;
use doppel_desktop::geometry::Size;
use doppel_host::capabilities::Clock;
use doppel_host::files::Filesystem;
use doppel_host::machines::{FINISH_RECORDING_TIMEOUT, MachineError, Machines};
use doppel_host::vmm::RecordingStart;
use doppel_machine::machine::MachineName;
use doppel_protocol::pb::agent::v1 as agent;
use doppel_protocol::pb::daemon::v1 as daemon;
use doppel_protocol::pb::daemon::v1::Gesture;
use doppel_protocol::pb::daemon::v1::call_started::Target;
use prost::Message as _;

use crate::activity;
use crate::call_events::CallEvent;
use crate::handle::Guest;
use crate::library::{Library, RecordingId};

/// One segment being recorded: its files, the host time its video starts
/// at, and its display's size in points, when the machine's spec says.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Segment {
    video: PathBuf,
    captions: PathBuf,
    origin: Instant,
    points: Option<Size>,
}

/// What this session knows of a machine's recording.
#[derive(Debug)]
enum Track {
    Recording(Segment),
    /// Not recorded, with the last reason logged, so a reason that persists
    /// (a VMM without recording, someone else's recording) is logged once.
    Off(Option<String>),
}

/// Why recording into a directory can't go on.
#[derive(Debug, thiserror::Error)]
pub enum RecordingError {
    #[error("{action} {}: {source}", path.display())]
    Directory {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{} already holds {max} segments of {machine}", dir.display())]
    Full {
        dir: PathBuf,
        max: u32,
        machine: MachineName,
    },
}

impl RecordingError {
    fn io(action: &'static str, path: &Path) -> impl FnOnce(std::io::Error) -> Self {
        let path = path.to_path_buf();
        move |source| Self::Directory {
            action,
            path,
            source,
        }
    }
}

/// The recordings of one session, in one directory.
#[derive(Debug)]
pub(crate) struct Recordings {
    dir: PathBuf,
    tracks: Mutex<HashMap<MachineName, Track>>,
    /// The session's start: what the trace's times count from.
    origin: Instant,
    /// The trace written so far, so names can find their line.
    traced: Mutex<Traces>,
    /// The segment each running call's machine was recorded into as it
    /// started, by request: its caption goes there.
    running: Mutex<HashMap<u64, Option<Segment>>>,
}

/// How many lines the trace has, and the line of each snapshot a call kept,
/// by its machine and id: the latest wins, as the agent may reuse an id
/// after it restarts.
#[derive(Debug, Default)]
struct Traces {
    lines: u32,
    seen: HashMap<(MachineName, u64), u32>,
}

/// How a traced request ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Traced {
    Done,
    /// It ran and failed on the host, or a wait failed.
    Failed(String),
    /// A wait ran out of time, with the last poll's error if it failed.
    TimedOut(Option<String>),
    /// The request itself was wrong.
    Refused(String),
}

/// One request, for the trace.
#[derive(Debug)]
pub(crate) struct TraceEntry<'a> {
    pub(crate) started: Instant,
    pub(crate) ended: Instant,
    pub(crate) machine: Option<&'a MachineName>,
    pub(crate) caption: &'a str,
    pub(crate) outcome: Traced,
    /// How long it waited before `started` for a person to hand the machine
    /// back, or for the session to be resumed.
    pub(crate) waited: Option<Duration>,
    /// How long it then waited in its machine's line, behind other calls,
    /// before `started`: it arrived that much earlier.
    pub(crate) queued: Option<Duration>,
    pub(crate) by: Actor,
    /// What it did on its machine's screen, and where, as viewers were told.
    pub(crate) acted: Option<&'a daemon::CallStarted>,
    /// The accessibility snapshot it saw, as viewers may see it: no
    /// password field's value.
    pub(crate) seen: Option<&'a agent::AxSnapshot>,
    /// What it read off the screen by text recognition.
    pub(crate) read: Option<&'a daemon::TextRead>,
}

/// Who did what a trace line records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Actor {
    /// The session: one of its calls.
    Session,
    /// A person at a viewer, who had control.
    Person,
}

impl Recordings {
    /// A segment number has three digits.
    const MAX_SEGMENTS: u32 = 999;

    const TRACE: &'static str = "trace.jsonl";
    const SEGMENTS: &'static str = "segments.jsonl";
    const REFS: &'static str = "refs.jsonl";

    /// Where a session that started at `started` is recorded, under the
    /// store's root: `recordings/2026-11-13T21-04-05Z-s3`, in UTC.
    pub(crate) fn folder(root: &Path, started: SystemTime, session: u64) -> PathBuf {
        let seconds = started
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        Library::in_root(root).folder(RecordingId::new(seconds, session))
    }

    /// Recordings in `dir`, created if missing, for a session that started
    /// at `origin`.
    pub(crate) fn open(dir: &Path, origin: Instant) -> Result<Self, RecordingError> {
        fs::create_dir_all(dir).map_err(RecordingError::io("creating", dir))?;
        // The VMM takes absolute paths.
        let dir = fs::canonicalize(dir).map_err(RecordingError::io("resolving", dir))?;
        for file in [Self::TRACE, Self::SEGMENTS, Self::REFS] {
            let path = dir.join(file);
            fs::write(&path, "").map_err(RecordingError::io("creating", &path))?;
        }
        Ok(Self {
            dir,
            tracks: Mutex::new(HashMap::new()),
            origin,
            traced: Mutex::new(Traces::default()),
            running: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// Records what the session's calls do: the machine a call names is
    /// recorded from before it runs; once answered it is captioned in that
    /// video and traced; and names the session gives elements are kept.
    pub(crate) fn on_call<C: Clock, F: Filesystem>(
        &self,
        event: &CallEvent<'_>,
        machines: &Machines<C, F>,
    ) {
        match event {
            CallEvent::Running { call } => {
                let segment = call
                    .machine
                    .as_ref()
                    .and_then(|machine| self.ensure(machines, machine));
                self.running().insert(call.request, segment);
            }
            CallEvent::TurnBegan { .. } => {}
            CallEvent::Ended {
                call,
                ran_from,
                began,
                ended,
                waited,
                answered,
            } => {
                let before = self.running().remove(&call.request).flatten();
                // A call that waited in its machine's line ran from its turn.
                let started = began.unwrap_or(*ran_from);
                let queued = Some(started.saturating_duration_since(*ran_from))
                    .filter(|queued| *queued >= Duration::from_millis(1));
                if let Some(machine) = &call.machine {
                    // A new VMM is recorded from its start, not from the next
                    // call.
                    let after = match call.guest {
                        Guest::MayRestart => self.ensure(machines, machine),
                        Guest::Kept => None,
                    };
                    if let Some(segment) = before.as_ref().or(after.as_ref()) {
                        let mut caption = call.caption.clone();
                        if let Some(why) = call
                            .acted
                            .as_ref()
                            .map(|acted| acted.why.as_str())
                            .filter(|why| !why.is_empty())
                        {
                            caption = format!("{caption} · {why}");
                        }
                        let caption = Self::caption_text(&caption, &answered.traced, *waited);
                        self.caption(segment, started, *ended, &caption);
                    }
                }
                self.trace(&TraceEntry {
                    started,
                    ended: *ended,
                    machine: call.machine.as_ref(),
                    caption: &call.caption,
                    outcome: answered.traced.clone(),
                    waited: *waited,
                    queued,
                    by: Actor::Session,
                    acted: call.acted.as_ref(),
                    seen: answered.seen.map(activity::shown).as_ref(),
                    read: answered.read.as_ref(),
                });
            }
            CallEvent::Named { machine, refs, .. } => self.named(machine, refs),
        }
    }

    /// A call's caption in the video: what it was, how it ended, and how
    /// long it was held up.
    fn caption_text(caption: &str, traced: &Traced, waited: Option<Duration>) -> String {
        let mut text = match traced {
            Traced::Done => caption.to_owned(),
            Traced::Failed(why) => format!("{caption}\nfailed: {why}"),
            Traced::TimedOut(None) => format!("{caption}\ntimed out"),
            Traced::TimedOut(Some(why)) => format!("{caption}\ntimed out: {why}"),
            Traced::Refused(why) => format!("{caption}\nrefused: {why}"),
        };
        if let Some(waited) = waited {
            text.push_str(&format!("\n(waited {:.1} s)", waited.as_secs_f64()));
        }
        text
    }

    fn running(&self) -> MutexGuard<'_, HashMap<u64, Option<Segment>>> {
        // Plain data: consistent after any panic.
        self.running.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A person had control of `machine` from `since` to `until`: a caption
    /// over that stretch of its video, and a line in the trace.
    pub(crate) fn person(&self, machine: &MachineName, since: Instant, until: Instant) {
        const CAPTION: &str = "a person had control";
        let segment = match self.tracks().get(machine) {
            Some(Track::Recording(segment)) => Some(segment.clone()),
            Some(Track::Off(_)) | None => None,
        };
        if let Some(segment) = segment {
            self.caption(&segment, since, until, CAPTION);
        }
        self.trace(&TraceEntry {
            started: since,
            ended: until,
            machine: Some(machine),
            caption: CAPTION,
            outcome: Traced::Done,
            waited: None,
            queued: None,
            by: Actor::Person,
            acted: None,
            seen: None,
            read: None,
        });
    }

    /// Appends a request to the trace. A trace that can't be written is
    /// logged, and the request's outcome stands.
    pub(crate) fn trace(&self, entry: &TraceEntry<'_>) {
        // Under the lock, so the count matches the lines' order.
        let mut traced = self.traced();
        self.append(Self::TRACE, &entry.json(self.origin));
        if let (Some(machine), Some(seen)) = (entry.machine, entry.seen) {
            let line = traced.lines;
            traced
                .seen
                .insert((machine.clone(), seen.snapshot_id), line);
        }
        traced.lines = traced.lines.saturating_add(1);
    }

    /// Keeps what the session named the elements of a snapshot of
    /// `machine` that a traced call kept, beside that call's line. Names
    /// of a snapshot the trace didn't keep are dropped.
    pub(crate) fn named(&self, machine: &MachineName, refs: &daemon::ElementRefs) {
        let traced = self.traced();
        let Some(&line) = traced.seen.get(&(machine.clone(), refs.snapshot)) else {
            return;
        };
        self.append(Self::REFS, &RefsLine::json(line, refs));
    }

    /// Plain data, valid after any panic that poisoned the lock.
    fn traced(&self) -> MutexGuard<'_, Traces> {
        self.traced.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Appends a line to one of the folder's files, logging a failure.
    fn append(&self, file: &str, line: &str) {
        let path = self.dir.join(file);
        let appended = OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut file| file.write_all(format!("{line}\n").as_bytes()));
        if let Err(error) = appended {
            eprintln!("doppeld: writing {}: {error}", path.display());
        }
    }

    /// The map is plain data, valid after any panic that poisoned the lock.
    fn tracks(&self) -> MutexGuard<'_, HashMap<MachineName, Track>> {
        self.tracks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Makes sure a running machine is being recorded: by the segment this
    /// session started, or a new one if its VMM has restarted since. `None`
    /// when it isn't running or can't be recorded.
    pub(crate) fn ensure<C: Clock, F: Filesystem>(
        &self,
        machines: &Machines<C, F>,
        name: &MachineName,
    ) -> Option<Segment> {
        let current = match self.tracks().get(name) {
            Some(Track::Recording(segment)) => Some(segment.clone()),
            Some(Track::Off(_)) | None => None,
        };
        let (video, captions) = match self.free_segment(name) {
            Ok(paths) => paths,
            Err(error) => return self.off(name, Some(error.to_string())),
        };
        let by = machines.deadline();
        match machines
            .machine(name)
            .running(by)
            .and_then(|mut running| running.start_recording(&video, by))
        {
            Ok(RecordingStart::Started) => {
                // The display the VMM records, as its spec sets it.
                let points = machines
                    .store()
                    .read_spec(&machines.store().bundle(name))
                    .ok()
                    .map(|spec| spec.display().points());
                let segment = Segment {
                    video,
                    captions,
                    origin: machines.clock().now(),
                    points,
                };
                if let Err(error) = fs::write(&segment.captions, Cue::FILE_HEADER) {
                    eprintln!(
                        "doppeld: {name}: writing {}: {error}",
                        segment.captions.display()
                    );
                }
                self.append(Self::SEGMENTS, &segment.json(name, self.origin));
                eprintln!("doppeld: {name}: recording to {}", segment.video.display());
                self.tracks()
                    .insert(name.clone(), Track::Recording(segment.clone()));
                Some(segment)
            }
            Ok(RecordingStart::AlreadyRecording(path)) => match current {
                Some(segment) if segment.video == path => Some(segment),
                Some(_) | None => self.off(
                    name,
                    Some(format!(
                        "already being recorded to {}, so not recorded here",
                        path.display()
                    )),
                ),
            },
            Err(MachineError::NotRunning(_)) => self.off(name, None),
            Err(error) => self.off(name, Some(format!("not recorded: {error}"))),
        }
    }

    /// Marks `name` unrecorded; logs `reason` unless it was the last one.
    fn off(&self, name: &MachineName, reason: Option<String>) -> Option<Segment> {
        let mut tracks = self.tracks();
        let repeated = match tracks.get(name) {
            Some(Track::Off(last)) => *last == reason,
            Some(Track::Recording(_)) | None => false,
        };
        if let (Some(reason), false) = (&reason, repeated) {
            eprintln!("doppeld: {name}: {reason}");
        }
        tracks.insert(name.clone(), Track::Off(reason));
        None
    }

    /// The first segment number with neither file taken.
    fn free_segment(&self, name: &MachineName) -> Result<(PathBuf, PathBuf), RecordingError> {
        for number in 1..=Self::MAX_SEGMENTS {
            let stem = format!("{name}-{number:03}");
            let video = self.dir.join(format!("{stem}.mp4"));
            let captions = self.dir.join(format!("{stem}.vtt"));
            let taken = |path: &Path| {
                path.try_exists()
                    .map_err(RecordingError::io("checking", path))
            };
            if !taken(&video)? && !taken(&captions)? {
                return Ok((video, captions));
            }
        }
        Err(RecordingError::Full {
            dir: self.dir.clone(),
            max: Self::MAX_SEGMENTS,
            machine: name.clone(),
        })
    }

    /// Adds a call that ran from `started` to `ended` to `segment`'s captions.
    pub(crate) fn caption(&self, segment: &Segment, started: Instant, ended: Instant, text: &str) {
        let cue = Cue::new(
            started.saturating_duration_since(segment.origin),
            ended.saturating_duration_since(segment.origin),
            text,
        );
        let appended = OpenOptions::new()
            .append(true)
            .open(&segment.captions)
            .and_then(|mut file| file.write_all(cue.render().as_bytes()));
        if let Err(error) = appended {
            eprintln!("doppeld: writing {}: {error}", segment.captions.display());
        }
    }

    /// Stops the recordings this session started; each video is complete
    /// once this returns. A machine that stopped completed its own.
    pub(crate) fn finish<C: Clock, F: Filesystem>(&self, machines: &Machines<C, F>) {
        let tracks = std::mem::take(&mut *self.tracks());
        for (name, track) in tracks {
            let segment = match track {
                Track::Recording(segment) => segment,
                Track::Off(_) => continue,
            };
            let by = Deadline::after(machines.clock().now(), FINISH_RECORDING_TIMEOUT);
            match machines
                .machine(&name)
                .running(by)
                .and_then(|mut running| running.stop_recording(Some(&segment.video), by))
            {
                Ok(summary) => eprintln!(
                    "doppeld: {name}: recorded {} ({} frames, {:.1} s)",
                    summary.path.display(),
                    summary.frames,
                    summary.seconds
                ),
                Err(MachineError::NotRunning(_)) => {}
                Err(error) => eprintln!(
                    "doppeld: {name}: stopping the recording to {}: {error}",
                    segment.video.display()
                ),
            }
        }
    }
}

/// A line of `trace.jsonl`, its fields in this order. Times are
/// milliseconds from the session's start; `error` is there when the request
/// didn't succeed.
#[derive(Debug, serde::Serialize)]
struct TraceLine<'a> {
    start_ms: u64,
    end_ms: u64,
    machine: Option<String>,
    call: &'a str,
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    waited_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    queued_ms: Option<u64>,
    /// Absent for the session's own calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    by: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gesture: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<TargetLine>,
    /// The snapshot it saw: a `doppel.agent.v1.AxSnapshot` as protobuf
    /// encodes it, in base64, so it reads back exactly and follows
    /// protobuf's rules as the schema grows.
    #[serde(skip_serializing_if = "Option::is_none")]
    seen: Option<String>,
    /// What it read off the screen: a `doppel.daemon.v1.TextRead`, encoded
    /// as `seen` is.
    #[serde(skip_serializing_if = "Option::is_none")]
    read: Option<String>,
    /// What a mark call put on the screen: a `doppel.daemon.v1.ScreenMark`,
    /// encoded as `seen` is.
    #[serde(skip_serializing_if = "Option::is_none")]
    mark: Option<String>,
    /// Why the client made the call, in its words, when it said.
    #[serde(skip_serializing_if = "Option::is_none")]
    why: Option<&'a str>,
}

/// A line of `refs.jsonl`: the names of the snapshot trace line `line`
/// (from 0) kept, as `doppel.daemon.v1.ElementRefs` fields.
#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RefsLine {
    pub(crate) line: u32,
    pub(crate) snapshot: u64,
    pub(crate) generation: u32,
    pub(crate) nodes: Vec<u32>,
}

impl RefsLine {
    fn json(line: u32, refs: &daemon::ElementRefs) -> String {
        let line = Self {
            line,
            snapshot: refs.snapshot,
            generation: refs.generation,
            nodes: refs.nodes.clone(),
        };
        // Numbers only: serializing can't fail.
        serde_json::to_string(&line).unwrap_or_default()
    }

    pub(crate) fn refs(self) -> daemon::ElementRefs {
        daemon::ElementRefs {
            snapshot: self.snapshot,
            generation: self.generation,
            nodes: self.nodes,
        }
    }
}

/// Where a call acted, in display points, in a trace line: one key naming
/// the kind (`{"point":{"x":1,"y":2}}`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TargetLine {
    Point(PointLine),
    Element {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        label: String,
    },
    Drag {
        from: PointLine,
        to: PointLine,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PointLine {
    x: f64,
    y: f64,
}

impl TargetLine {
    /// A target as a trace line writes it; `None` for one with a part missing.
    pub(crate) fn of(target: &Target) -> Option<Self> {
        let point = |at: Option<doppel_protocol::pb::common::v1::Point>| {
            at.map(|at| PointLine { x: at.x, y: at.y })
        };
        match target {
            Target::Point(at) => point(Some(*at)).map(Self::Point),
            Target::Element(element) => {
                let frame = element.frame?;
                let (origin, size) = (frame.origin?, frame.size?);
                Some(Self::Element {
                    x: origin.x,
                    y: origin.y,
                    width: size.width,
                    height: size.height,
                    label: element.label.clone(),
                })
            }
            Target::Drag(drag) => Some(Self::Drag {
                from: point(drag.from)?,
                to: point(drag.to)?,
            }),
        }
    }

    /// The target a trace line read back names.
    pub(crate) fn target(self) -> Target {
        use doppel_protocol::pb::common::v1 as common;
        let point = |at: PointLine| common::Point { x: at.x, y: at.y };
        match self {
            Self::Point(at) => Target::Point(point(at)),
            Self::Element {
                x,
                y,
                width,
                height,
                label,
            } => Target::Element(daemon::ElementTarget {
                frame: Some(common::Rect {
                    origin: Some(common::Point { x, y }),
                    size: Some(common::Size { width, height }),
                }),
                label,
            }),
            Self::Drag { from, to } => Target::Drag(daemon::DragTarget {
                from: Some(point(from)),
                to: Some(point(to)),
            }),
        }
    }
}

/// A gesture's name in a trace line.
pub(crate) const fn gesture_name(gesture: Gesture) -> Option<&'static str> {
    match gesture {
        Gesture::Click => Some("click"),
        Gesture::DoubleClick => Some("double_click"),
        Gesture::SecondaryClick => Some("secondary_click"),
        Gesture::Move => Some("move"),
        Gesture::Drag => Some("drag"),
        Gesture::Scroll => Some("scroll"),
        Gesture::Key => Some("key"),
        Gesture::Type => Some("type"),
        Gesture::Act => Some("act"),
        Gesture::Look => Some("look"),
        Gesture::Wait => Some("wait"),
        Gesture::Run => Some("run"),
        Gesture::Lifecycle => Some("lifecycle"),
        Gesture::Checkpoint => Some("checkpoint"),
        Gesture::Rollback => Some("rollback"),
        Gesture::Other => Some("other"),
        Gesture::Mark => Some("mark"),
        Gesture::Unspecified => None,
    }
}

/// The gesture a trace line names; a name this build doesn't know reads as
/// unspecified, which viewers show as another call.
pub(crate) fn gesture_named(name: &str) -> Gesture {
    [
        Gesture::Click,
        Gesture::DoubleClick,
        Gesture::SecondaryClick,
        Gesture::Move,
        Gesture::Drag,
        Gesture::Scroll,
        Gesture::Key,
        Gesture::Type,
        Gesture::Act,
        Gesture::Look,
        Gesture::Wait,
        Gesture::Run,
        Gesture::Lifecycle,
        Gesture::Checkpoint,
        Gesture::Rollback,
        Gesture::Other,
        Gesture::Mark,
    ]
    .into_iter()
    .find(|gesture| gesture_name(*gesture) == Some(name))
    .unwrap_or(Gesture::Unspecified)
}

impl TraceEntry<'_> {
    fn json(&self, origin: Instant) -> String {
        let millis = |at: Instant| {
            u64::try_from(at.saturating_duration_since(origin).as_millis()).unwrap_or(u64::MAX)
        };
        let (outcome, error) = match &self.outcome {
            Traced::Done => ("done", None),
            Traced::Failed(why) => ("failed", Some(why.as_str())),
            Traced::TimedOut(last_error) => ("timed_out", last_error.as_deref()),
            Traced::Refused(why) => ("refused", Some(why.as_str())),
        };
        let line = TraceLine {
            start_ms: millis(self.started),
            end_ms: millis(self.ended),
            machine: self.machine.map(ToString::to_string),
            call: self.caption,
            outcome,
            error,
            waited_ms: self
                .waited
                .map(|waited| u64::try_from(waited.as_millis()).unwrap_or(u64::MAX)),
            queued_ms: self
                .queued
                .map(|queued| u64::try_from(queued.as_millis()).unwrap_or(u64::MAX)),
            by: match self.by {
                Actor::Session => None,
                Actor::Person => Some("person"),
            },
            gesture: self.acted.and_then(|acted| gesture_name(acted.gesture())),
            target: self
                .acted
                .and_then(|acted| acted.target.as_ref())
                .and_then(TargetLine::of),
            seen: self.seen.map(|seen| BASE64.encode(seen.encode_to_vec())),
            read: self.read.map(|read| BASE64.encode(read.encode_to_vec())),
            mark: self
                .acted
                .and_then(|acted| acted.mark.as_ref())
                .map(|mark| BASE64.encode(mark.encode_to_vec())),
            why: self
                .acted
                .map(|acted| acted.why.as_str())
                .filter(|why| !why.is_empty()),
        };
        // Strings and numbers only: serializing can't fail.
        serde_json::to_string(&line).unwrap_or_default()
    }
}

/// A segment's line in `segments.jsonl`: its video's file name, its
/// machine, and when in the session its video starts. Fields keep this
/// order.
#[derive(serde::Serialize)]
struct SegmentLine<'a> {
    file: &'a str,
    machine: &'a str,
    start_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    points: Option<SizeLine>,
}

/// A display's size in points, in `segments.jsonl`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SizeLine {
    pub(crate) width: f64,
    pub(crate) height: f64,
}

impl Segment {
    fn json(&self, machine: &MachineName, origin: Instant) -> String {
        let file = self
            .video
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let start = self.origin.saturating_duration_since(origin);
        let line = SegmentLine {
            file,
            machine: machine.as_str(),
            start_ms: u64::try_from(start.as_millis()).unwrap_or(u64::MAX),
            points: self.points.map(|points| SizeLine {
                width: points.width(),
                height: points.height(),
            }),
        };
        // Strings and numbers only: serializing can't fail.
        serde_json::to_string(&line).unwrap_or_default()
    }
}

/// One call in the captions: shown while it ran, and for at least
/// [`Cue::MIN`] so a quick call can be read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cue {
    start: Duration,
    end: Duration,
    text: String,
}

impl Cue {
    const MIN: Duration = Duration::from_secs(1);
    /// What a captions file starts with, before its cues.
    const FILE_HEADER: &'static str = "WEBVTT\n\n";

    fn new(start: Duration, end: Duration, text: &str) -> Self {
        // A blank line ends a cue, and "-->" may not appear in one.
        let text = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| line.replace("-->", "->"))
            .collect::<Vec<_>>()
            .join("\n");
        Self {
            start,
            end: end.max(start + Self::MIN),
            text,
        }
    }

    fn render(&self) -> String {
        format!(
            "{} --> {}\n{}\n\n",
            Self::timestamp(self.start),
            Self::timestamp(self.end),
            self.text
        )
    }

    /// `HH:MM:SS.mmm`.
    fn timestamp(at: Duration) -> String {
        let millis = at.as_millis();
        format!(
            "{:02}:{:02}:{:02}.{:03}",
            millis / 3_600_000,
            millis / 60_000 % 60,
            millis / 1_000 % 60,
            millis % 1_000
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_s_folder_is_named_by_when_it_started_in_utc() {
        let started = SystemTime::UNIX_EPOCH + Duration::from_secs(1_794_603_845);
        assert_eq!(
            Recordings::folder(Path::new("/data"), started, 3),
            Path::new("/data/recordings/2026-11-13T21-04-05Z-s3")
        );
    }

    #[test]
    fn a_segment_line_says_when_in_the_session_its_video_starts() {
        let origin = doppel_host::capabilities::SystemClock.now();
        let segment = Segment {
            video: PathBuf::from("/data/recordings/x-s1/dev-002.mp4"),
            captions: PathBuf::from("/data/recordings/x-s1/dev-002.vtt"),
            origin: origin + Duration::from_millis(4_321),
            points: None,
        };
        assert_eq!(
            segment.json(&"dev".parse().unwrap(), origin),
            r#"{"file":"dev-002.mp4","machine":"dev","start_ms":4321}"#
        );
        let sized = Segment {
            points: Some(Size::new(1280.0, 800.0).unwrap()),
            ..segment
        };
        assert_eq!(
            sized.json(&"dev".parse().unwrap(), origin),
            r#"{"file":"dev-002.mp4","machine":"dev","start_ms":4321,"points":{"width":1280.0,"height":800.0}}"#
        );
    }

    #[test]
    fn a_trace_line_keeps_the_snapshot_a_call_saw_exactly() {
        use crate::library::TraceLine;
        let origin = doppel_host::capabilities::SystemClock.now();
        let dev: MachineName = "dev".parse().unwrap();
        let seen = agent::AxSnapshot {
            snapshot_id: 3,
            app: Some(agent::RunningApp {
                pid: 412,
                bundle_id: "com.apple.calculator".into(),
                name: "Calculator".into(),
            }),
            nodes: vec![agent::AxNode {
                role: "AXButton".into(),
                description: "Equals".into(),
                ..agent::AxNode::default()
            }],
            truncation: agent::Truncation::Complete.into(),
        };
        let line = |seen: Option<&agent::AxSnapshot>| {
            TraceEntry {
                started: origin,
                ended: origin + Duration::from_millis(5),
                machine: Some(&dev),
                caption: "snapshot",
                outcome: Traced::Done,
                waited: None,
                queued: None,
                by: Actor::Session,
                acted: None,
                seen,
                read: None,
            }
            .json(origin)
        };
        let kept = line(Some(&seen));
        let TraceLine::Call(call) = TraceLine::parse(&kept) else {
            panic!("{kept}");
        };
        assert!(call.seen);
        assert_eq!(TraceLine::seen(&kept), Some(seen));
        // A call that saw none keeps no key, as lines did before.
        let none = line(None);
        assert!(!none.contains("seen"), "{none}");
        assert_eq!(TraceLine::seen(&none), None);
        // A snapshot this build can't decode is dropped, the call kept.
        let garbled = kept.replace("\"seen\":\"", "\"seen\":\"!!");
        assert!(matches!(TraceLine::parse(&garbled), TraceLine::Call(_)));
        assert_eq!(TraceLine::seen(&garbled), None);
    }

    #[test]
    fn a_trace_line_says_what_a_call_did_on_the_screen_and_where() {
        use doppel_protocol::pb::common::v1 as common;
        let origin = doppel_host::capabilities::SystemClock.now();
        let dev: MachineName = "dev".parse().unwrap();
        let line = |acted: &daemon::CallStarted| {
            TraceEntry {
                started: origin,
                ended: origin + Duration::from_millis(5),
                machine: Some(&dev),
                caption: "c",
                outcome: Traced::Done,
                waited: None,
                queued: None,
                by: Actor::Session,
                acted: Some(acted),
                seen: None,
                read: None,
            }
            .json(origin)
        };
        let click = daemon::CallStarted {
            why: String::new(),
            mark: None,
            gesture: Gesture::DoubleClick.into(),
            target: Some(Target::Point(common::Point { x: 1.5, y: 2.0 })),
        };
        assert_eq!(
            line(&click),
            r#"{"start_ms":0,"end_ms":5,"machine":"dev","call":"c","outcome":"done","gesture":"double_click","target":{"point":{"x":1.5,"y":2.0}}}"#
        );
        let pressed = daemon::CallStarted {
            why: String::new(),
            mark: None,
            gesture: Gesture::Act.into(),
            target: Some(Target::Element(daemon::ElementTarget {
                frame: Some(common::Rect {
                    origin: Some(common::Point { x: 3.0, y: 4.0 }),
                    size: Some(common::Size {
                        width: 40.0,
                        height: 30.0,
                    }),
                }),
                label: "Equals".into(),
            })),
        };
        assert_eq!(
            line(&pressed),
            r#"{"start_ms":0,"end_ms":5,"machine":"dev","call":"c","outcome":"done","gesture":"act","target":{"element":{"x":3.0,"y":4.0,"width":40.0,"height":30.0,"label":"Equals"}}}"#
        );
        let dragged = daemon::CallStarted {
            why: String::new(),
            mark: None,
            gesture: Gesture::Drag.into(),
            target: Some(Target::Drag(daemon::DragTarget {
                from: Some(common::Point { x: 0.0, y: 0.0 }),
                to: Some(common::Point { x: 9.0, y: 9.0 }),
            })),
        };
        assert_eq!(
            line(&dragged),
            r#"{"start_ms":0,"end_ms":5,"machine":"dev","call":"c","outcome":"done","gesture":"drag","target":{"drag":{"from":{"x":0.0,"y":0.0},"to":{"x":9.0,"y":9.0}}}}"#
        );
        // Typing has no place.
        let typed = daemon::CallStarted {
            why: String::new(),
            mark: None,
            gesture: Gesture::Type.into(),
            target: None,
        };
        assert_eq!(
            line(&typed),
            r#"{"start_ms":0,"end_ms":5,"machine":"dev","call":"c","outcome":"done","gesture":"type"}"#
        );
        // Each target reads back as written.
        for acted in [click, pressed, dragged] {
            let target = acted.target.as_ref().unwrap();
            assert_eq!(&TargetLine::of(target).unwrap().target(), target);
        }
        // Every gesture's name reads back.
        for gesture in 1..=15 {
            let gesture = Gesture::try_from(gesture).unwrap();
            assert_eq!(gesture_named(gesture_name(gesture).unwrap()), gesture);
        }
        assert_eq!(gesture_named("juggle"), Gesture::Unspecified);
    }

    #[test]
    fn a_trace_line_names_the_call_its_times_and_how_it_ended() {
        let origin = doppel_host::capabilities::SystemClock.now();
        let dev: MachineName = "dev".parse().unwrap();
        let line = |machine, outcome| {
            TraceEntry {
                started: origin + Duration::from_millis(1_500),
                ended: origin + Duration::from_millis(2_250),
                machine,
                caption: "snapshot",
                outcome,
                waited: None,
                queued: None,
                by: Actor::Session,
                acted: None,
                seen: None,
                read: None,
            }
            .json(origin)
        };
        assert_eq!(
            line(Some(&dev), Traced::Done),
            r#"{"start_ms":1500,"end_ms":2250,"machine":"dev","call":"snapshot","outcome":"done"}"#
        );
        assert_eq!(
            line(None, Traced::Refused("bad name".into())),
            r#"{"start_ms":1500,"end_ms":2250,"machine":null,"call":"snapshot","outcome":"refused","error":"bad name"}"#
        );
        assert_eq!(
            line(Some(&dev), Traced::TimedOut(None)),
            r#"{"start_ms":1500,"end_ms":2250,"machine":"dev","call":"snapshot","outcome":"timed_out"}"#
        );
        assert_eq!(
            line(Some(&dev), Traced::TimedOut(Some("no VMM".into()))),
            r#"{"start_ms":1500,"end_ms":2250,"machine":"dev","call":"snapshot","outcome":"timed_out","error":"no VMM"}"#
        );
        let person = TraceEntry {
            started: origin + Duration::from_millis(1_500),
            ended: origin + Duration::from_millis(7_500),
            machine: Some(&dev),
            caption: "a person had control",
            outcome: Traced::Done,
            waited: None,
            queued: None,
            by: Actor::Person,
            acted: None,
            seen: None,
            read: None,
        };
        assert_eq!(
            person.json(origin),
            r#"{"start_ms":1500,"end_ms":7500,"machine":"dev","call":"a person had control","outcome":"done","by":"person"}"#
        );
        let held_up = TraceEntry {
            waited: Some(Duration::from_millis(3_100)),
            by: Actor::Session,
            ..person
        };
        assert_eq!(
            held_up.json(origin),
            r#"{"start_ms":1500,"end_ms":7500,"machine":"dev","call":"a person had control","outcome":"done","waited_ms":3100}"#
        );
        let queued = TraceEntry {
            queued: Some(Duration::from_millis(3_324)),
            ..held_up
        };
        assert_eq!(
            queued.json(origin),
            r#"{"start_ms":1500,"end_ms":7500,"machine":"dev","call":"a person had control","outcome":"done","waited_ms":3100,"queued_ms":3324}"#
        );
    }

    #[test]
    fn names_are_kept_beside_the_line_that_kept_their_snapshot() {
        let dir = std::env::temp_dir().join(format!("dr-refs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let origin = doppel_host::capabilities::SystemClock.now();
        let recordings = Recordings::open(&dir, origin).unwrap();
        let dev: MachineName = "dev".parse().unwrap();
        let seen = |id| agent::AxSnapshot {
            snapshot_id: id,
            ..agent::AxSnapshot::default()
        };
        let (first, second) = (seen(5), seen(6));
        let entry = |seen| TraceEntry {
            started: origin,
            ended: origin,
            machine: Some(&dev),
            caption: "snapshot",
            outcome: Traced::Done,
            waited: None,
            queued: None,
            by: Actor::Session,
            acted: None,
            seen,
            read: None,
        };
        recordings.trace(&entry(Some(&first)));
        recordings.trace(&entry(None));
        recordings.trace(&entry(Some(&second)));
        let refs = |snapshot| daemon::ElementRefs {
            snapshot,
            generation: 2,
            nodes: vec![1, 3],
        };
        recordings.named(&dev, &refs(6));
        // Not a snapshot the trace kept, or not on this machine.
        recordings.named(&dev, &refs(7));
        recordings.named(&"other".parse().unwrap(), &refs(5));
        let written = fs::read_to_string(dir.join("refs.jsonl")).unwrap();
        assert_eq!(
            written,
            "{\"line\":2,\"snapshot\":6,\"generation\":2,\"nodes\":[1,3]}\n"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_caption_says_how_its_call_ended_and_how_long_it_waited() {
        let caption = |traced, waited| Recordings::caption_text("click Save", &traced, waited);
        assert_eq!(caption(Traced::Done, None), "click Save");
        assert_eq!(
            caption(Traced::Failed("no window".into()), None),
            "click Save\nfailed: no window"
        );
        assert_eq!(
            caption(Traced::TimedOut(None), Some(Duration::from_millis(2_340))),
            "click Save\ntimed out\n(waited 2.3 s)"
        );
        assert_eq!(
            caption(Traced::Refused("bad ref".into()), None),
            "click Save\nrefused: bad ref"
        );
    }

    #[test]
    fn a_cue_spans_its_call_in_webvtt_time() {
        let cue = Cue::new(
            Duration::from_millis(3_723_456),
            Duration::from_millis(3_725_000),
            "clicked (10, 20) (via host)",
        );
        assert_eq!(
            cue.render(),
            "01:02:03.456 --> 01:02:05.000\nclicked (10, 20) (via host)\n\n"
        );
    }

    #[test]
    fn a_quick_call_shows_long_enough_to_read() {
        let cue = Cue::new(
            Duration::from_millis(500),
            Duration::from_millis(520),
            "status",
        );
        assert_eq!(cue.end, Duration::from_millis(1_500));
    }

    #[test]
    fn text_that_would_end_or_break_a_cue_is_kept_inside_it() {
        let cue = Cue::new(
            Duration::ZERO,
            Duration::from_secs(2),
            "exec /bin/sh\n\n  failed: a --> b  \n",
        );
        assert_eq!(cue.text, "exec /bin/sh\nfailed: a -> b");
    }
}
