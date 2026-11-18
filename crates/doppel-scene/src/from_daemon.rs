//! What the daemon tells a console, in the scene's words: the activity stream and a
//! recording as parsed (`doppel_machine::activity` and `::recording`)
//! become the events and trace the overlay draws. The stream keeps what
//! this build doesn't know and times in `Duration`; the scene folds
//! unknowns into `Other` or `Done`, failures into one `Failed`, and times
//! into milliseconds.

use std::sync::Arc;

use doppel_machine::activity as told;
use doppel_machine::recording as kept;

use crate::recordings::{
    Actor, Recording, RecordingId, TraceCall, TraceLine, TraceOutcome, Video, VideoName,
};
use crate::timeline::{
    ActivityEvent, Call, CallKey, Callout, CalloutLasts, CalloutOn, CalloutShape, Ending, Gesture,
    ReadLine, RecentEnd, Target, TextRead,
};

impl ActivityEvent {
    /// An event of the activity stream, as the overlay takes it in.
    #[must_use]
    pub fn of(activity: told::Activity) -> Self {
        let key = CallKey {
            session: activity.session,
            request: activity.request,
        };
        let call = |acted: told::OnScreen| {
            let why = acted.why.clone();
            let (gesture, target, callout) = on_screen(acted);
            Call {
                key,
                machine: activity.machine.clone(),
                client: activity.client.clone(),
                caption: activity.caption.clone(),
                gesture,
                target,
                callout,
                why,
            }
        };
        match activity.phase {
            told::Phase::Started(acted) => Self::Started(call(acted)),
            told::Phase::Ended(ended) => Self::Ended {
                key,
                machine: activity.machine,
                ending: Ending::of(ended.ending),
                seen: ended.seen.map(|seen| Arc::new(*seen)),
                read: ended.read.map(|read| Arc::new(TextRead::of(&read))),
            },
            told::Phase::Named(refs) => Self::Named {
                machine: activity.machine,
                refs: Arc::new(refs),
            },
            told::Phase::Recent {
                acted,
                started_ago,
                ended,
            } => Self::Recent {
                call: call(acted),
                started_ms_ago: started_ago.as_secs_f64() * 1000.0,
                ended: ended.map(|(ended, ago)| {
                    Box::new(RecentEnd {
                        ending: Ending::of(ended.ending),
                        seen: ended.seen.map(|seen| Arc::new(*seen)),
                        refs: ended.refs.map(Arc::new),
                        read: ended.read.map(|read| Arc::new(TextRead::of(&read))),
                        ms_ago: ago.as_secs_f64() * 1000.0,
                    })
                }),
            },
        }
    }
}

impl TextRead {
    /// What a call read: lines and matches in display points.
    #[must_use]
    pub fn of(read: &told::TextRead) -> Self {
        Self {
            lines: read
                .text
                .lines()
                .iter()
                .map(|line| ReadLine {
                    text: line.text().to_owned(),
                    frame: line.region(),
                })
                .collect(),
            query: read.query.as_ref().map(|query| query.as_str().to_owned()),
            found: read.found.clone(),
        }
    }
}

impl Ending {
    /// An ending this build can't read still ends the call; a refusal or
    /// a time-out failed it.
    fn of(ending: told::Ending) -> Self {
        match ending {
            told::Ending::Done | told::Ending::Unknown => Self::Done,
            told::Ending::Failed(why) | told::Ending::Refused(why) => Self::Failed(why),
            told::Ending::TimedOut(why) if why.is_empty() => Self::Failed("timed out".into()),
            told::Ending::TimedOut(why) => Self::Failed(format!("timed out: {why}")),
        }
    }
}

/// What a call did on the screen. A mark's place is its own, not where
/// the agent's pointer goes: its call has no target.
fn on_screen(acted: told::OnScreen) -> (Gesture, Target, Option<Arc<Callout>>) {
    let gesture = Gesture::of(acted.gesture);
    match acted.callout {
        None => (gesture, Target::of(acted.target), None),
        Some(callout) => {
            let on = match Target::of(acted.target) {
                Target::Point(at) => Some(CalloutOn::Point(at)),
                Target::Element { frame, label } => Some(CalloutOn::Area { frame, label }),
                Target::Drag { .. } | Target::Nowhere => None,
            };
            let callout = on.map(|on| {
                Arc::new(Callout {
                    on,
                    shape: match callout.shape {
                        told::CalloutShape::Dot => CalloutShape::Dot,
                        told::CalloutShape::Arrow { from } => CalloutShape::Arrow { from },
                        // A shape a newer daemon added is drawn as a box.
                        told::CalloutShape::Box | told::CalloutShape::Unknown(_) => {
                            CalloutShape::Box
                        }
                    },
                    note: callout.note,
                    lasts: match callout.lasts {
                        told::CalloutLasts::For(time) => {
                            CalloutLasts::For(time.as_secs_f64() * 1000.0)
                        }
                        told::CalloutLasts::UntilActed(time) => {
                            CalloutLasts::UntilActed(time.as_secs_f64() * 1000.0)
                        }
                    },
                })
            });
            (gesture, Target::Nowhere, callout)
        }
    }
}

impl Gesture {
    /// A gesture a newer daemon added is drawn as any other call.
    const fn of(gesture: told::Gesture) -> Self {
        match gesture {
            told::Gesture::Click => Self::Click,
            told::Gesture::DoubleClick => Self::DoubleClick,
            told::Gesture::SecondaryClick => Self::SecondaryClick,
            told::Gesture::Move => Self::Move,
            told::Gesture::Drag => Self::Drag,
            told::Gesture::Scroll => Self::Scroll,
            told::Gesture::Key => Self::Key,
            told::Gesture::Type => Self::Type,
            told::Gesture::Act => Self::Act,
            told::Gesture::Look => Self::Look,
            told::Gesture::Wait => Self::Wait,
            told::Gesture::Run => Self::Run,
            told::Gesture::Lifecycle => Self::Lifecycle,
            told::Gesture::Checkpoint => Self::Checkpoint,
            told::Gesture::Rollback => Self::Rollback,
            told::Gesture::Mark => Self::Mark,
            told::Gesture::Other | told::Gesture::Unknown(_) => Self::Other,
        }
    }
}

impl Target {
    fn of(target: Option<told::Target>) -> Self {
        match target {
            Some(told::Target::Point(at)) => Self::Point(at),
            Some(told::Target::Element { frame, label }) => Self::Element { frame, label },
            Some(told::Target::Drag { from, to }) => Self::Drag { from, to },
            None => Self::Nowhere,
        }
    }
}

impl Recording {
    /// A recording, opened, as replay shows it.
    #[must_use]
    pub fn of(recording: kept::Recording) -> Self {
        Self {
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
            trace: recording.trace.into_iter().map(TraceLine::of).collect(),
        }
    }
}

impl TraceLine {
    fn of(line: kept::TraceLine) -> Self {
        match line {
            kept::TraceLine::Call(call) => {
                let why = call.acted.as_ref().and_then(|acted| acted.why.clone());
                let (acted, callout) = match call.acted {
                    Some(acted) => {
                        let (gesture, target, callout) = on_screen(*acted);
                        (Some((gesture, target)), callout)
                    }
                    None => (None, None),
                };
                Self::Call(Box::new(TraceCall {
                    why,
                    start_ms: call.start_ms,
                    end_ms: call.end_ms,
                    machine: call.machine,
                    call: call.call,
                    outcome: match call.outcome {
                        kept::TraceOutcome::Done => TraceOutcome::Done,
                        kept::TraceOutcome::Failed => TraceOutcome::Failed,
                        kept::TraceOutcome::TimedOut => TraceOutcome::TimedOut,
                        kept::TraceOutcome::Refused => TraceOutcome::Refused,
                        kept::TraceOutcome::Unknown(raw) => TraceOutcome::Unknown(raw),
                    },
                    error: call.error,
                    waited_ms: call.waited_ms,
                    queued_ms: call.queued_ms,
                    by: match call.by {
                        kept::TraceActor::Session => Actor::Session,
                        kept::TraceActor::Person => Actor::Person,
                        kept::TraceActor::Unknown(raw) => Actor::Unknown(raw),
                    },
                    acted,
                    callout,
                    seen: call.seen,
                    refs: call.refs.map(|refs| Arc::new(*refs)),
                    read: call.read.map(|read| Arc::new(TextRead::of(&read))),
                }))
            }
            kept::TraceLine::Unreadable(raw) => Self::Unreadable(raw),
        }
    }
}

#[cfg(test)]
mod tests {
    use doppel_desktop::geometry::Point;

    use super::*;

    #[test]
    fn activity_keeps_its_call_and_ending() {
        let started = ActivityEvent::of(told::Activity {
            session: 2,
            request: 5,
            machine: "dev".parse().unwrap(),
            client: "claude".into(),
            caption: "click OK".into(),
            phase: told::Phase::Started(told::OnScreen {
                why: None,
                gesture: told::Gesture::Unknown(77),
                target: Some(told::Target::Point(Point::new(1.0, 2.0).unwrap())),
                callout: None,
            }),
        });
        let ActivityEvent::Started(call) = started else {
            panic!("a start");
        };
        assert_eq!(
            call.key,
            CallKey {
                session: 2,
                request: 5
            }
        );
        assert_eq!(call.gesture, Gesture::Other);
        assert_eq!(call.target, Target::Point(Point::new(1.0, 2.0).unwrap()));
        let ended = ActivityEvent::of(told::Activity {
            session: 2,
            request: 5,
            machine: "dev".parse().unwrap(),
            client: String::new(),
            caption: String::new(),
            phase: told::Phase::Ended(told::Ended {
                ending: told::Ending::TimedOut(String::new()),
                seen: None,
                refs: None,
                read: None,
            }),
        });
        assert!(matches!(
            ended,
            ActivityEvent::Ended { ending: Ending::Failed(why), .. } if why == "timed out"
        ));
    }
}
