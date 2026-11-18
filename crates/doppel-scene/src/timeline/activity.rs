//! The activity stream as `doppel_machine::activity` parses it, taken in:
//! what this build doesn't know folds into `Other` or `Done`, failures into
//! one `Failed`, and times into milliseconds.

use std::sync::Arc;

use doppel_machine::activity as told;

use super::{
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
pub(crate) fn on_screen(acted: told::OnScreen) -> (Gesture, Target, Option<Arc<Callout>>) {
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
