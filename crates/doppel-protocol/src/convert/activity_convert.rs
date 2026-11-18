use std::time::Duration;

use doppel_desktop::ax::tree::AxSnapshot;
use doppel_desktop::geometry::{Point, Rect, Size};
use doppel_machine::activity::{
    Activity, CALLOUT_LASTS, Callout, CalloutLasts, CalloutShape, Ended, Ending, Gesture, OnScreen,
    Phase, Target, TextRead,
};
use doppel_machine::recording::{
    Recording, RecordingSummary, RecordingVideo, TraceActor, TraceCall, TraceLine, TraceOutcome,
};

use super::ParseError;
use crate::pb::common::v1 as common;
use crate::pb::daemon::v1 as daemon;

/// An event as the daemon sends it. Refused without a phase, with a
/// machine name that isn't one, or with names a console can't read.
impl TryFrom<daemon::Activity> for Activity {
    type Error = ParseError;
    fn try_from(activity: daemon::Activity) -> Result<Self, ParseError> {
        let phase = match activity.phase.ok_or(ParseError::Missing("phase"))? {
            daemon::activity::Phase::Started(started) => Phase::Started(on_screen(started)),
            daemon::activity::Phase::Ended(ended) => Phase::Ended(ending(ended)),
            daemon::activity::Phase::Recent(recent) => Phase::Recent {
                acted: on_screen(recent.started.ok_or(ParseError::Missing("started"))?),
                started_ago: Duration::from_millis(recent.started_ms_ago),
                ended: recent
                    .ended
                    .zip(recent.ended_ms_ago)
                    .map(|(ended, ago)| (ending(ended), Duration::from_millis(ago))),
            },
            daemon::activity::Phase::Named(refs) => Phase::Named(refs.try_into()?),
        };
        Ok(Self {
            session: activity.session,
            request: activity.request,
            machine: activity
                .machine
                .parse()
                .map_err(|error| ParseError::invalid("machine", error))?,
            client: activity.client,
            caption: activity.caption,
            phase,
        })
    }
}

/// What a call read. Refused without its text, or with text, a query
/// or a region this build can't read.
impl TryFrom<daemon::TextRead> for TextRead {
    type Error = ParseError;
    fn try_from(read: daemon::TextRead) -> Result<Self, ParseError> {
        Ok(Self {
            text: read.text.ok_or(ParseError::Missing("text"))?.try_into()?,
            query: match read.query {
                Some(query) => Some(
                    query
                        .parse()
                        .map_err(|error| ParseError::invalid("query", error))?,
                ),
                None => None,
            },
            found: read
                .found
                .into_iter()
                .map(Rect::try_from)
                .collect::<Result<_, _>>()?,
        })
    }
}

/// How a call ended, and what it saw, as the daemon says it: what can't
/// be read is left out.
fn ending(ended: daemon::CallEnded) -> Ended {
    Ended {
        ending: match ended.outcome {
            Some(daemon::call_ended::Outcome::Done(_)) => Ending::Done,
            Some(daemon::call_ended::Outcome::Failed(why)) => Ending::Failed(why),
            Some(daemon::call_ended::Outcome::TimedOut(why)) => Ending::TimedOut(why),
            Some(daemon::call_ended::Outcome::Refused(why)) => Ending::Refused(why),
            None => Ending::Unknown,
        },
        seen: ended
            .seen
            .and_then(|seen| AxSnapshot::try_from(seen).ok())
            .map(Box::new),
        refs: ended.refs.and_then(|refs| refs.try_into().ok()),
        read: ended
            .read
            .and_then(|read| TextRead::try_from(read).ok())
            .map(Box::new),
    }
}

/// What a call did on the screen, and where, as the daemon says it. A
/// mark is on its own place, not the call's target; a place that can't
/// be read is left out.
fn on_screen(started: daemon::CallStarted) -> OnScreen {
    let (target, callout) = match started.mark {
        Some(mark) => {
            let target = mark.at.clone().and_then(|at| match at {
                daemon::screen_mark::At::Point(at) => {
                    target(daemon::call_started::Target::Point(at))
                }
                daemon::screen_mark::At::Area(area) => {
                    target(daemon::call_started::Target::Element(area))
                }
            });
            (target, Some(Box::new(callout(&mark))))
        }
        None => (started.target.and_then(target), None),
    };
    let why = Some(started.why.trim())
        .filter(|why| !why.is_empty())
        .map(ToOwned::to_owned);
    OnScreen {
        gesture: gesture(started.gesture),
        target,
        callout,
        why,
    }
}

fn callout(mark: &daemon::ScreenMark) -> Callout {
    use daemon::MarkShape as Wire;
    let from = point(mark.from);
    Callout {
        shape: match Wire::try_from(mark.shape) {
            Ok(Wire::Box) => CalloutShape::Box,
            Ok(Wire::Dot) => CalloutShape::Dot,
            Ok(Wire::Arrow) => CalloutShape::Arrow { from },
            Ok(Wire::Unspecified) | Err(_) => CalloutShape::Unknown(mark.shape),
        },
        note: Some(mark.note.trim())
            .filter(|note| !note.is_empty())
            .map(ToOwned::to_owned),
        lasts: match mark.lasts {
            Some(daemon::screen_mark::Lasts::ForMs(ms)) => {
                CalloutLasts::For(Duration::from_millis(ms.into()))
            }
            Some(daemon::screen_mark::Lasts::UntilActedMs(ms)) => {
                CalloutLasts::UntilActed(Duration::from_millis(ms.into()))
            }
            None => CalloutLasts::For(CALLOUT_LASTS),
        },
    }
}

fn gesture(raw: i32) -> Gesture {
    use daemon::Gesture as Wire;
    match Wire::try_from(raw) {
        Ok(Wire::Click) => Gesture::Click,
        Ok(Wire::DoubleClick) => Gesture::DoubleClick,
        Ok(Wire::SecondaryClick) => Gesture::SecondaryClick,
        Ok(Wire::Move) => Gesture::Move,
        Ok(Wire::Drag) => Gesture::Drag,
        Ok(Wire::Scroll) => Gesture::Scroll,
        Ok(Wire::Key) => Gesture::Key,
        Ok(Wire::Type) => Gesture::Type,
        Ok(Wire::Act) => Gesture::Act,
        Ok(Wire::Look) => Gesture::Look,
        Ok(Wire::Wait) => Gesture::Wait,
        Ok(Wire::Run) => Gesture::Run,
        Ok(Wire::Lifecycle) => Gesture::Lifecycle,
        Ok(Wire::Checkpoint) => Gesture::Checkpoint,
        Ok(Wire::Rollback) => Gesture::Rollback,
        Ok(Wire::Mark) => Gesture::Mark,
        Ok(Wire::Other | Wire::Unspecified) => Gesture::Other,
        Err(_) => Gesture::Unknown(raw),
    }
}

/// `None` for a target without the points it needs, a point off the
/// display, or an element's frame without area.
fn target(target: daemon::call_started::Target) -> Option<Target> {
    use daemon::call_started::Target as Wire;
    match target {
        Wire::Point(at) => Some(Target::Point(point(Some(at))?)),
        Wire::Element(element) => {
            let frame = Rect::try_from(element.frame?).ok()?;
            frame.has_area().then_some(Target::Element {
                frame,
                label: element.label,
            })
        }
        Wire::Drag(drag) => Some(Target::Drag {
            from: point(drag.from)?,
            to: point(drag.to)?,
        }),
    }
}

fn point(at: Option<common::Point>) -> Option<Point> {
    let at = at?;
    Point::new(at.x, at.y).ok()
}

/// A recording as listed. Refused when its id or a machine isn't a name.
impl TryFrom<daemon::RecordingSummary> for RecordingSummary {
    type Error = ParseError;
    fn try_from(summary: daemon::RecordingSummary) -> Result<Self, ParseError> {
        Ok(Self {
            id: summary
                .id
                .parse()
                .map_err(|error| ParseError::invalid("id", error))?,
            started_unix_seconds: summary.started_unix_seconds,
            session: summary.session,
            machines: summary
                .machines
                .iter()
                .map(|name| {
                    name.parse()
                        .map_err(|error| ParseError::invalid("machines", error))
                })
                .collect::<Result<_, _>>()?,
            videos: summary.videos,
            calls: summary.calls,
            traced_ms: summary.traced_ms,
        })
    }
}

/// A recording, opened. Refused when its id isn't one; videos whose
/// names aren't are left out, and trace lines that can't be read are
/// kept as they are.
impl TryFrom<daemon::Recording> for Recording {
    type Error = ParseError;
    fn try_from(recording: daemon::Recording) -> Result<Self, ParseError> {
        Ok(Self {
            id: recording
                .id
                .parse()
                .map_err(|error| ParseError::invalid("id", error))?,
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
                .map(|(line, traced)| trace_line(traced, u32::try_from(line).ok()))
                .collect(),
        })
    }
}

/// Line `index` of a trace.
fn trace_line(line: daemon::TraceLine, index: Option<u32>) -> TraceLine {
    use daemon::trace_line::Line;
    match line.line {
        Some(Line::Call(call)) => {
            let machine = match call.machine {
                Some(raw) => match raw.parse() {
                    Ok(name) => Some(name),
                    Err(_) => return TraceLine::Unreadable(format!("a call on {raw:?}")),
                },
                None => None,
            };
            TraceLine::Call(TraceCall {
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
                    Ok(daemon::TraceActor::Unspecified) | Err(_) => TraceActor::Unknown(call.by),
                },
                acted: call.acted.map(|acted| Box::new(on_screen(acted))),
                seen: index.filter(|_| call.seen),
                refs: call
                    .refs
                    .and_then(|refs| refs.try_into().ok())
                    .map(Box::new),
                read: call
                    .read
                    .and_then(|read| TextRead::try_from(read).ok())
                    .map(Box::new),
            })
        }
        Some(Line::Unreadable(raw)) => TraceLine::Unreadable(raw),
        None => TraceLine::Unreadable(String::new()),
    }
}

#[cfg(test)]
mod tests {
    use doppel_desktop::geometry::{Point, Size};
    use doppel_machine::activity::*;
    use doppel_machine::recording::*;
    use std::time::Duration;

    use crate::pb::common::v1 as common;
    use crate::pb::daemon::v1 as daemon;

    fn started(target: Option<daemon::call_started::Target>, gesture: i32) -> daemon::Activity {
        daemon::Activity {
            session: 3,
            request: 9,
            machine: "dev".into(),
            client: "claude".into(),
            caption: "click".into(),
            phase: Some(daemon::activity::Phase::Started(daemon::CallStarted {
                why: String::new(),
                gesture,
                target,
                mark: None,
            })),
        }
    }

    #[test]
    fn a_started_call_keeps_its_gesture_and_place() {
        let read = Activity::try_from(started(
            Some(daemon::call_started::Target::Point(common::Point {
                x: 10.0,
                y: 20.0,
            })),
            daemon::Gesture::Click as i32,
        ));
        assert_eq!(
            read.ok().map(|activity| activity.phase),
            Some(Phase::Started(OnScreen {
                why: None,
                gesture: Gesture::Click,
                target: Some(Target::Point(Point::new(10.0, 20.0).unwrap())),
                callout: None,
            }))
        );
    }

    #[test]
    fn unknown_gestures_are_kept_and_bad_places_dropped() {
        let read = Activity::try_from(started(
            Some(daemon::call_started::Target::Element(
                daemon::ElementTarget {
                    frame: Some(common::Rect {
                        origin: Some(common::Point { x: 1.0, y: 1.0 }),
                        size: Some(common::Size {
                            width: 0.0,
                            height: 4.0,
                        }),
                    }),
                    label: "OK".into(),
                },
            )),
            99,
        ));
        assert_eq!(
            read.ok().map(|activity| activity.phase),
            Some(Phase::Started(OnScreen {
                why: None,
                gesture: Gesture::Unknown(99),
                target: None,
                callout: None,
            }))
        );
    }

    #[test]
    fn a_mark_is_drawn_on_its_own_place() {
        let mut event = started(None, daemon::Gesture::Mark as i32);
        event.phase = Some(daemon::activity::Phase::Started(daemon::CallStarted {
            why: String::new(),
            gesture: daemon::Gesture::Mark as i32,
            target: None,
            mark: Some(daemon::ScreenMark {
                shape: daemon::MarkShape::Arrow as i32,
                at: Some(daemon::screen_mark::At::Point(common::Point {
                    x: 5.0,
                    y: 6.0,
                })),
                from: Some(common::Point { x: 1.0, y: 2.0 }),
                note: " here ".into(),
                lasts: Some(daemon::screen_mark::Lasts::UntilActedMs(3000)),
            }),
        }));
        assert_eq!(
            Activity::try_from(event)
                .ok()
                .map(|activity| activity.phase),
            Some(Phase::Started(OnScreen {
                why: None,
                gesture: Gesture::Mark,
                target: Some(Target::Point(Point::new(5.0, 6.0).unwrap())),
                callout: Some(Box::new(Callout {
                    shape: CalloutShape::Arrow {
                        from: Point::new(1.0, 2.0).ok()
                    },
                    note: Some("here".into()),
                    lasts: CalloutLasts::UntilActed(Duration::from_secs(3)),
                })),
            }))
        );
    }

    #[test]
    fn events_without_a_phase_or_a_valid_machine_are_skipped() {
        let mut no_phase = started(None, 1);
        no_phase.phase = None;
        assert!(Activity::try_from(no_phase).is_err());
        let mut bad_name = started(None, 1);
        bad_name.machine = "Not A Name".into();
        assert!(Activity::try_from(bad_name).is_err());
    }

    #[test]
    fn endings_say_why() {
        let mut ended = started(None, 1);
        ended.phase = Some(daemon::activity::Phase::Ended(daemon::CallEnded {
            outcome: Some(daemon::call_ended::Outcome::TimedOut("no window".into())),
            seen: None,
            refs: None,
            read: None,
        }));
        assert_eq!(
            Activity::try_from(ended)
                .ok()
                .map(|activity| activity.phase),
            Some(Phase::Ended(Ended {
                ending: Ending::TimedOut("no window".into()),
                seen: None,
                refs: None,
                read: None,
            }))
        );
    }

    #[test]
    fn names_are_read_and_a_generation_of_zero_is_skipped() {
        let named = |generation| {
            let mut event = started(None, 1);
            event.phase = Some(daemon::activity::Phase::Named(daemon::ElementRefs {
                snapshot: 7,
                generation,
                nodes: vec![3, 5],
            }));
            Activity::try_from(event)
                .ok()
                .map(|activity| activity.phase)
        };
        let Some(Phase::Named(refs)) = named(2) else {
            panic!("expected names");
        };
        let refs: Vec<_> = refs
            .refs()
            .map(|(node, r)| (node.get(), r.to_string()))
            .collect();
        assert_eq!(refs, [(3, "s2e1".to_owned()), (5, "s2e2".to_owned())]);
        assert_eq!(named(0), None);
    }

    #[test]
    fn a_recording_keeps_what_it_can_read() {
        let recording = Recording::try_from(daemon::Recording {
            id: "2026-11-14T06-15-42Z-s1".into(),
            videos: vec![
                daemon::RecordingVideo {
                    file: "live-001.mp4".into(),
                    machine: "live".into(),
                    start_ms: Some(250),
                    display_points: Some(crate::pb::common::v1::Size {
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
