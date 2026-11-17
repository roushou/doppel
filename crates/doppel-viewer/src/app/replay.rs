//! The replay page: one recording's videos with what the agent did drawn
//! over them, a seekable strip of its calls, and the trace beside,
//! following the playhead.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use doppel_machine::ax::tree::AxSnapshot;

use doppel_machine::machine::MachineName;
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    AnyElement, Bounds, Context, Div, ElementId, FontWeight, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, Pixels, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement, Styled, Task, Window, canvas, div, prelude::FluentBuilder, px,
};

use crate::elements::Named;
use crate::icons::Icon;
use crate::input::Placement;
use crate::inspect::{self, Act, Empty, Inspect, Pointing};
use crate::overlay;
use crate::platform::{Frame, Player};
use crate::reading;
use crate::recordings::{self, Actor, Recording, RecordingId, Seek, TraceCall, TraceLine};
use crate::screen::Picture;
use crate::theme::{MONO, Theme};
use crate::timeline::{self, Moment};
use crate::transport::{Follow, Transport};
use crate::ui::{Button, Look};
use crate::{Daemon, LocalClock};

/// Narrower than this, the trace goes under the video.
const NARROW: f32 = 860.0;
/// The trace column is at least this wide.
const TRACE_MIN: f32 = 280.0;
/// A queue shorter than this isn't worth a line.
const QUEUED_SHOWN_MS: u64 = 100;

use super::recordings::notice;

/// A video open in the player, and the task that redraws as it plays.
struct Playing {
    player: Player,
    _wake: Task<()>,
}

/// One recording, replayed.
pub(crate) struct Replay {
    daemon: Arc<dyn Daemon>,
    clock: Arc<dyn LocalClock>,
    recording: Recording,
    /// Which of its videos is showing.
    current: usize,
    playing: Option<Playing>,
    /// Why the current video can't be played.
    unplayable: Option<String>,
    _locate: Option<Task<()>>,
    /// The current machine's calls, on a live machine's timeline.
    moments: Vec<Moment>,
    /// The screen's box and the strip's, as last laid out.
    screen: Rc<Cell<Option<Bounds<Pixels>>>>,
    strip: Rc<Cell<Option<Bounds<Pixels>>>>,
    scrub: Rc<Cell<Option<Bounds<Pixels>>>>,
    trace: ScrollHandle,
    /// The first highlighted line the trace last scrolled to.
    followed: Option<usize>,
    /// The elements the agent last saw by the playhead, when shown.
    inspect: Option<Inspect>,
    /// Whether the text the agent last read by the playhead is drawn.
    reading: bool,
    /// Snapshots fetched, by trace line: each is asked for once.
    fetched: HashMap<u32, Fetch>,
    _fetches: Vec<Task<()>>,
    /// The replay's clock: the video follows it, so the trace plays and
    /// seeks to its end though the video stops at the screen's last change.
    transport: Transport,
    /// The wall clock's zero, for the transport's milliseconds.
    /// When the replay opened, on the clock in ms.
    epoch: f64,
    /// Whose screen is shown, when the session had several machines.
    machine: Option<MachineName>,
    /// The session's time is before the shown machine's first video.
    unfilmed: bool,
}

/// A trace line's snapshot, as far as it has been fetched.
#[derive(Debug, Clone)]
enum Fetch {
    Loading,
    Seen(Arc<AxSnapshot>),
    Failed(String),
}

impl Replay {
    /// The recording it replays.
    pub(super) const fn id(&self) -> &RecordingId {
        &self.recording.id
    }

    pub(super) fn new(
        daemon: Arc<dyn Daemon>,
        clock: Arc<dyn LocalClock>,
        recording: Recording,
        cx: &mut Context<Self>,
    ) -> Self {
        let epoch = clock.millis();
        let mut replay = Self {
            daemon,
            clock,
            recording,
            current: 0,
            playing: None,
            unplayable: None,
            _locate: None,
            moments: Vec::new(),
            screen: Rc::default(),
            strip: Rc::default(),
            scrub: Rc::default(),
            trace: ScrollHandle::new(),
            followed: None,
            inspect: None,
            reading: false,
            fetched: HashMap::new(),
            _fetches: Vec::new(),
            transport: Transport::start(1000.0),
            epoch,
            machine: None,
            unfilmed: false,
        };
        replay.machine = replay
            .recording
            .videos
            .first()
            .map(|video| video.machine.clone());
        replay.transport = Transport::start(replay.traced_length());
        if !replay.recording.videos.is_empty() {
            replay.load(0, 0.0, false, cx);
        }
        replay
    }

    /// Opens video `index` at `seconds`, playing when `play` says so.
    fn load(&mut self, index: usize, seconds: f64, play: bool, cx: &mut Context<Self>) {
        let Some(video) = self.recording.videos.get(index) else {
            return;
        };
        self.current = index;
        self.playing = None;
        self.unplayable = None;
        self.moments = timeline::from_trace(&self.recording, &video.machine);
        let located = self.daemon.video(&self.recording.id, &video.file);
        self._locate = Some(cx.spawn(async move |replay, cx| {
            let located = located.await;
            let _ = replay.update(cx, |replay, cx| {
                match located {
                    Ok(path) => {
                        let (wake, mut woken) = mpsc::channel(1);
                        let player = Player::open(path, seconds, play, replay.clock.clone(), wake);
                        let redraw = cx.spawn(async move |replay, cx| {
                            while woken.next().await.is_some() {
                                if replay.update(cx, |replay, cx| replay.woken(cx)).is_err() {
                                    return;
                                }
                            }
                        });
                        replay.playing = Some(Playing {
                            player,
                            _wake: redraw,
                        });
                    }
                    Err(error) => replay.unplayable = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The player has something new: a frame, or the end.
    /// The player has something new: a frame, or the end. Past a video's
    /// end the clock plays on over its last frame.
    fn woken(&mut self, cx: &mut Context<Self>) {
        if let Some(playing) = &self.playing {
            let _ = playing.player.take_ended();
            playing.player.release_stale(cx);
        }
        cx.notify();
    }

    fn is_playing(&self) -> bool {
        self.transport.playing()
    }

    /// The wall clock, in the transport's milliseconds.
    fn wall(&self) -> f64 {
        self.clock.millis() - self.epoch
    }

    /// The session's length as traced, at least a second.
    fn traced_length(&self) -> f64 {
        (self.recording.traced_ms() as f64).max(1000.0)
    }

    /// The video's start in the session, in ms: 0 for one recorded before
    /// doppel kept it, which the trace then can't follow.
    fn start_of(&self, index: usize) -> f64 {
        self.recording
            .videos
            .get(index)
            .and_then(|video| video.start_ms)
            .map_or(0.0, |start| start as f64)
    }

    /// Puts the video where the clock is: playing along inside it, held on
    /// its last frame past its end; another of the machine's videos when
    /// the clock moved into it.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let wall = self.wall();
        if let Some(duration) = self.playing.as_ref().and_then(|p| p.player.duration()) {
            let end = self.start_of(self.current) + duration * 1000.0;
            self.transport = self.transport.lengthen(end, wall);
        }
        self.transport = self.transport.settle(wall);
        let at = self.transport.at(wall);
        let seek = if self.recording.aligned() {
            self.recording.seek_at(self.machine.as_ref(), at)
        } else {
            Some(Seek {
                video: self.current,
                seconds: at / 1000.0,
            })
        };
        self.unfilmed = seek.is_none();
        let Some(seek) = seek else {
            if let Some(playing) = &self.playing
                && playing.player.playing()
            {
                playing.player.pause();
            }
            return;
        };
        if seek.video != self.current {
            self.load(seek.video, seek.seconds, self.transport.playing(), cx);
            return;
        }
        let Some(playing) = &self.playing else {
            return;
        };
        let player = &playing.player;
        match (
            Follow::of(seek.seconds, player.duration()),
            self.transport.playing(),
        ) {
            (Follow::Inside(seconds), true) => {
                if !player.playing() {
                    player.seek(seconds);
                    player.play();
                } else if (player.position() - seconds).abs() > 0.3 {
                    player.seek(seconds);
                }
            }
            (Follow::Inside(seconds) | Follow::After(seconds), false)
            | (Follow::After(seconds), true) => {
                if player.playing() {
                    player.pause();
                }
                if (player.position() - seconds).abs() > 0.04 {
                    player.seek(seconds);
                }
            }
        }
    }

    /// Shows video `index` at `seconds`: seeks it if it's showing.
    fn show(&mut self, index: usize, seconds: f64, play: bool, cx: &mut Context<Self>) {
        match &self.playing {
            Some(playing) if index == self.current => {
                playing.player.seek(seconds);
                cx.notify();
            }
            Some(_) | None => self.load(index, seconds, play, cx),
        }
    }

    /// Moves the clock to the session's time `ms`.
    fn seek_time(&mut self, ms: f64, cx: &mut Context<Self>) {
        self.transport = self.transport.seek(ms, self.wall());
        self.sync(cx);
        cx.notify();
    }

    fn choose(&mut self, call: &TraceCall, cx: &mut Context<Self>) {
        if let Some(machine) = &call.machine
            && self
                .recording
                .videos
                .iter()
                .any(|video| &video.machine == machine)
        {
            self.machine = Some(machine.clone());
        }
        self.seek_time(call.start_ms as f64, cx);
    }

    fn toggle(&mut self, cx: &mut Context<Self>) {
        let wall = self.wall();
        self.transport = if self.transport.playing() {
            self.transport.pause(wall)
        } else {
            self.transport.play(wall)
        };
        self.sync(cx);
        cx.notify();
    }

    /// The session's time at the playhead, when the videos line up with it.
    fn now(&self) -> Option<f64> {
        self.recording
            .aligned()
            .then(|| self.transport.at(self.wall()))
    }

    /// The snapshot taken by the playhead, and whether the machine was
    /// acted on since; asks for it the first time it's needed.
    fn seen(
        &mut self,
        now: Option<f64>,
        cx: &mut Context<Self>,
    ) -> (Option<timeline::SeenAt>, Result<Arc<AxSnapshot>, Empty>) {
        let found = now.and_then(|now| timeline::seen_at(&self.moments, now));
        let line = match found.as_ref().map(|found| &found.seen) {
            Some(timeline::Seen::Recorded(line)) => *line,
            Some(timeline::Seen::Live(snapshot)) => return (found.clone(), Ok(snapshot.clone())),
            None => return (found, Err(Empty::None)),
        };
        let shown = match self.fetched.get(&line) {
            Some(Fetch::Seen(snapshot)) => Ok(snapshot.clone()),
            Some(Fetch::Failed(why)) => Err(Empty::Failed(why.clone())),
            Some(Fetch::Loading) => Err(Empty::Loading),
            None => {
                self.fetched.insert(line, Fetch::Loading);
                let fetched = self.daemon.seen_snapshot(&self.recording.id, line);
                self._fetches.push(cx.spawn(async move |replay, cx| {
                    let fetched = fetched.await;
                    let _ = replay.update(cx, |replay, cx| {
                        replay.fetched.insert(
                            line,
                            match fetched {
                                Ok(snapshot) => Fetch::Seen(Arc::new(snapshot)),
                                Err(error) => Fetch::Failed(error.to_string()),
                            },
                        );
                        cx.notify();
                    });
                }));
                Err(Empty::Loading)
            }
        };
        (found, shown)
    }

    /// The moments with each snapshot fetched so far in place, so the film
    /// shows what the agent took in as soon as it's here.
    fn with_seen(&self) -> Vec<Moment> {
        let mut moments = self.moments.clone();
        for moment in &mut moments {
            if let Some(timeline::Seen::Recorded(line)) = moment.seen
                && let Some(Fetch::Seen(snapshot)) = self.fetched.get(&line)
            {
                moment.seen = Some(timeline::Seen::Live(snapshot.clone()));
            }
        }
        moments
    }

    /// Takes in what a person did in the elements panel.
    fn inspected(&mut self, pointing: Pointing, cx: &mut Context<Self>) {
        let now = self.now();
        let (_, shown) = self.seen(now, cx);
        let Some(inspect) = self.inspect.as_mut() else {
            return;
        };
        let before = (inspect.pointed, inspect.picked);
        let changes = !matches!(pointing, Pointing::Hover(_) | Pointing::Leave(_));
        if !inspect.apply(pointing, shown.as_deref().ok()) {
            self.inspect = None;
        } else if !changes && before == (inspect.pointed, inspect.picked) {
            return;
        }
        cx.notify();
    }

    fn screen(
        &mut self,
        now: Option<f64>,
        seen: (Option<&timeline::SeenAt>, Option<&Arc<AxSnapshot>>),
        act: &Act,
        theme: &Theme,
    ) -> Div {
        let shown = self
            .playing
            .as_ref()
            .and_then(|playing| playing.player.frame());
        let problem = self.unplayable.clone().or_else(|| {
            self.playing
                .as_ref()
                .and_then(|playing| playing.player.failed())
        });
        let bounds = self.screen.clone();
        let mut frame = div()
            .relative()
            .w_full()
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .bg(theme.screen)
            .map(|mut frame| {
                frame.style().aspect_ratio = Some(16.0 / 10.0);
                frame
            })
            .child(
                canvas(
                    move |laid_out, _, _| bounds.set(Some(laid_out)),
                    |_, (), _, _| {},
                )
                .absolute()
                .size_full(),
            );
        let video = self.recording.videos.get(self.current);
        let picture = shown
            .as_ref()
            .zip(self.screen.get())
            .and_then(|(buffer, bounds)| {
                let points = video?.points?;
                let (width, height) = buffer.size();
                let placed = Placement::contain(
                    (
                        0.0,
                        0.0,
                        f64::from(bounds.size.width),
                        f64::from(bounds.size.height),
                    ),
                    (width as f64, height as f64),
                )?;
                let (left, top, width, height) = placed.rect();
                Some(Picture {
                    left: left as f32,
                    top: top as f32,
                    width: width as f32,
                    height: height as f32,
                    points,
                })
            });
        if let Some(drawn) = shown.as_ref().and_then(Frame::draw) {
            frame = frame.child(drawn);
        }
        if let (Some(inspect), Some(picture), (Some(found), Some(snapshot))) =
            (self.inspect.as_ref(), picture, seen)
        {
            let paused = !self.is_playing();
            frame = frame.child(inspect::boxes(
                snapshot,
                &Named::of(snapshot, found.refs.as_deref()),
                inspect,
                picture,
                found.acted_since,
                paused,
                act,
            ));
        }
        if let (true, Some(picture), Some(now)) = (self.reading, picture, now) {
            let read = timeline::read_at(&self.moments, now);
            let (drawn, whose, stale) = match &read {
                Some(read) => (
                    reading::Drawn::Read(&read.read),
                    SharedString::from(format!(
                        "agent · {:.1} s before",
                        ((now - read.since_ms) / 1000.0).max(0.0)
                    )),
                    read.acted_since,
                ),
                None => (reading::Drawn::Nothing, SharedString::default(), false),
            };
            let nothing: reading::ReadNow = Rc::new(|_, _| {});
            frame = frame.child(reading::layer(
                "replay-text",
                drawn,
                picture,
                whose,
                stale,
                None,
                theme,
                &nothing,
            ));
        }
        if let (Some(picture), Some(now)) = (picture, now)
            && !self.moments.is_empty()
        {
            let moments = self.with_seen();
            frame = frame.child(overlay::layer(&moments, picture, now));
        }
        if let Some(problem) = problem {
            frame = frame.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p(px(16.))
                    .child(
                        div()
                            .px(px(12.))
                            .py(px(5.))
                            .text_size(px(12.))
                            .text_color(theme.muted)
                            .bg(theme.overlay.opacity(0.85))
                            .child(problem),
                    ),
            );
        }
        frame
    }

    /// Play or pause, where the playhead is, and a bar to seek along.
    fn controls(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let wall = self.wall();
        let length = self.transport.length();
        let position = self.transport.at(wall);
        let playing = self.is_playing();
        let fraction = (position / length.max(1.0)).clamp(0.0, 1.0) as f32;
        let scrub = self.scrub.clone();
        let bar = scrub.clone();
        let names = {
            let mut names: Vec<MachineName> = self
                .recording
                .videos
                .iter()
                .map(|video| video.machine.clone())
                .collect();
            names.dedup();
            names
        };
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .mt(px(8.))
            .child(
                div()
                    .id("replay-play")
                    .flex_none()
                    .size(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme.slab)
                    .cursor_pointer()
                    .child(if playing { Icon::Pause } else { Icon::Play }.svg(13., theme.on_slab))
                    .on_click(cx.listener(|replay, _, _, cx| replay.toggle(cx))),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .font_family(MONO)
                    .text_size(px(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.strong)
                    .child(recordings::clock(position))
                    .child(
                        div()
                            .text_color(theme.faint)
                            .child(format!(" / {}", recordings::clock(length))),
                    ),
            )
            .child(
                div()
                    .id("replay-scrub")
                    .flex_1()
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .h(px(4.))
                            .bg(theme.pressed)
                            .child(
                                canvas(
                                    move |laid_out, _, _| bar.set(Some(laid_out)),
                                    |_, (), _, _| {},
                                )
                                .absolute()
                                .size_full(),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .bottom_0()
                                    .left_0()
                                    .w(gpui::relative(fraction))
                                    .bg(theme.live),
                            ),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |replay, event: &MouseDownEvent, _, cx| {
                            let Some(bounds) = scrub.get() else {
                                return;
                            };
                            let Some(at) = along(bounds, event) else {
                                return;
                            };
                            let length = replay.transport.length();
                            replay.seek_time(at * length, cx);
                        }),
                    ),
            )
            .when(names.len() > 1, |row| {
                row.children(names.into_iter().map(|name| {
                    let on = self.machine.as_ref() == Some(&name);
                    let picked = name.clone();
                    div()
                        .id(SharedString::from(format!("replay-machine-{name}")))
                        .flex_none()
                        .px(px(10.))
                        .h(px(26.))
                        .flex()
                        .items_center()
                        .font_family(MONO)
                        .text_size(px(11.))
                        .text_color(if on { theme.on_slab } else { theme.muted })
                        .when(on, |chip| {
                            chip.bg(theme.slab).border_b_2().border_color(theme.live)
                        })
                        .cursor_pointer()
                        .child(name.to_string())
                        .on_click(cx.listener(move |replay, _, _, cx| {
                            replay.machine = Some(picked.clone());
                            replay.sync(cx);
                            cx.notify();
                        }))
                }))
            })
    }

    /// The strip of the showing machine's calls, over the whole session.
    fn strip(&self, now: Option<f64>, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let length = self.recording.traced_ms() as f64;
        let bounds = self.strip.clone();
        let laid = bounds.clone();
        div()
            .id("replay-strip")
            .relative()
            .mt(px(8.))
            .cursor_pointer()
            .child(
                canvas(
                    move |laid_out, _, _| laid.set(Some(laid_out)),
                    |_, (), _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(overlay::strip(
                "replay-timeline",
                &self.moments,
                overlay::Span {
                    from: 0.0,
                    to: length,
                    now: now.unwrap_or(0.0),
                    at: now,
                },
                recordings::clock,
                theme,
            ))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |replay, event: &MouseDownEvent, _, cx| {
                    if let Some(at) = bounds.get().and_then(|bounds| along(bounds, event)) {
                        replay.seek_time(at * length, cx);
                    }
                }),
            )
    }

    fn chips(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        div().flex().flex_wrap().gap(px(6.)).mt(px(8.)).children(
            self.recording
                .videos
                .iter()
                .enumerate()
                .map(|(index, video)| {
                    let on = index == self.current;
                    div()
                        .id(ElementId::NamedInteger("replay-video".into(), index as u64))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .h(px(30.))
                        .px(px(11.))
                        .border_1()
                        .border_color(if on {
                            theme.accent
                        } else {
                            gpui::transparent_black()
                        })
                        .text_size(px(13.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.muted)
                        .cursor_pointer()
                        .hover(|chip| chip.bg(theme.hover).text_color(theme.text))
                        .child(video.file.stem().to_owned())
                        .children(video.start_ms.map(|start| {
                            div()
                                .text_color(theme.muted)
                                .child(format!("at {}", recordings::clock(start as f64)))
                        }))
                        .on_click(
                            cx.listener(move |replay, _, _, cx| replay.show(index, 0.0, false, cx)),
                        )
                }),
        )
    }

    fn line(
        &self,
        index: usize,
        line: &TraceLine,
        active: &[usize],
        lined: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let on = active.contains(&index);
        let person = matches!(line, TraceLine::Call(call) if call.by == Actor::Person);
        let row = div()
            .relative()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(theme.line)
            .text_size(px(13.))
            .map(|row| {
                if on {
                    row.bg(theme.accent_fill.opacity(0.16)).child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left_0()
                            .w(px(3.))
                            .bg(theme.accent),
                    )
                } else if person {
                    row.bg(theme.accent_fill.opacity(0.08))
                } else {
                    row
                }
            });
        let detail = |text: String, muted: bool| {
            div()
                .px(px(10.))
                .pb(px(8.))
                .text_color(if muted { theme.muted } else { theme.text })
                .child(text)
        };
        let call = match line {
            TraceLine::Call(call) => call,
            TraceLine::Unreadable(raw) => {
                return row
                    .child(detail(
                        format!("A line this window can't read: {raw}"),
                        true,
                    ))
                    .into_any_element();
            }
        };
        let machines = self.recording.machine_count();
        let chosen = call.clone();
        let mut text = div().flex_1().min_w_0().child(call.call.clone());
        if machines > 1
            && let Some(machine) = &call.machine
        {
            text = text.child(div().text_color(theme.muted).child(format!(" · {machine}")));
        }
        let button = div()
            .id(ElementId::NamedInteger("trace-line".into(), index as u64))
            .flex()
            .items_start()
            .gap(px(8.))
            .px(px(10.))
            .py(px(8.))
            .text_color(theme.text)
            .when(lined, |button| {
                button
                    .cursor_pointer()
                    .hover(|button| button.bg(theme.hover))
            })
            .child(
                div()
                    .flex_none()
                    .text_color(theme.muted)
                    .child(recordings::clock(call.start_ms as f64)),
            )
            .child(text.flex().flex_wrap())
            .children(call.outcome.words().map(|words| {
                div()
                    .flex_none()
                    .px(px(6.))
                    .bg(theme.warning_soft)
                    .text_size(px(12.))
                    .child(words)
            }))
            .child(
                div()
                    .flex_none()
                    .text_color(theme.muted)
                    .child(recordings::seconds(
                        call.end_ms.saturating_sub(call.start_ms),
                    )),
            )
            .when(lined, |button| {
                button.on_click(cx.listener(move |replay, _, _, cx| replay.choose(&chosen, cx)))
            });
        row.child(button)
            .children(call.error.clone().map(|error| detail(error, false)))
            .children(call.waited_ms.map(|waited| {
                detail(
                    format!(
                        "waited {} for the machine first",
                        recordings::seconds(waited)
                    ),
                    true,
                )
            }))
            .children(
                call.queued_ms
                    .filter(|queued| *queued >= QUEUED_SHOWN_MS)
                    .map(|queued| {
                        detail(
                            format!("queued {} behind other calls", recordings::seconds(queued)),
                            true,
                        )
                    }),
            )
            .into_any_element()
    }
}

/// How far along `bounds` a press is, from 0 to 1.
fn along(bounds: Bounds<Pixels>, event: &MouseDownEvent) -> Option<f64> {
    let width = f64::from(bounds.size.width);
    (width > 0.0).then(|| (f64::from(event.position.x - bounds.origin.x) / width).clamp(0.0, 1.0))
}

impl Render for Replay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let narrow = f32::from(window.viewport_size().width) < NARROW;
        self.sync(cx);
        let now = self.now();
        if self.is_playing() {
            window.request_animation_frame();
        }
        let lined = self.recording.aligned();
        let active = match (self.recording.videos.get(self.current), now) {
            (Some(video), Some(now)) => self.recording.calls_at(&video.machine, now),
            (Some(_) | None, _) => Vec::new(),
        };
        if active.first() != self.followed.as_ref()
            && let Some(first) = active.first()
        {
            self.trace.scroll_to_item(*first);
        }
        self.followed = active.first().copied();

        let act: Act = {
            let listener = cx.listener(|replay, pointing: &Pointing, _, cx| {
                replay.inspected(pointing.clone(), cx);
            });
            Rc::new(move |pointing, window, cx| listener(&pointing, window, cx))
        };
        // Fetched whether or not the elements show: the film over the
        // screen shows what the agent took in.
        let seen = self.seen(now, cx);
        let (found, shown) = if self.inspect.is_some() {
            seen
        } else {
            (None, Err(Empty::None))
        };
        if let Some(inspect) = self.inspect.as_mut() {
            inspect.follow(shown.as_deref().ok());
        }

        let mut stage = div().flex().flex_col().min_w_0();
        if self.recording.videos.is_empty() {
            stage =
                stage.child(div().text_color(theme.muted).child(
                    "No video: the session's machines weren't running while it was recorded.",
                ));
        } else {
            stage = stage
                .child(self.screen(now, (found.as_ref(), shown.as_ref().ok()), &act, &theme))
                .child(self.controls(&theme, cx));
            if lined && !self.moments.is_empty() {
                stage = stage.child(self.strip(now, &theme, cx));
                let open = self.inspect.is_some();
                stage = stage.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .mt(px(8.))
                        .child(
                            Button::new(
                                "replay-elements",
                                &theme,
                                if open { Look::On } else { Look::Ghost },
                                Some(Icon::ListTree),
                                Some("Elements the agent saw".into()),
                            )
                            .on_press(cx.listener(
                                |replay, _, _, cx| {
                                    replay.inspect = match replay.inspect.take() {
                                        Some(_) => None,
                                        None => Some(Inspect::new()),
                                    };
                                    cx.notify();
                                },
                            )),
                        )
                        .child(
                            Button::new(
                                "replay-text",
                                &theme,
                                if self.reading { Look::On } else { Look::Ghost },
                                Some(Icon::ScanText),
                                Some("Text the agent read".into()),
                            )
                            .on_press(cx.listener(
                                |replay, _, _, cx| {
                                    replay.reading = !replay.reading;
                                    cx.notify();
                                },
                            )),
                        )
                        .when(open && self.is_playing(), |row| {
                            row.child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(theme.muted)
                                    .child("Pause to point at elements on the video."),
                            )
                        }),
                );
                if let Some(inspect) = &self.inspect {
                    let age = match (found.as_ref(), now) {
                        (Some(found), Some(now)) => {
                            format!("{:.1} s before", ((now - found.since_ms) / 1000.0).max(0.0))
                        }
                        (Some(_) | None, _) => String::new(),
                    };
                    let named = match (&shown, found.as_ref()) {
                        (Ok(snapshot), Some(found)) => Named::of(snapshot, found.refs.as_deref()),
                        (Ok(_) | Err(_), _) => Named::default(),
                    };
                    stage = stage.child(div().flex().h(px(320.)).mt(px(8.)).child(inspect::tree(
                        "replay-tree",
                        shown.as_ref().map_err(Clone::clone),
                        &named,
                        inspect,
                        age.into(),
                        found.as_ref().is_some_and(|found| found.acted_since),
                        None,
                        None,
                        &theme,
                        &act,
                    )));
                }
            }
            if self.recording.videos.len() > 1 {
                stage = stage.child(self.chips(&theme, cx));
            }
        }
        if !lined && !self.recording.videos.is_empty() {
            stage = stage.child(notice(&theme).mt(px(8.)).text_size(px(13.)).child(
                "Recorded before doppel kept when each video starts, so the trace can't follow the video.",
            ));
        }

        let lines: Vec<AnyElement> = if self.recording.trace.is_empty() {
            vec![
                div()
                    .px(px(10.))
                    .py(px(8.))
                    .text_color(theme.muted)
                    .child("Nothing traced.")
                    .into_any_element(),
            ]
        } else {
            let trace = self.recording.trace.clone();
            trace
                .iter()
                .enumerate()
                .map(|(index, line)| self.line(index, line, &active, lined, &theme, cx))
                .collect()
        };
        let height = f32::from(window.viewport_size().height);
        let trace = div()
            .id("replay-trace")
            .track_scroll(&self.trace)
            .overflow_y_scroll()
            .max_h(px((height * 0.7).min(720.)))
            .border_1()
            .border_color(theme.line)
            .children(lines);

        if narrow {
            div()
                .flex()
                .flex_col()
                .gap(px(16.))
                .child(stage)
                .child(trace)
        } else {
            div()
                .flex()
                .items_start()
                .gap(px(16.))
                // Two parts video to one part trace, as the browser's grid.
                .child(
                    stage
                        .flex_shrink(1.)
                        .flex_basis(gpui::relative(0.))
                        .map(|mut stage| {
                            stage.style().flex_grow = Some(2.0);
                            stage
                        }),
                )
                .child(
                    trace
                        .flex_grow(1.)
                        .flex_shrink(1.)
                        .flex_basis(gpui::relative(0.))
                        .min_w(px(TRACE_MIN)),
                )
        }
    }
}
