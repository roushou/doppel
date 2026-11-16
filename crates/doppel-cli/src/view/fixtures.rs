//! Cross-language contract: what the Rust viewer's scene makes of the
//! daemon's wire, pinned under `fixtures/scene/` for the browser viewer to
//! check its own against (`web/src/lib/scene.test.ts`). The inputs are wire
//! bytes, read here as the window reads them (the client, then the port);
//! the outputs are the scene's, in the page's shapes, so a difference
//! between the two viewers fails one suite or the other.
//!
//! Regenerate after an intentional change to the scene:
//! `DOPPEL_UPDATE_GOLDEN=1 cargo test -p doppel-cli fixtures`

use std::collections::HashSet;
use std::path::PathBuf;

use base64::Engine as _;
use doppel_machine::ax::tree::{AxSnapshot, NodeIndex};
use doppel_machine::geometry::{Point, Rect};
use doppel_protocol::pb::agent::v1 as agent;
use doppel_protocol::pb::common::v1 as common;
use doppel_protocol::pb::daemon::v1 as daemon;
use doppel_scene::elements::{self, Kind, Wrappers};
use doppel_scene::timeline::{
    Callout, CalloutLasts, CalloutOn, CalloutShape, Gesture, Mark, Moment, Scene, lanes, record,
    scene_at,
};
use prost::Message as _;
use serde_json::{Value, json};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

/// Compares `value` with the committed file, or writes it when asked to.
fn pinned(name: &str, value: &Value) {
    let path = fixtures().join("scene").join(name);
    let mut text = String::new();
    layout(value, 0, &mut text);
    text.push('\n');
    #[expect(
        clippy::disallowed_methods,
        reason = "test-only switch to regenerate fixtures"
    )]
    let update = std::env::var("DOPPEL_UPDATE_GOLDEN").is_ok();
    if update {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == text,
        "{} differs from what the scene makes now; if the change is meant, \
         regenerate it with DOPPEL_UPDATE_GOLDEN=1 and update the browser \
         viewer to match",
        path.display()
    );
}

/// How long a value may be and still go on one line.
const ONE_LINE: usize = 160;

/// `value` as indented JSON, but with a short value (a point, an element,
/// a tree row) or an array of plain ones (a line of the grid) on one line,
/// so the files stay small enough to read a change in.
fn layout(value: &Value, depth: usize, out: &mut String) {
    let pad = |depth: usize| "  ".repeat(depth);
    let plain = |value: &Value| !matches!(value, Value::Array(_) | Value::Object(_));
    let compact = serde_json::to_string(value).unwrap();
    match value {
        _ if compact.len() <= ONE_LINE => out.push_str(&compact),
        Value::Array(items) if items.iter().all(plain) => out.push_str(&compact),
        Value::Array(items) => {
            out.push_str("[\n");
            for (at, item) in items.iter().enumerate() {
                out.push_str(&pad(depth + 1));
                layout(item, depth + 1, out);
                out.push_str(if at + 1 == items.len() { "\n" } else { ",\n" });
            }
            out.push_str(&pad(depth));
            out.push(']');
        }
        Value::Object(fields) => {
            out.push_str("{\n");
            for (at, (key, item)) in fields.iter().enumerate() {
                out.push_str(&pad(depth + 1));
                out.push_str(&serde_json::to_string(key).unwrap());
                out.push_str(": ");
                layout(item, depth + 1, out);
                out.push_str(if at + 1 == fields.len() { "\n" } else { ",\n" });
            }
            out.push_str(&pad(depth));
            out.push('}');
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            out.push_str(&compact);
        }
    }
}

// --- The page's shapes -----------------------------------------------------

fn point(at: Point) -> Value {
    json!({ "x": at.x(), "y": at.y() })
}

fn rect(frame: Rect) -> Value {
    json!({
        "left": frame.left(),
        "top": frame.top(),
        "width": frame.width(),
        "height": frame.height(),
    })
}

const fn gesture(gesture: Gesture) -> &'static str {
    match gesture {
        Gesture::Click => "click",
        Gesture::DoubleClick => "double-click",
        Gesture::SecondaryClick => "secondary-click",
        Gesture::Move => "move",
        Gesture::Drag => "drag",
        Gesture::Scroll => "scroll",
        Gesture::Key => "key",
        Gesture::Type => "type",
        Gesture::Act => "act",
        Gesture::Look => "look",
        Gesture::Wait => "wait",
        Gesture::Run => "run",
        Gesture::Lifecycle => "lifecycle",
        Gesture::Mark => "mark",
        Gesture::Other => "other",
    }
}

fn callout(callout: &Callout) -> Value {
    json!({
        "on": match &callout.on {
            CalloutOn::Point(at) => json!({ "kind": "point", "at": point(*at) }),
            CalloutOn::Area { frame, label } => {
                json!({ "kind": "area", "frame": rect(*frame), "label": label })
            }
        },
        "shape": match callout.shape {
            CalloutShape::Box => json!({ "kind": "box" }),
            CalloutShape::Dot => json!({ "kind": "dot" }),
            CalloutShape::Arrow { from } => {
                json!({ "kind": "arrow", "from": from.map(point) })
            }
        },
        "note": callout.note,
        "lasts": match callout.lasts {
            CalloutLasts::For(ms) => json!({ "kind": "for", "ms": ms }),
            CalloutLasts::UntilActed(ms) => json!({ "kind": "until-acted", "ms": ms }),
        },
    })
}

fn mark(mark: &Mark) -> Value {
    match mark {
        Mark::Ripple {
            at,
            gesture: g,
            since,
        } => {
            json!({ "kind": "ripple", "at": point(*at), "gesture": gesture(*g), "since": since })
        }
        Mark::Box {
            frame,
            label,
            since,
        } => {
            json!({ "kind": "box", "frame": rect(*frame), "label": label, "since": since })
        }
        Mark::Path { from, to, since } => {
            json!({ "kind": "path", "from": point(*from), "to": point(*to), "since": since })
        }
        Mark::Failed { at, reason, since } => {
            json!({ "kind": "failed", "at": point(*at), "reason": reason, "since": since })
        }
    }
}

fn scene(scene: &Scene) -> Value {
    json!({
        "agents": scene.agents.iter().map(|agent| json!({
            "session": agent.session,
            "client": agent.client,
            "cursor": agent.cursor.map(point),
            "current": agent.current.as_ref().map(|(call, since)| json!({
                "caption": call.caption,
                "gesture": gesture(call.gesture),
                "since": since,
            })),
            "scan": agent.scan,
            "marks": agent.marks.iter().map(mark).collect::<Vec<_>>(),
            "active": agent.active,
        })).collect::<Vec<_>>(),
        "callouts": scene.callouts.iter().map(|shown| json!({
            "callout": callout(&shown.callout),
            "since": shown.since,
            "until": shown.until,
        })).collect::<Vec<_>>(),
    })
}

fn moment(moment: &Moment) -> Value {
    json!({
        "caption": moment.call.caption,
        "gesture": gesture(moment.call.gesture),
        "start": moment.start_ms,
        "end": moment.end_ms,
        "failed": moment.failed,
    })
}

// --- Activity on the wire --------------------------------------------------

fn at(x: f64, y: f64) -> common::Point {
    common::Point { x, y }
}

fn area(left: f64, top: f64, width: f64, height: f64, label: &str) -> daemon::ElementTarget {
    daemon::ElementTarget {
        frame: Some(common::Rect {
            origin: Some(at(left, top)),
            size: Some(common::Size { width, height }),
        }),
        label: label.into(),
    }
}

/// Who calls: a session, and its client's name.
#[derive(Clone, Copy)]
struct Session(u64, &'static str);

const AGENT: Session = Session(1, "agent");
const OTHER: Session = Session(2, "other agent");

fn activity(
    by: Session,
    request: u64,
    caption: &str,
    phase: daemon::activity::Phase,
) -> daemon::Activity {
    daemon::Activity {
        session: by.0,
        request,
        machine: "dev".into(),
        client: by.1.into(),
        caption: caption.into(),
        phase: Some(phase),
    }
}

fn call(gesture: i32, target: Option<daemon::call_started::Target>) -> daemon::CallStarted {
    daemon::CallStarted {
        gesture,
        target,
        mark: None,
    }
}

fn started(
    by: Session,
    request: u64,
    caption: &str,
    started: daemon::CallStarted,
) -> daemon::Activity {
    activity(
        by,
        request,
        caption,
        daemon::activity::Phase::Started(started),
    )
}

fn on_point(gesture: daemon::Gesture, x: f64, y: f64) -> daemon::CallStarted {
    call(
        gesture as i32,
        Some(daemon::call_started::Target::Point(at(x, y))),
    )
}

fn nowhere(gesture: daemon::Gesture) -> daemon::CallStarted {
    call(gesture as i32, None)
}

fn marked(mark: daemon::ScreenMark) -> daemon::CallStarted {
    daemon::CallStarted {
        gesture: daemon::Gesture::Mark as i32,
        target: None,
        mark: Some(mark),
    }
}

fn outcome(outcome: daemon::call_ended::Outcome) -> daemon::CallEnded {
    daemon::CallEnded {
        outcome: Some(outcome),
        seen: None,
        refs: None,
        read: None,
    }
}

fn done() -> daemon::CallEnded {
    outcome(daemon::call_ended::Outcome::Done(daemon::Done {}))
}

fn ended(by: Session, request: u64, ending: daemon::CallEnded) -> daemon::Activity {
    activity(by, request, "", daemon::activity::Phase::Ended(ending))
}

/// Events at instants on the page's clock, and the instants to look at.
struct Scenario {
    name: &'static str,
    events: Vec<(f64, daemon::Activity)>,
    instants: Vec<f64>,
}

#[expect(clippy::too_many_lines, reason = "one scenario after another")]
fn scenarios() -> Vec<Scenario> {
    use daemon::Gesture as G;
    use daemon::call_ended::Outcome;
    use daemon::call_started::Target;
    vec![
        Scenario {
            name: "a click, then typing",
            events: vec![
                (
                    0.0,
                    started(AGENT, 1, "click", on_point(G::Click, 10.0, 20.0)),
                ),
                (200.0, ended(AGENT, 1, done())),
                (400.0, started(AGENT, 2, "type", nowhere(G::Type))),
                (600.0, ended(AGENT, 2, done())),
            ],
            instants: vec![0.0, 100.0, 300.0, 500.0, 1700.0, 2100.0],
        },
        Scenario {
            name: "an element pressed, then a drag",
            events: vec![
                (
                    0.0,
                    started(
                        AGENT,
                        1,
                        "press OK",
                        call(
                            G::Act as i32,
                            Some(Target::Element(area(100.0, 100.0, 80.0, 20.0, "OK"))),
                        ),
                    ),
                ),
                (100.0, ended(AGENT, 1, done())),
                (
                    300.0,
                    started(
                        AGENT,
                        2,
                        "drag",
                        call(
                            G::Drag as i32,
                            Some(Target::Drag(daemon::DragTarget {
                                from: Some(at(10.0, 10.0)),
                                to: Some(at(200.0, 300.0)),
                            })),
                        ),
                    ),
                ),
                (900.0, ended(AGENT, 2, done())),
            ],
            instants: vec![50.0, 400.0, 1000.0, 2600.0],
        },
        Scenario {
            name: "a failure leaves a mark",
            events: vec![
                (
                    0.0,
                    started(AGENT, 1, "click", on_point(G::Click, 50.0, 60.0)),
                ),
                (
                    250.0,
                    ended(AGENT, 1, outcome(Outcome::Failed("no window".into()))),
                ),
            ],
            instants: vec![100.0, 300.0, 1500.0, 2000.0],
        },
        Scenario {
            name: "timed out, and refused",
            events: vec![
                (
                    0.0,
                    started(AGENT, 1, "click", on_point(G::Click, 5.0, 5.0)),
                ),
                (
                    100.0,
                    ended(AGENT, 1, outcome(Outcome::TimedOut(String::new()))),
                ),
                (200.0, started(AGENT, 2, "key", nowhere(G::Key))),
                (
                    300.0,
                    ended(
                        AGENT,
                        2,
                        outcome(Outcome::Refused("held by a person".into())),
                    ),
                ),
            ],
            instants: vec![150.0, 350.0, 2000.0],
        },
        Scenario {
            name: "looks sweep the screen",
            events: vec![
                (0.0, started(AGENT, 1, "snapshot", nowhere(G::Look))),
                (2000.0, ended(AGENT, 1, done())),
                (3000.0, started(AGENT, 2, "screenshot", nowhere(G::Look))),
                (3100.0, ended(AGENT, 2, done())),
            ],
            instants: vec![500.0, 1500.0, 2500.0, 3050.0, 3200.0, 5000.0],
        },
        Scenario {
            name: "marks for the people watching",
            events: vec![
                (
                    0.0,
                    started(
                        AGENT,
                        1,
                        "mark a dot",
                        marked(daemon::ScreenMark {
                            shape: daemon::MarkShape::Dot as i32,
                            at: Some(daemon::screen_mark::At::Point(at(40.0, 50.0))),
                            from: None,
                            note: " here ".into(),
                            lasts: Some(daemon::screen_mark::Lasts::ForMs(3000)),
                        }),
                    ),
                ),
                (10.0, ended(AGENT, 1, done())),
                (
                    500.0,
                    started(
                        AGENT,
                        2,
                        "mark a box",
                        marked(daemon::ScreenMark {
                            shape: daemon::MarkShape::Box as i32,
                            at: Some(daemon::screen_mark::At::Area(area(
                                10.0, 10.0, 100.0, 50.0, "Save",
                            ))),
                            from: None,
                            note: String::new(),
                            lasts: Some(daemon::screen_mark::Lasts::UntilActedMs(10_000)),
                        }),
                    ),
                ),
                (510.0, ended(AGENT, 2, done())),
                (
                    700.0,
                    started(
                        AGENT,
                        3,
                        "mark an arrow",
                        marked(daemon::ScreenMark {
                            shape: daemon::MarkShape::Arrow as i32,
                            at: Some(daemon::screen_mark::At::Area(area(
                                300.0, 200.0, 40.0, 40.0, "",
                            ))),
                            from: Some(at(0.0, 0.0)),
                            note: "look".into(),
                            lasts: Some(daemon::screen_mark::Lasts::ForMs(2000)),
                        }),
                    ),
                ),
                (710.0, ended(AGENT, 3, done())),
                (
                    4000.0,
                    started(AGENT, 4, "click", on_point(G::Click, 1.0, 1.0)),
                ),
                (4100.0, ended(AGENT, 4, done())),
            ],
            instants: vec![100.0, 600.0, 800.0, 2600.0, 3500.0, 4100.0, 11_000.0],
        },
        Scenario {
            name: "two sessions",
            events: vec![
                (
                    0.0,
                    started(AGENT, 1, "click", on_point(G::Click, 10.0, 10.0)),
                ),
                (50.0, ended(AGENT, 1, done())),
                (
                    100.0,
                    started(OTHER, 1, "click", on_point(G::Click, 300.0, 300.0)),
                ),
                (150.0, ended(OTHER, 1, done())),
                (
                    200.0,
                    started(AGENT, 2, "move", on_point(G::Move, 20.0, 20.0)),
                ),
            ],
            instants: vec![120.0, 250.0, 3000.0],
        },
        Scenario {
            name: "calls from before watching",
            events: vec![
                (
                    10_000.0,
                    activity(
                        AGENT,
                        1,
                        "click",
                        daemon::activity::Phase::Recent(daemon::CallRecent {
                            started: Some(on_point(G::Click, 30.0, 40.0)),
                            ended: Some(done()),
                            started_ms_ago: 5000,
                            ended_ms_ago: Some(4000),
                        }),
                    ),
                ),
                (
                    10_000.0,
                    activity(
                        AGENT,
                        2,
                        "wait",
                        daemon::activity::Phase::Recent(daemon::CallRecent {
                            started: Some(nowhere(G::Wait)),
                            ended: None,
                            started_ms_ago: 1000,
                            ended_ms_ago: None,
                        }),
                    ),
                ),
                (10_500.0, ended(AGENT, 2, done())),
            ],
            instants: vec![5000.0, 9500.0, 10_200.0, 10_600.0],
        },
        Scenario {
            name: "what this build doesn't know",
            events: vec![
                (0.0, started(AGENT, 1, "something new", call(77, None))),
                (
                    20.0,
                    started(
                        AGENT,
                        2,
                        "mark",
                        marked(daemon::ScreenMark {
                            shape: 0,
                            at: Some(daemon::screen_mark::At::Point(at(5.0, 6.0))),
                            from: None,
                            note: String::new(),
                            lasts: None,
                        }),
                    ),
                ),
                (40.0, ended(AGENT, 9, done())),
            ],
            instants: vec![10.0, 100.0, 6000.0],
        },
    ]
}

/// The moments and scenes each scenario makes, from wire bytes in.
#[test]
fn the_timeline_is_pinned() {
    let encode = |activity: &daemon::Activity| {
        base64::engine::general_purpose::STANDARD.encode(activity.encode_to_vec())
    };
    let out: Vec<Value> = scenarios()
        .into_iter()
        .map(|scenario| {
            let mut moments = Vec::new();
            for (now, wire) in &scenario.events {
                let bytes = wire.encode_to_vec();
                let read = doppel_client::Activity::from_wire(
                    daemon::Activity::decode(bytes.as_slice()).unwrap(),
                )
                .unwrap();
                record(&mut moments, super::port::activity(read), *now);
            }
            json!({
                "name": scenario.name,
                "events": scenario.events.iter().map(|(now, wire)| json!({
                    "at": now,
                    "activity": encode(wire),
                })).collect::<Vec<_>>(),
                "moments": moments.iter().map(moment).collect::<Vec<_>>(),
                "lanes": lanes(&moments).iter().map(|lane| json!({
                    "session": lane.session,
                    "client": lane.client,
                    "captions": lane.moments.iter().map(|m| &m.call.caption).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "scenes": scenario.instants.iter().map(|&instant| json!({
                    "at": instant,
                    "scene": scene(&scene_at(&moments, instant)),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    pinned(
        "timeline.v1.json",
        &json!({ "machine": "dev", "scenarios": out }),
    );
}

// --- Accessibility snapshots -----------------------------------------------

/// The snapshots recorded from a live guest (`fixtures/ax`).
const RECORDED: [&str; 4] = [
    "finder-window",
    "textedit-app-menus",
    "textedit-window",
    "vscode-welcome",
];

/// Where the element under a point is asked for: a grid over the display.
const GRID: (usize, usize, f64) = (32, 20, 40.0);

const fn kind(kind: Kind) -> &'static str {
    match kind {
        Kind::Window => "window",
        Kind::Control => "control",
        Kind::Field => "field",
        Kind::Text => "text",
        Kind::Container => "container",
        Kind::Menu => "menu",
        Kind::Other => "other",
    }
}

fn index(index: NodeIndex) -> u32 {
    index.get()
}

fn tree(snapshot: &AxSnapshot, wrappers: Wrappers) -> Value {
    let folded = elements::initially_folded(snapshot);
    let rows = elements::rows(snapshot, wrappers, &folded);
    let (columns, lines, step) = GRID;
    let grid: Vec<Vec<Option<u32>>> = (0..lines)
        .map(|row| {
            (0..columns)
                .map(|column| {
                    #[expect(clippy::cast_precision_loss, reason = "a small grid")]
                    let at = Point::new(
                        (column as f64).mul_add(step, step / 2.0),
                        (row as f64).mul_add(step, step / 2.0),
                    )
                    .unwrap();
                    elements::element_at(snapshot, at, wrappers).map(index)
                })
                .collect()
        })
        .collect();
    json!({
        "rows": rows.iter().map(|row| json!([
            index(row.index),
            row.depth,
            row.parent.map(index),
            row.branch,
            row.open,
            row.last,
            row.lines,
        ])).collect::<Vec<_>>(),
        "at": grid,
    })
}

/// What the tree panel and the elements layer make of recorded snapshots.
#[test]
fn the_elements_are_pinned() {
    let out: Vec<Value> = RECORDED
        .iter()
        .map(|name| {
            let file = format!("ax/{name}.pb");
            let bytes = std::fs::read(fixtures().join(&file)).unwrap();
            let snapshot: AxSnapshot = agent::AxSnapshot::decode(bytes.as_slice())
                .unwrap()
                .try_into()
                .unwrap();
            let mut folded: Vec<u32> = elements::initially_folded(&snapshot)
                .into_iter()
                .map(index)
                .collect();
            folded.sort_unstable();
            let unique: HashSet<u32> = folded.iter().copied().collect();
            assert_eq!(unique.len(), folded.len());
            json!({
                "snapshot": file,
                "elements": snapshot.tree.iter().map(|(at, node)| json!({
                    "index": index(at),
                    "kind": kind(Kind::of(&node.role)),
                    "name": node.name().unwrap_or_default(),
                    "value": elements::value(node),
                    "wrapper": elements::wrapper(node),
                    "seen": elements::seen(&snapshot, at).map(rect),
                })).collect::<Vec<_>>(),
                "folded": folded,
                "grid": { "columns": GRID.0, "rows": GRID.1, "step": GRID.2 },
                "hide": tree(&snapshot, Wrappers::Hide),
                "show": tree(&snapshot, Wrappers::Show),
            })
        })
        .collect();
    pinned("elements.v1.json", &json!({ "snapshots": out }));
}
