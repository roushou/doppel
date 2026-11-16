//! One machine's screen in the window: its picture, and the person's input
//! once they have control. Its source is read on a thread of its own, which
//! converts frames to pixel buffers and leaves only the latest for the UI;
//! its input goes out on another. Dropping the view ends both: the reader
//! stops at its next frame, and the input thread once its queue is empty,
//! which closes the screen's connection and hands control back.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};

use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use gpui::{
    App, Bounds, Context, EventEmitter, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit, ParentElement, Pixels,
    Render, ScrollDelta, ScrollWheelEvent, Styled, Task, Window, canvas, div, px, surface,
};

use crate::decode::Decoder;
use crate::input::Placement;
use crate::pixels::{Buffer, Pixels as PixelPool};
use crate::resync::{Resync, Step};
use crate::steering::{SteerEvent, SteerRequest, Steering};
use crate::theme::Theme;
use crate::{
    Button, EncodedSource, FrameSource, Frames, GuestInput, InputSink, Key, Modifier, Next,
    NextEncoded, PointerPhase, Screen, Takeover,
};
use doppel_machine::geometry::Size;

/// What a reader leaves for its view.
#[derive(Debug)]
enum Update {
    Frame(Buffer),
    Ended,
    Failed(String),
}

/// The latest update from a reader, and a nudge to look at it.
#[derive(Debug, Clone)]
struct Mailbox {
    latest: Arc<Mutex<Option<Update>>>,
    wake: mpsc::Sender<()>,
}

impl Mailbox {
    fn put(&mut self, update: Update) {
        *self.latest.lock().unwrap_or_else(PoisonError::into_inner) = Some(update);
        // Full: the view hasn't looked yet, and will see this update.
        let _ = self.wake.try_send(());
    }

    /// The view is gone: no one will look again.
    fn abandoned(&self) -> bool {
        self.wake.is_closed()
    }
}

/// Reads one source until it ends, fails, or its view goes away.
struct Reader {
    source: Frames,
    mailbox: Mailbox,
}

impl Reader {
    fn spawn(self) {
        std::thread::Builder::new()
            .name("doppel-viewer-reader".into())
            .spawn(move || self.run())
            .map(drop)
            .unwrap_or_else(|_| {
                // No thread for this screen: its view keeps waiting.
            });
    }

    fn run(self) {
        match self.source {
            Frames::Raw(source) => Self::read_raw(source, self.mailbox),
            Frames::Encoded(source) => Self::read_encoded(source, self.mailbox),
        }
    }

    fn read_raw(mut source: Box<dyn FrameSource>, mut mailbox: Mailbox) {
        // Made here, on the reader's thread: the pool stays on it.
        let mut pool: Option<PixelPool> = None;
        while !mailbox.abandoned() {
            let update = match source.next_frame() {
                Ok(Next::Frame(frame)) => {
                    let size = (frame.width(), frame.height());
                    let pixels = match pool.take() {
                        Some(pixels) if pixels.size() == size => Ok(pixels),
                        Some(_) | None => PixelPool::new(size.0, size.1),
                    };
                    match pixels.and_then(|pixels| {
                        let buffer = pixels.fill(frame);
                        pool = Some(pixels);
                        buffer
                    }) {
                        Ok(buffer) => Update::Frame(buffer),
                        Err(error) => Update::Failed(error.to_string()),
                    }
                }
                Ok(Next::Unchanged) => continue,
                Ok(Next::Ended) => Update::Ended,
                Err(error) => Update::Failed(error.to_string()),
            };
            let last = !matches!(update, Update::Frame(_));
            mailbox.put(update);
            if last {
                return;
            }
        }
    }

    /// Decodes frames from a key frame on. A frame that fails to decode
    /// isn't shown, and the screen holds its last frame until the next key
    /// frame; one that can't get a decoder ends the screen with why.
    fn read_encoded(mut source: Box<dyn EncodedSource>, mut mailbox: Mailbox) {
        // Made here, on the reader's thread: the decoder stays on it.
        let mut decoder: Option<Decoder> = None;
        let mut resync = Resync::START;
        while !mailbox.abandoned() {
            let frame = match source.next_frame() {
                Ok(NextEncoded::Frame(frame)) => frame,
                Ok(NextEncoded::Unchanged) => continue,
                Ok(NextEncoded::Ended) => return mailbox.put(Update::Ended),
                Err(error) => return mailbox.put(Update::Failed(error.to_string())),
            };
            let (next, step) = resync.next(&frame);
            resync = next;
            match step {
                Step::Restart(parameter_sets) => {
                    if !decoder
                        .as_ref()
                        .is_some_and(|decoder| decoder.decodes(parameter_sets))
                    {
                        // The old session goes before the new one is made.
                        drop(decoder.take());
                        match Decoder::new(parameter_sets) {
                            Ok(made) => decoder = Some(made),
                            Err(error) => return mailbox.put(Update::Failed(error.to_string())),
                        }
                    }
                }
                Step::Decode => {}
                Step::Skip => continue,
            }
            let Some(decoder) = decoder.as_mut() else {
                resync = resync.failed();
                continue;
            };
            match decoder.decode(&frame.data) {
                Ok(Some(buffer)) => mailbox.put(Update::Frame(buffer)),
                Ok(None) => {}
                Err(_) => resync = resync.failed(),
            }
        }
    }
}

#[derive(Debug)]
enum ViewState {
    Waiting,
    Showing(Buffer),
    Ended(Option<Buffer>),
    Failed(String),
}

/// What a view hands its input thread.
#[derive(Debug)]
enum Pumped {
    Input(GuestInput),
    Steer(SteerRequest),
}

/// Sends one screen's input, and its requests for control, from a thread of
/// its own and in order, so a slow guest never stalls the window. Moves that
/// queue up behind a slow send collapse to the last one.
struct InputPump {
    sink: Option<Box<dyn InputSink>>,
    takeover: Option<Box<dyn Takeover>>,
    inputs: std::sync::mpsc::Receiver<Pumped>,
    /// The last failure, shown over the screen until an input gets through.
    failure: Arc<Mutex<Option<String>>>,
    /// The answer to the last request for control, for the view.
    answer: Arc<Mutex<Option<SteerEvent>>>,
    wake: mpsc::Sender<()>,
}

impl InputPump {
    fn spawn(self) {
        // No thread: the screen stays view-only (its sender's sends fail).
        let _ = std::thread::Builder::new()
            .name("doppel-viewer-input".into())
            .spawn(move || self.run());
    }

    fn run(mut self) {
        while let Ok(first) = self.inputs.recv() {
            let batch: Vec<_> = std::iter::once(first)
                .chain(self.inputs.try_iter())
                .collect();
            for (index, pumped) in batch.iter().enumerate() {
                match pumped {
                    Pumped::Input(input) => {
                        let next = match batch.get(index + 1) {
                            Some(Pumped::Input(next)) => Some(next),
                            Some(Pumped::Steer(_)) | None => None,
                        };
                        if Self::superseded(input, next) {
                            continue;
                        }
                        let Some(sink) = self.sink.as_mut() else {
                            continue;
                        };
                        let outcome = sink.send(input.clone());
                        *self.failure.lock().unwrap_or_else(PoisonError::into_inner) =
                            outcome.err().map(|error| format!("input: {error}"));
                    }
                    Pumped::Steer(request) => self.steer(*request),
                }
            }
        }
    }

    fn steer(&mut self, request: SteerRequest) {
        let outcome = match (self.takeover.as_mut(), request) {
            (Some(takeover), SteerRequest::Take) => takeover.take(),
            (Some(takeover), SteerRequest::HandBack) => takeover.hand_back(),
            // Views only steer screens that have a takeover.
            (None, SteerRequest::Take | SteerRequest::HandBack) => Ok(()),
        };
        *self.answer.lock().unwrap_or_else(PoisonError::into_inner) = Some(match outcome {
            Ok(()) => SteerEvent::Answered,
            Err(error) => SteerEvent::Refused(error.to_string()),
        });
        // Full: the view hasn't looked yet, and will see this answer.
        let _ = self.wake.try_send(());
    }

    /// A move followed by another move of the same kind: only the last counts.
    fn superseded(input: &GuestInput, next: Option<&GuestInput>) -> bool {
        match (input, next) {
            (
                GuestInput::Pointer {
                    phase: PointerPhase::Move { held },
                    ..
                },
                Some(GuestInput::Pointer {
                    phase: PointerPhase::Move { held: next_held },
                    ..
                }),
            ) => held == next_held,
            (
                GuestInput::Pointer { .. } | GuestInput::Scroll { .. } | GuestInput::Key { .. },
                _,
            ) => false,
        }
    }
}

/// Where the picture is drawn in the screen's box, and the display's size
/// in points: what the overlay needs to draw over it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Picture {
    /// The picture's rectangle, from the box's top left, in window points.
    pub(crate) left: f32,
    pub(crate) top: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
    /// The display's size in its points.
    pub(crate) points: Size,
}

impl Picture {
    /// A display point, from the box's top left, in window points.
    pub(crate) fn place(&self, x: f64, y: f64) -> (f32, f32) {
        let (width, height) = (self.points.width(), self.points.height());
        (
            self.left + (x / width) as f32 * self.width,
            self.top + (y / height) as f32 * self.height,
        )
    }
}

/// What a screen tells the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScreenEvent {
    /// Who steers it changed.
    Steering,
    /// A request for control was refused, with why.
    Refused(String),
    /// Its source ended or failed: the machine stopped, or it can't be read.
    Ended,
}

/// One machine's screen.
pub(crate) struct ScreenView {
    state: ViewState,
    focus: FocusHandle,
    /// The display's size in points, when known.
    points: Option<Size>,
    /// `None` for a view-only screen.
    input: Option<std::sync::mpsc::Sender<Pumped>>,
    input_failure: Arc<Mutex<Option<String>>>,
    /// `None` when the screen's input needs no control.
    steering: Option<Steering>,
    /// Waiting for the request in flight to be answered.
    waiters: Vec<oneshot::Sender<()>>,
    /// The picture's box as last laid out.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The button pressed on this screen and not yet released.
    pressed: Option<Button>,
    _listen: Task<()>,
}

impl std::fmt::Debug for ScreenView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScreenView")
            .field("state", &self.state)
            .field("steering", &self.steering)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<ScreenEvent> for ScreenView {}

impl ScreenView {
    pub(crate) fn new(screen: Screen, cx: &mut Context<Self>) -> Self {
        let (wake, mut woken) = mpsc::channel(1);
        let latest = Arc::new(Mutex::new(None));
        Reader {
            source: screen.source,
            mailbox: Mailbox {
                latest: latest.clone(),
                wake: wake.clone(),
            },
        }
        .spawn();
        let input_failure = Arc::new(Mutex::new(None));
        let answers = Arc::new(Mutex::new(None));
        let steering = screen.takeover.is_some().then_some(Steering::START);
        let input = (screen.input.is_some() || screen.takeover.is_some()).then(|| {
            let (sender, inputs) = std::sync::mpsc::channel();
            InputPump {
                sink: screen.input,
                takeover: screen.takeover,
                inputs,
                failure: input_failure.clone(),
                answer: answers.clone(),
                wake,
            }
            .spawn();
            sender
        });
        let listen = cx.spawn(async move |this, cx| {
            while woken.next().await.is_some() {
                let update = latest.lock().unwrap_or_else(PoisonError::into_inner).take();
                let answer = answers
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                if update.is_none() && answer.is_none() {
                    continue;
                }
                let applied = this.update(cx, |view, cx| {
                    if let Some(update) = update {
                        view.apply(update, cx);
                    }
                    if let Some(answer) = answer {
                        if let SteerEvent::Refused(why) = &answer {
                            cx.emit(ScreenEvent::Refused(why.clone()));
                        }
                        view.steer(answer, cx);
                    }
                    cx.notify();
                });
                if applied.is_err() {
                    return;
                }
            }
        });
        Self {
            state: ViewState::Waiting,
            focus: cx.focus_handle(),
            points: screen.points,
            input,
            input_failure,
            steering,
            waiters: Vec::new(),
            bounds: Rc::new(Cell::new(None)),
            pressed: None,
            _listen: listen,
        }
    }

    fn apply(&mut self, update: Update, cx: &mut Context<Self>) {
        self.state = match (
            update,
            std::mem::replace(&mut self.state, ViewState::Waiting),
        ) {
            (Update::Frame(buffer), _) => ViewState::Showing(buffer),
            (Update::Ended, ViewState::Showing(last)) => {
                self.steer(SteerEvent::Ended, cx);
                cx.emit(ScreenEvent::Ended);
                ViewState::Ended(Some(last))
            }
            (Update::Ended, ViewState::Waiting | ViewState::Ended(_) | ViewState::Failed(_)) => {
                self.steer(SteerEvent::Ended, cx);
                cx.emit(ScreenEvent::Ended);
                ViewState::Ended(None)
            }
            (Update::Failed(reason), _) => {
                self.steer(SteerEvent::Ended, cx);
                cx.emit(ScreenEvent::Ended);
                ViewState::Failed(reason)
            }
        };
    }

    /// Whether its source is done: the machine stopped, or it failed.
    pub(crate) const fn ended(&self) -> bool {
        match self.state {
            ViewState::Ended(_) | ViewState::Failed(_) => true,
            ViewState::Waiting | ViewState::Showing(_) => false,
        }
    }

    /// Who steers it, when taking control is how input is sent.
    pub(crate) const fn steering(&self) -> Option<&Steering> {
        self.steering.as_ref()
    }

    /// Whether the person has control of it.
    pub(crate) fn driving(&self) -> bool {
        self.steering.as_ref().is_some_and(Steering::sends_input)
    }

    /// Takes control, or hands it back: the header's button.
    pub(crate) fn press(&mut self, cx: &mut Context<Self>) {
        self.steer(SteerEvent::Pressed, cx);
        cx.notify();
    }

    /// Hands control back if the person has it; the answer comes when
    /// it's done (or refused).
    pub(crate) fn hand_back(&mut self, cx: &mut Context<Self>) -> Option<oneshot::Receiver<()>> {
        if !self.driving() {
            return None;
        }
        let (done, answered) = oneshot::channel();
        self.waiters.push(done);
        self.press(cx);
        Some(answered)
    }

    /// Why the screen isn't showing, or input isn't getting through.
    fn problem(&self) -> Option<String> {
        let state = match &self.state {
            ViewState::Waiting => Some("connecting…".to_owned()),
            ViewState::Showing(_) => None,
            ViewState::Ended(_) => Some("the screen closed; reconnecting…".to_owned()),
            ViewState::Failed(reason) => Some(reason.clone()),
        };
        let failure = self
            .input_failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        failure.or(state)
    }

    /// Where the picture is, for the overlay: `None` before a frame and a
    /// layout, or when the display's size isn't known.
    pub(crate) fn picture(&self) -> Option<Picture> {
        let ViewState::Showing(buffer) = &self.state else {
            return None;
        };
        let points = self.points?;
        let bounds = self.bounds.get()?;
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
    }

    /// Where the live image is drawn in the window; `None` before a frame
    /// or layout, and once the machine stopped (its last frame no longer
    /// takes input).
    fn placement(&self) -> Option<Placement> {
        let ViewState::Showing(buffer) = &self.state else {
            return None;
        };
        let bounds = self.bounds.get()?;
        let (width, height) = buffer.size();
        Placement::contain(
            (
                f64::from(bounds.origin.x),
                f64::from(bounds.origin.y),
                f64::from(bounds.size.width),
                f64::from(bounds.size.height),
            ),
            (width as f64, height as f64),
        )
    }

    fn send(&self, input: GuestInput) {
        if !self.steering.as_ref().is_none_or(Steering::sends_input) {
            return;
        }
        if let Some(input_sender) = &self.input {
            // The pump is gone only if its thread couldn't start.
            let _ = input_sender.send(Pumped::Input(input));
        }
    }

    /// Moves the steering on, and sends the request it makes, if any.
    fn steer(&mut self, event: SteerEvent, cx: &mut Context<Self>) {
        let Some(steering) = self.steering.take() else {
            return;
        };
        let answered = matches!(
            event,
            SteerEvent::Answered | SteerEvent::Refused(_) | SteerEvent::Ended
        );
        let (next, request) = steering.clone().on(event);
        let changed = steering != next;
        self.steering = Some(next);
        if let (Some(request), Some(input_sender)) = (request, &self.input) {
            let _ = input_sender.send(Pumped::Steer(request));
        }
        if answered {
            for waiter in self.waiters.drain(..) {
                let _ = waiter.send(());
            }
        }
        if changed {
            cx.emit(ScreenEvent::Steering);
        }
    }

    fn press_pointer(&mut self, event: &MouseDownEvent, window: &mut Window) {
        window.focus(&self.focus);
        let (Some(button), Some(placement)) = (Button::from_gpui(event.button), self.placement())
        else {
            return;
        };
        let Some(at) = placement.locate(f64::from(event.position.x), f64::from(event.position.y))
        else {
            return;
        };
        self.pressed = Some(button);
        self.send(GuestInput::Pointer {
            phase: PointerPhase::Press {
                button,
                clicks: u32::try_from(event.click_count).unwrap_or(1),
            },
            at,
            modifiers: Modifier::from_gpui(&event.modifiers),
        });
    }

    /// A release anywhere ends a press made on this screen.
    fn release(&mut self, event: &MouseUpEvent) {
        let button = Button::from_gpui(event.button);
        if button.is_none() || button != self.pressed {
            return;
        }
        self.pressed = None;
        let (Some(button), Some(placement)) = (button, self.placement()) else {
            return;
        };
        self.send(GuestInput::Pointer {
            phase: PointerPhase::Release {
                button,
                clicks: u32::try_from(event.click_count).unwrap_or(1),
            },
            at: placement.clamp(f64::from(event.position.x), f64::from(event.position.y)),
            modifiers: Modifier::from_gpui(&event.modifiers),
        });
    }

    fn moved(&mut self, event: &MouseMoveEvent) {
        let Some(placement) = self.placement() else {
            return;
        };
        let (x, y) = (f64::from(event.position.x), f64::from(event.position.y));
        // A drag follows the pointer off the image; a hover stops at its edge.
        let held = self.pressed;
        let at = match held {
            Some(_) => Some(placement.clamp(x, y)),
            None => placement.locate(x, y),
        };
        let Some(at) = at else { return };
        self.send(GuestInput::Pointer {
            phase: PointerPhase::Move { held },
            at,
            modifiers: Modifier::from_gpui(&event.modifiers),
        });
    }

    fn scrolled(&mut self, event: &ScrollWheelEvent) {
        let Some(placement) = self.placement() else {
            return;
        };
        let Some(at) = placement.locate(f64::from(event.position.x), f64::from(event.position.y))
        else {
            return;
        };
        let (dx, dy) = match event.delta {
            ScrollDelta::Pixels(delta) => (f64::from(delta.x), f64::from(delta.y)),
            ScrollDelta::Lines(lines) => (
                f64::from(lines.x) * Self::LINE_POINTS,
                f64::from(lines.y) * Self::LINE_POINTS,
            ),
        };
        let (dx, dy) = placement.scale(dx, dy);
        self.send(GuestInput::Scroll { at, dx, dy });
    }

    /// Window points a scroll of one line moves.
    const LINE_POINTS: f64 = 20.0;

    fn key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        // Keys go to the machine only while the person drives it; otherwise
        // they're the window's (its shortcuts).
        if self.placement().is_none() || !self.steering.as_ref().is_none_or(Steering::sends_input) {
            return;
        }
        if let Ok(key) = Key::try_from(&event.keystroke) {
            self.send(GuestInput::Key {
                key,
                modifiers: Modifier::from_gpui(&event.keystroke.modifiers),
            });
            cx.stop_propagation();
        }
    }
}

impl Render for ScreenView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let shown = match &self.state {
            ViewState::Showing(buffer) | ViewState::Ended(Some(buffer)) => {
                Some(buffer.pixel_buffer())
            }
            ViewState::Waiting | ViewState::Ended(None) | ViewState::Failed(_) => None,
        };
        let bounds = self.bounds.clone();
        let body = div()
            .relative()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.screen)
            .track_focus(&self.focus)
            .child(
                canvas(
                    move |laid_out, _, _| bounds.set(Some(laid_out)),
                    |_, (), _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .children(
                shown.map(|pixels| surface(pixels).object_fit(ObjectFit::Contain).size_full()),
            )
            .children(self.problem().map(|problem| {
                div()
                    .absolute()
                    .bottom(px(12.))
                    .left_0()
                    .right_0()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .px(px(12.))
                            .py(px(5.))
                            .text_size(px(12.))
                            .text_color(theme.muted)
                            .bg(theme.overlay.opacity(0.85))
                            .whitespace_nowrap()
                            .child(problem),
                    )
            }));
        if self.input.is_none() {
            return body;
        }
        body.on_mouse_down(
            MouseButton::Left,
            cx.listener(|view, event, window, _| view.press_pointer(event, window)),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(|view, event, window, _| view.press_pointer(event, window)),
        )
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|view, event, _, _| view.release(event)),
        )
        .on_mouse_up(
            MouseButton::Right,
            cx.listener(|view, event, _, _| view.release(event)),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|view, event, _, _| view.release(event)),
        )
        .on_mouse_up_out(
            MouseButton::Right,
            cx.listener(|view, event, _, _| view.release(event)),
        )
        .on_mouse_move(cx.listener(|view, event, _, _| view.moved(event)))
        .on_scroll_wheel(cx.listener(|view, event, _, _| view.scrolled(event)))
        .on_key_down(cx.listener(|view, event, _, cx| view.key(event, cx)))
    }
}

/// Opens a screen's view.
pub(crate) fn open(screen: Screen, cx: &mut App) -> gpui::Entity<ScreenView> {
    use gpui::AppContext as _;
    cx.new(|cx| ScreenView::new(screen, cx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Key, Position};

    fn moved(held: Option<Button>) -> GuestInput {
        GuestInput::Pointer {
            phase: PointerPhase::Move { held },
            at: Position::new(0.5, 0.5).unwrap(),
            modifiers: Vec::new(),
        }
    }

    #[test]
    fn only_a_run_of_like_moves_collapses() {
        let hover = moved(None);
        let drag = moved(Some(Button::Left));
        let key = GuestInput::Key {
            key: Key::Character('a'),
            modifiers: Vec::new(),
        };
        assert!(InputPump::superseded(&hover, Some(&hover)));
        assert!(InputPump::superseded(&drag, Some(&drag)));
        assert!(!InputPump::superseded(&hover, Some(&drag)), "a drag starts");
        assert!(!InputPump::superseded(&hover, Some(&key)));
        assert!(!InputPump::superseded(&hover, None), "the last one is sent");
        assert!(
            !InputPump::superseded(&key, Some(&key)),
            "keys never collapse"
        );
    }

    #[test]
    fn a_display_point_lands_on_the_picture() {
        let picture = Picture {
            left: 10.0,
            top: 0.0,
            width: 640.0,
            height: 400.0,
            points: Size::new(1280.0, 800.0).unwrap(),
        };
        assert_eq!(picture.place(640.0, 400.0), (330.0, 200.0));
    }
}
