use doppel_machine::activity as told;

use super::*;

#[test]
fn activity_keeps_its_call_and_ending() {
    let started = ActivityEvent::of(told::Activity {
        session: 2.into(),
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
            session: 2.into(),
            request: 5
        }
    );
    assert_eq!(call.gesture, Gesture::Other);
    assert_eq!(call.target, Target::Point(Point::new(1.0, 2.0).unwrap()));
    let ended = ActivityEvent::of(told::Activity {
        session: 2.into(),
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

fn call(request: u64, gesture: Gesture, target: Target) -> Call {
    Call {
        key: CallKey {
            session: 1.into(),
            request,
        },
        machine: "dev".parse().unwrap(),
        client: "claude".into(),
        caption: "a call".into(),
        gesture,
        target,
        callout: None,
        why: None,
    }
}

fn at(x: f64, y: f64) -> Point {
    Point::new(x, y).unwrap()
}

fn ended(request: u64, ending: Ending) -> ActivityEvent {
    ActivityEvent::Ended {
        key: CallKey {
            session: 1.into(),
            request,
        },
        machine: "dev".parse().unwrap(),
        ending,
        seen: None,
        read: None,
    }
}

#[test]
fn the_snapshot_shown_is_the_latest_taken_and_says_when_the_agent_acted_since() {
    use doppel_desktop::agent::{ProcessId, RunningApp};
    use doppel_desktop::ax::tree::{AxNode, AxTree, Role, SnapshotId, States, Truncation};
    let snapshot = |id| {
        Arc::new(AxSnapshot {
            id: SnapshotId::new(id),
            app: RunningApp {
                pid: ProcessId::new(1),
                bundle_id: None,
                name: "Calculator".into(),
            },
            tree: AxTree::new(vec![(
                None,
                AxNode {
                    role: Role::Window,
                    subrole: None,
                    title: None,
                    description: None,
                    identifier: None,
                    value: None,
                    frame: None,
                    states: States::default(),
                    actions: Vec::new(),
                },
            )])
            .unwrap(),
            truncation: Truncation::Complete,
        })
    };
    let look = |request, id, start: f64, timeline: &mut Timeline| {
        timeline.record(
            ActivityEvent::Started(call(request, Gesture::Look, Target::Nowhere)),
            start,
        );
        timeline.record(
            ActivityEvent::Ended {
                key: CallKey {
                    session: 1.into(),
                    request,
                },
                machine: "dev".parse().unwrap(),
                ending: Ending::Done,
                seen: Some(snapshot(id)),
                read: None,
            },
            start + 100.0,
        );
    };
    let mut timeline = Timeline::new();
    look(1, 3, 0.0, &mut timeline);
    timeline.record(
        ActivityEvent::Started(call(2, Gesture::Wait, Target::Nowhere)),
        200.0,
    );
    timeline.record(ended(2, Ending::Done), 300.0);
    timeline.record(
        ActivityEvent::Started(call(3, Gesture::Key, Target::Nowhere)),
        400.0,
    );
    timeline.record(ended(3, Ending::Done), 450.0);
    look(4, 4, 500.0, &mut timeline);
    let id = |at| match timeline.seen_at(at).map(|shown| shown.seen) {
        Some(Seen::Live(seen)) => Some(seen.id.get()),
        Some(Seen::Recorded(_)) | None => None,
    };
    assert_eq!(id(50.0), None);
    assert_eq!(id(300.0), Some(3));
    assert_eq!(
        timeline
            .seen_at(300.0)
            .map(|shown| (shown.since_ms, shown.acted_since)),
        Some((100.0, false))
    );
    // A key press may change the screen; a wait doesn't.
    assert_eq!(
        timeline.seen_at(420.0).map(|shown| shown.acted_since),
        Some(true)
    );
    assert_eq!(id(700.0), Some(4));
    assert_eq!(
        timeline.seen_at(700.0).map(|shown| shown.acted_since),
        Some(false)
    );
    // Names reach the call that saw their snapshot, and come with it.
    use doppel_desktop::ax::refs::Generation;
    use doppel_desktop::ax::tree::NodeIndex;
    let named = |id| ActivityEvent::Named {
        machine: "dev".parse().unwrap(),
        refs: Arc::new(SnapshotRefs::new(
            SnapshotId::new(id),
            Generation::FIRST,
            vec![NodeIndex::ROOT],
        )),
    };
    timeline.record(named(3), 710.0);
    timeline.record(named(9), 720.0);
    let generation = |at| {
        timeline
            .seen_at(at)
            .and_then(|shown| shown.refs)
            .map(|refs| refs.generation())
    };
    assert_eq!(generation(300.0), Some(Generation::FIRST));
    assert_eq!(generation(700.0), None);
}

#[test]
fn a_recent_call_is_placed_back_in_time_once() {
    let mut timeline = Timeline::new();
    let recent = |request| ActivityEvent::Recent {
        call: call(request, Gesture::Click, Target::Nowhere),
        started_ms_ago: 5000.0,
        ended: Some(Box::new(RecentEnd {
            ending: Ending::Failed("no window".into()),
            seen: None,
            read: None,
            refs: None,
            ms_ago: 4000.0,
        })),
    };
    timeline.record(recent(1), 10_000.0);
    timeline.record(
        ActivityEvent::Recent {
            call: call(2, Gesture::Wait, Target::Nowhere),
            started_ms_ago: 9000.0,
            ended: None,
        },
        10_000.0,
    );
    // In start order, the one under way still open.
    let placed: Vec<(f64, Option<f64>)> = timeline
        .moments()
        .iter()
        .map(|moment| (moment.start_ms, moment.end_ms))
        .collect();
    assert_eq!(placed, vec![(1000.0, None), (5000.0, Some(6000.0))]);
    assert_eq!(timeline.moments()[1].failed.as_deref(), Some("no window"));
    // Told again, it stays as it is.
    timeline.record(recent(1), 20_000.0);
    assert_eq!(timeline.moments().len(), 2);
}

#[test]
fn the_reading_band_finishes_its_pass_after_a_look_ends() {
    let look = || ActivityEvent::Started(call(1, Gesture::Look, Target::Nowhere));
    let scan = |timeline: &Timeline, at: f64| timeline.scene_at(at).of(1.into()).unwrap().scan;
    let mut quick = Timeline::new();
    quick.record(look(), 0.0);
    quick.record(ended(1, Ending::Done), 300.0);
    assert_eq!(scan(&quick, 400.0), Some(0.0));
    assert_eq!(scan(&quick, SWEEP_MS - 1.0), Some(0.0));
    assert_eq!(scan(&quick, SWEEP_MS), None);
    // A longer look runs to the end of the pass it ends in.
    let mut long = Timeline::new();
    long.record(look(), 0.0);
    long.record(ended(1, Ending::Done), SWEEP_MS + 100.0);
    assert_eq!(scan(&long, 2.0 * SWEEP_MS - 1.0), Some(0.0));
    assert_eq!(scan(&long, 2.0 * SWEEP_MS), None);
    // One still looking keeps sweeping, each pass from the top.
    let mut open = Timeline::new();
    open.record(look(), 0.0);
    assert_eq!(scan(&open, 10.0 * SWEEP_MS), Some(0.0));
    assert_eq!(swept(500.0, 500.0), 0.0);
    assert_eq!(swept(0.0, SWEEP_MS / 2.0), 0.5);
    assert_eq!(swept(0.0, SWEEP_MS), 0.0);
}

#[test]
fn a_call_is_under_way_until_it_ends_and_a_little_after() {
    let mut timeline = Timeline::new();
    timeline.record(
        ActivityEvent::Started(call(1, Gesture::Click, Target::Point(at(10.0, 20.0)))),
        0.0,
    );
    let scene = timeline.scene_at(100.0);
    assert_eq!(scene.of(1.into()).unwrap().cursor, Some(at(10.0, 20.0)));
    assert!(scene.of(1.into()).unwrap().current.is_some());
    assert!(matches!(
        scene.of(1.into()).unwrap().marks.as_slice(),
        [Mark::Ripple { .. }]
    ));
    timeline.record(ended(1, Ending::Done), 200.0);
    assert!(
        timeline
            .scene_at(500.0)
            .of(1.into())
            .unwrap()
            .current
            .is_some(),
        "shown at least 600 ms"
    );
    assert!(
        timeline
            .scene_at(700.0)
            .of(1.into())
            .unwrap()
            .current
            .is_none()
    );
    assert!(
        timeline
            .scene_at(2000.0)
            .of(1.into())
            .unwrap()
            .marks
            .is_empty(),
        "the ripple faded"
    );
}

#[test]
fn each_session_has_its_own_pointer_call_and_lane() {
    let mut timeline = Timeline::new();
    let by = |session, request, gesture, target| Call {
        key: CallKey { session, request },
        client: format!("agent {}", SessionId::get(session)),
        ..call(request, gesture, target)
    };
    timeline.record(
        ActivityEvent::Started(by(4.into(), 1, Gesture::Click, Target::Point(at(1.0, 1.0)))),
        0.0,
    );
    timeline.record(
        ActivityEvent::Started(by(2.into(), 1, Gesture::Click, Target::Point(at(9.0, 9.0)))),
        10.0,
    );
    timeline.record(
        ActivityEvent::Started(by(4.into(), 2, Gesture::Type, Target::Nowhere)),
        20.0,
    );
    let scene = timeline.scene_at(30.0);
    let sessions: Vec<u64> = scene
        .agents
        .iter()
        .map(|agent| agent.session.get())
        .collect();
    assert_eq!(sessions, [2, 4], "by session id");
    let four = scene.of(4.into()).unwrap();
    assert_eq!(four.cursor, Some(at(1.0, 1.0)));
    assert_eq!(four.client, "agent 4");
    assert_eq!(
        four.current.as_ref().map(|(call, _)| call.gesture),
        Some(Gesture::Type)
    );
    let two = scene.of(2.into()).unwrap();
    assert_eq!(two.cursor, Some(at(9.0, 9.0)));
    assert_eq!(
        two.current.as_ref().map(|(call, _)| call.key.request),
        Some(1)
    );
    let lanes = lanes(timeline.moments());
    let lanes: Vec<(u64, &str, usize)> = lanes
        .iter()
        .map(|lane| (lane.session.get(), lane.client, lane.moments.len()))
        .collect();
    assert_eq!(lanes, [(2, "agent 2", 1), (4, "agent 4", 2)]);
}

#[test]
fn a_mark_goes_just_after_its_call_ends_or_when_the_next_starts() {
    let press = || {
        call(
            1,
            Gesture::Act,
            Target::Element {
                frame: Rect::new(10.0, 10.0, 80.0, 30.0).unwrap(),
                label: "Save".into(),
            },
        )
    };
    let mut timeline = Timeline::new();
    timeline.record(ActivityEvent::Started(press()), 0.0);
    let marks = |timeline: &Timeline, at| timeline.scene_at(at).of(1.into()).unwrap().marks.len();
    assert_eq!(marks(&timeline, 900.0), 1, "under way, it shows");
    timeline.record(ended(1, Ending::Done), 30.0);
    let scene = timeline.scene_at(100.0);
    let shown = &scene.of(1.into()).unwrap().marks[0];
    assert_eq!(shown.until(), 30.0 + SETTLE_MS);
    assert_eq!(shown.opacity(30.0 + SETTLE_MS - MARK_FADE_MS), 1.0);
    assert_eq!(
        marks(&timeline, 30.0 + SETTLE_MS),
        0,
        "gone once the screen changed"
    );

    let mut timeline = Timeline::new();
    timeline.record(ActivityEvent::Started(press()), 0.0);
    timeline.record(
        ActivityEvent::Started(call(2, Gesture::Look, Target::Nowhere)),
        80.0,
    );
    assert_eq!(marks(&timeline, 90.0), 0, "the next call took over");
}

#[test]
fn a_failure_marks_where_the_pointer_was() {
    let mut timeline = Timeline::new();
    timeline.record(
        ActivityEvent::Started(call(1, Gesture::Move, Target::Point(at(5.0, 5.0)))),
        0.0,
    );
    timeline.record(
        ActivityEvent::Started(call(2, Gesture::Type, Target::Nowhere)),
        10.0,
    );
    timeline.record(ended(2, Ending::Failed("no focus".into())), 50.0);
    let scene = timeline.scene_at(60.0);
    assert!(scene.of(1.into()).unwrap().marks.contains(&Mark::Failed {
        at: at(5.0, 5.0),
        reason: "no focus".into(),
        since: 50.0,
        until: 50.0 + MARK_MS,
    }));
    assert_eq!(
        scene
            .of(1.into())
            .unwrap()
            .current
            .as_ref()
            .map(|(call, _)| call.key.request),
        Some(2)
    );
}

#[test]
fn calls_read_by_what_they_did() {
    let element = Target::Element {
        frame: Rect::new(0.0, 0.0, 10.0, 10.0).unwrap(),
        label: "OK".into(),
    };
    assert_eq!(call(1, Gesture::Act, element.clone()).describe(), "“OK”");
    assert_eq!(element.aim(), Some(at(5.0, 5.0)));
    assert_eq!(
        call(1, Gesture::Look, Target::Nowhere).describe(),
        "Looking at the screen"
    );
    assert_eq!(
        call(1, Gesture::Other, Target::Nowhere).describe(),
        "a call"
    );
}

#[test]
fn a_timeline_keeps_its_latest_calls() {
    let mut timeline = Timeline::new();
    for request in 0..(KEPT as u64 + 5) {
        timeline.record(
            ActivityEvent::Started(call(request, Gesture::Wait, Target::Nowhere)),
            0.0,
        );
    }
    assert_eq!(timeline.moments().len(), KEPT);
    assert_eq!(
        timeline
            .moments()
            .first()
            .map(|moment| moment.call.key.request),
        Some(5)
    );
}

#[test]
fn a_timeline_moves_while_a_call_runs_or_a_mark_fades() {
    let mut timeline = Timeline::new();
    timeline.record(
        ActivityEvent::Started(call(1, Gesture::Wait, Target::Nowhere)),
        0.0,
    );
    assert!(timeline.moving(10_000.0));
    timeline.record(ended(1, Ending::Done), 100.0);
    assert!(timeline.moving(1000.0));
    assert!(!timeline.moving(5000.0));
    assert!((timeline.latest(5000.0) - 100.0).abs() < f64::EPSILON);
}

#[test]
fn the_panel_shows_the_newer_of_the_agents_snapshot_and_a_persons_look() {
    let mut timeline = Timeline::new();
    timeline.record(
        ActivityEvent::Started(call(1, Gesture::Look, Target::Nowhere)),
        0.0,
    );
    timeline.record(
        ActivityEvent::Ended {
            key: CallKey {
                session: 1.into(),
                request: 1,
            },
            machine: "dev".parse().unwrap(),
            ending: Ending::Done,
            seen: Some(Arc::new(snapshot_with_id(5))),
            read: None,
        },
        100.0,
    );
    let look = Look::new(Ticket::default(), 200.0);
    assert!(matches!(timeline.showing(None, 300.0), Showing::Agent(_)));
    assert!(matches!(
        timeline.showing(Some(&look), 300.0),
        Showing::Person {
            acted_since: false,
            ..
        }
    ));
    // A key press after the look may have changed the screen.
    timeline.record(
        ActivityEvent::Started(call(2, Gesture::Key, Target::Nowhere)),
        400.0,
    );
    timeline.record(ended(2, Ending::Done), 450.0);
    assert!(matches!(
        timeline.showing(Some(&look), 500.0),
        Showing::Person {
            acted_since: true,
            ..
        }
    ));
    // The agent's next snapshot is newer than the look.
    timeline.record(
        ActivityEvent::Started(call(3, Gesture::Look, Target::Nowhere)),
        600.0,
    );
    timeline.record(
        ActivityEvent::Ended {
            key: CallKey {
                session: 1.into(),
                request: 3,
            },
            machine: "dev".parse().unwrap(),
            ending: Ending::Done,
            seen: Some(Arc::new(snapshot_with_id(6))),
            read: None,
        },
        700.0,
    );
    assert!(matches!(
        timeline.showing(Some(&look), 800.0),
        Showing::Agent(SeenAt {
            since_ms: 700.0,
            ..
        })
    ));
    assert_eq!(Timeline::new().showing(None, 0.0), Showing::Nothing);
}

#[test]
fn the_text_layer_shows_the_newer_of_the_agents_read_and_a_persons() {
    let read = |text: &str| {
        Some(Arc::new(TextRead {
            lines: vec![ReadLine {
                text: text.into(),
                frame: Rect::new(0.0, 0.0, 10.0, 10.0).unwrap(),
            }],
            query: None,
            found: Vec::new(),
        }))
    };
    let mut timeline = Timeline::new();
    let find = |request, start: f64, text: &str, timeline: &mut Timeline| {
        timeline.record(
            ActivityEvent::Started(call(request, Gesture::Look, Target::Nowhere)),
            start,
        );
        timeline.record(
            ActivityEvent::Ended {
                key: CallKey {
                    session: 1.into(),
                    request,
                },
                machine: "dev".parse().unwrap(),
                ending: Ending::Done,
                seen: None,
                read: read(text),
            },
            start + 50.0,
        );
    };
    find(1, 0.0, "Save", &mut timeline);
    let words = |shown: Reading<'_>| match shown {
        Reading::Agent(read) => Some(read.read.lines[0].text.clone()),
        Reading::Person { .. } | Reading::Nothing => None,
    };
    assert_eq!(words(timeline.reading(None, 10.0)), None);
    assert_eq!(words(timeline.reading(None, 100.0)), Some("Save".into()));
    let mine = TextLook::new(Ticket::default(), 200.0);
    assert!(matches!(
        timeline.reading(Some(&mine), 300.0),
        Reading::Person {
            acted_since: false,
            ..
        }
    ));
    find(2, 400.0, "Open", &mut timeline);
    assert_eq!(
        words(timeline.reading(Some(&mine), 500.0)),
        Some("Open".into())
    );
    // A call that only read doesn't make the read stale.
    assert_eq!(
        timeline.read_at(500.0).map(|read| read.acted_since),
        Some(false)
    );
}

fn snapshot_with_id(id: u64) -> AxSnapshot {
    use doppel_desktop::agent::{ProcessId, RunningApp};
    use doppel_desktop::ax::tree::{AxNode, AxTree, Role, SnapshotId, States, Truncation};
    AxSnapshot {
        id: SnapshotId::new(id),
        app: RunningApp {
            pid: ProcessId::new(1),
            bundle_id: None,
            name: "Calculator".into(),
        },
        tree: AxTree::new(vec![(
            None,
            AxNode {
                role: Role::Window,
                subrole: None,
                title: None,
                description: None,
                identifier: None,
                value: None,
                frame: None,
                states: States::default(),
                actions: Vec::new(),
            },
        )])
        .unwrap(),
        truncation: Truncation::Complete,
    }
}

fn marked(request: u64, lasts: CalloutLasts) -> Call {
    Call {
        callout: Some(Arc::new(Callout {
            on: CalloutOn::Point(at(40.0, 50.0)),
            shape: CalloutShape::Dot,
            note: Some("here".into()),
            lasts,
        })),
        ..call(request, Gesture::Mark, Target::Nowhere)
    }
}

#[test]
fn a_mark_shows_for_its_time_without_moving_the_pointer() {
    let mut timeline = Timeline::new();
    timeline.record(
        ActivityEvent::Started(call(1, Gesture::Click, Target::Point(at(1.0, 2.0)))),
        0.0,
    );
    timeline.record(ended(1, Ending::Done), 10.0);
    timeline.record(
        ActivityEvent::Started(marked(2, CalloutLasts::For(5000.0))),
        100.0,
    );
    timeline.record(ended(2, Ending::Done), 101.0);
    let scene = timeline.scene_at(2000.0);
    assert_eq!(scene.of(1.into()).unwrap().cursor, Some(at(1.0, 2.0)));
    assert_eq!(scene.callouts.len(), 1);
    assert_eq!(scene.callouts[0].until, 5100.0);
    assert!(timeline.moving(4000.0));
    assert!(timeline.scene_at(5100.0).callouts.is_empty());
    assert!(!timeline.moving(5200.0));
    assert_eq!(timeline.moments()[1].call.describe(), "Pointing out: here");
}

#[test]
fn a_mark_until_acted_goes_when_the_agent_next_changes_the_screen() {
    let mut timeline = Timeline::new();
    timeline.record(
        ActivityEvent::Started(marked(1, CalloutLasts::UntilActed(5000.0))),
        0.0,
    );
    timeline.record(ended(1, Ending::Done), 1.0);
    // Looking changes nothing: the mark stays.
    timeline.record(
        ActivityEvent::Started(call(2, Gesture::Look, Target::Nowhere)),
        300.0,
    );
    timeline.record(
        ActivityEvent::Started(call(3, Gesture::Click, Target::Point(at(40.0, 50.0)))),
        900.0,
    );
    assert_eq!(timeline.scene_at(800.0).callouts.len(), 1);
    assert!(timeline.scene_at(900.0).callouts.is_empty());
}

#[test]
fn a_refused_mark_shows_nothing() {
    let mut timeline = Timeline::new();
    timeline.record(
        ActivityEvent::Started(marked(1, CalloutLasts::For(5000.0))),
        0.0,
    );
    timeline.record(ended(1, Ending::Failed("no".into())), 1.0);
    assert!(timeline.scene_at(500.0).callouts.is_empty());
}

#[test]
fn calls_added_out_of_order_take_their_place_by_start() {
    let mut timeline = Timeline::new();
    for (request, start_ms) in [(1, 300.0), (2, 100.0), (3, 300.0), (4, 200.0)] {
        timeline.add(Moment {
            start_ms,
            end_ms: Some(start_ms + 50.0),
            call: call(request, Gesture::Click, Target::Nowhere),
            failed: None,
            person: false,
            seen: None,
            refs: None,
            read: None,
        });
    }
    let order: Vec<u64> = timeline
        .moments()
        .iter()
        .map(|moment| moment.call.key.request)
        .collect();
    // Calls that started together keep the order they came in.
    assert_eq!(order, [2, 4, 1, 3]);
}
