//! The window: a header with the pages and the machine slots, the page
//! shown, and dialogs and toasts over them. Each page is a component of
//! its own, in its own module: `machines` (the stage of machine panels,
//! each with its screen, a rail of the agents driving it and the tape of
//! their calls), `recordings` and `replay`. The window reads the daemon's
//! lists every two seconds (the daemon has no machine events yet) and
//! hands them to the machines page.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use doppel_machine::machine::MachineName;
use futures::StreamExt;
use gpui::{
    App, AppContext, Bounds, Context, Div, ElementId, Entity, FocusHandle, FontWeight, Hsla,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement, Render,
    SharedString, Stateful, StatefulInteractiveElement, Styled, Subscription, Task,
    TitlebarOptions, Window as GpuiWindow, WindowBounds, WindowOptions, deferred, div,
    prelude::FluentBuilder, px, relative, size,
};

mod machines;
mod recordings;
mod replay;

use crate::address::Entry;
use crate::ask::{Ask, AskEvent, Question};
use crate::icons::Icon;
use crate::model::{Machine, Session, Tone};
use crate::route::Route;
use crate::theme::{FONTS, MONO, SANS, Theme};
use crate::ui::{self, Button, Look};
use crate::{Daemon, DaemonError, LocalClock, Settings, ThemeChoice};
use machines::{MachinesEvent, MachinesPage};
use recordings::{Recordings, RecordingsEvent};
use replay::{ReplayEvent, ReplayPage};

/// How often the lists are read again.
const REFRESH: Duration = Duration::from_secs(2);
/// How long a done toast stays.
const TOAST_FOR: Duration = Duration::from_millis(4500);
/// Narrower than this, the header and a panel's footer keep what fits,
/// and a panel puts its rail under its screen.
const NARROW: f32 = 860.0;
/// The header's height, and a panel's footer's.
const HEADER: f32 = 40.0;
const FACTS: f32 = 24.0;
/// Narrower than this, a panel's footer keeps only what matters most.
const FACTS_NARROW: f32 = 520.0;
/// A panel's head row.
const HEAD: f32 = 36.0;
/// The strip above a live screen that holds who has it.
const RIM: f32 = 22.0;
/// The rail beside a panel's screen.
const RAIL: f32 = 264.0;
/// The group a segment's icon follows on hover.
const SEGMENT: &str = "doppel-segment";

/// Whether the window reaches the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Link {
    Connecting,
    Connected,
    Closed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToastTone {
    Done,
    Refused,
    /// It may or may not have happened.
    Unknown,
}

struct Toast {
    id: u64,
    tone: ToastTone,
    text: SharedString,
    _dismiss: Option<Task<()>>,
}

/// The page the main area shows: one for each kind of [`Route`].
enum Page {
    /// The machines page, which the window keeps.
    Machines,
    Recordings(Entity<Recordings>),
    Replay(Entity<ReplayPage>),
}

/// How the window came to a place, which decides what the address bar
/// does with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arrival {
    /// The window moved there (a click): a new entry, which Back returns
    /// from.
    Moved,
    /// The address bar was there first (the page opened at it, or Back
    /// and Forward moved it): it's settled in place, as the place shown.
    Followed,
}

/// How a segment of a hairline row looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Segment {
    Plain,
    /// The one picked.
    On,
    /// A toggle that is off.
    Faint,
    /// A person taking control: filled in the signal colour.
    Signal,
    /// Handing control back: the signal colour's outline.
    SignalLine,
}

pub(crate) struct Root {
    daemon: Arc<dyn Daemon>,
    clock: Arc<dyn LocalClock>,
    link: Link,
    page: Page,
    /// The machines page, kept while other pages show.
    machines: Entity<MachinesPage>,
    ask: Option<(Entity<Ask>, Subscription)>,
    toasts: Vec<Toast>,
    next_toast: u64,
    focus: FocusHandle,
    _poll: Task<()>,
    _machines_events: [Subscription; 2],
    _appearance: Subscription,
    settings: Arc<dyn Settings>,
    /// The theme chosen: the system's, light or dark.
    theme: ThemeChoice,
    /// Where the window is. [`Root::navigate`] alone changes it, and the
    /// page with it.
    route: Route,
    /// A machine a place named before the machines were listed: shown once
    /// they are.
    waiting: Option<MachineName>,
    /// The machine the next recordings page lists first.
    recordings_for: Option<MachineName>,
    /// What the recordings or replay page tells the window.
    _page_events: Option<Subscription>,
    /// Follows Back and Forward.
    _address: Task<()>,
}

impl Root {
    /// Opens the window, with a root view of its own, in `cx`, once the platform has launched
    /// (`platform::launch`). False when it can't be opened.
    pub(crate) fn open_window(
        daemon: Arc<dyn Daemon>,
        clock: Arc<dyn LocalClock>,
        settings: Arc<dyn Settings>,
        shown: Vec<MachineName>,
        cx: &mut App,
    ) -> bool {
        // Without them the window falls back to the system's fonts.
        let _ = cx
            .text_system()
            .add_fonts(FONTS.iter().map(|font| Cow::Borrowed(*font)).collect());
        cx.set_global(Theme::dark());
        // Opens zoomed to fill the screen: the machine's picture is the
        // point. Un-zoomed, it takes this centred size.
        let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Maximized(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("doppel".into()),
                    ..TitlebarOptions::default()
                }),
                window_min_size: Some(size(px(480.), px(360.))),
                ..WindowOptions::default()
            },
            |window, cx| {
                crate::platform::tag_window(window);
                cx.set_global(Theme::chosen(settings.theme(), window.appearance()));
                cx.new(|cx| Root::new(daemon, clock, settings, shown, window, cx))
            },
        )
        .is_ok()
    }

    fn new(
        daemon: Arc<dyn Daemon>,
        clock: Arc<dyn LocalClock>,
        settings: Arc<dyn Settings>,
        shown: Vec<MachineName>,
        window: &mut GpuiWindow,
        cx: &mut Context<Self>,
    ) -> Self {
        // Where the page was opened, from a link; a desktop window opens on
        // its machines.
        let start = crate::platform::address::current().map_or(Route::Machines, |path| {
            Route::parse(&path).unwrap_or(Route::Machines)
        });
        let address = cx.spawn(async move |root, cx| {
            let mut changes = crate::platform::address::changes();
            while let Some(path) = changes.next().await {
                let route = Route::parse(&path).unwrap_or(Route::Machines);
                let moved = root.update(cx, |root, cx| root.navigate(route, Arrival::Followed, cx));
                if moved.is_err() {
                    return;
                }
            }
        });
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let poll = cx.spawn(async move |root, cx| {
            loop {
                if root.update(cx, |root, cx| root.refresh(cx)).is_err() {
                    return;
                }
                cx.background_executor().timer(REFRESH).await;
            }
        });
        let machines = cx.new(|cx| MachinesPage::new(daemon.clone(), clock.clone(), shown, cx));
        let machines_events = [
            // Its header slots are drawn here.
            cx.observe(&machines, |_, _, cx| cx.notify()),
            cx.subscribe_in(&machines, window, |root, _, event, window, cx| {
                root.machines_event(event, window, cx);
            }),
        ];
        let appearance = cx.observe_window_appearance(window, |root: &mut Self, window, cx| {
            cx.set_global(Theme::chosen(root.theme, window.appearance()));
            window.refresh();
        });
        let mut root = Self {
            daemon,
            clock,
            link: Link::Connecting,
            page: Page::Machines,
            machines,
            ask: None,
            toasts: Vec::new(),
            next_toast: 0,
            focus,
            _poll: poll,
            _machines_events: machines_events,
            _appearance: appearance,
            theme: settings.theme(),
            settings,
            route: Route::Machines,
            waiting: None,
            recordings_for: None,
            _page_events: None,
            _address: address,
        };
        root.navigate(start, Arrival::Followed, cx);
        root
    }

    /// Moves the window to `route`: the one way its page changes. A place
    /// that can't be shown says why and shows the machines instead, and
    /// the address bar shows the place shown.
    fn navigate(&mut self, route: Route, arrival: Arrival, cx: &mut Context<Self>) {
        let shown = self.show(route, cx);
        match arrival {
            Arrival::Moved if shown == self.route => {}
            Arrival::Moved => crate::platform::address::show(&shown.path(), Entry::Push),
            Arrival::Followed => crate::platform::address::show(&shown.path(), Entry::Replace),
        }
        self.route = shown;
        cx.notify();
    }

    /// Shows `route`'s page, and says which place it shows.
    fn show(&mut self, route: Route, cx: &mut Context<Self>) -> Route {
        match route {
            Route::Machines => {
                self.enter_machines(cx);
                Route::Machines
            }
            Route::Machine(name) => {
                self.enter_machines(cx);
                match self.link {
                    Link::Connected => {}
                    // The machines aren't listed yet: it's shown once they
                    // are.
                    Link::Connecting | Link::Closed(_) => {
                        self.waiting = Some(name.clone());
                        return Route::Machine(name);
                    }
                }
                match self.machines.update(cx, |page, cx| page.bring(&name, cx)) {
                    Ok(()) => Route::Machine(name),
                    Err(why) => {
                        self.toast(ToastTone::Refused, format!("Can't show {name}: {why}."), cx);
                        Route::Machines
                    }
                }
            }
            Route::Recordings => {
                let only = self.recordings_for.take();
                let kept = match &self.page {
                    Page::Recordings(page) => page.read(cx).opened_for() == only.as_ref(),
                    Page::Machines | Page::Replay(_) => false,
                };
                if !kept {
                    let page = cx.new(|cx| {
                        Recordings::new(self.daemon.clone(), self.clock.clone(), only, cx)
                    });
                    self._page_events =
                        Some(cx.subscribe(&page, |root, _, event, cx| match event {
                            RecordingsEvent::Open(id) => {
                                root.navigate(Route::Replay(id.clone()), Arrival::Moved, cx);
                            }
                        }));
                    self.page = Page::Recordings(page);
                }
                self.machines.update(cx, |page, cx| page.hide(cx));
                Route::Recordings
            }
            Route::Replay(id) => {
                let kept = match &self.page {
                    Page::Replay(page) => page.read(cx).id() == &id,
                    Page::Machines | Page::Recordings(_) => false,
                };
                if !kept {
                    let (daemon, clock, opened) =
                        (self.daemon.clone(), self.clock.clone(), id.clone());
                    let page = cx.new(|cx| ReplayPage::open(daemon, clock, opened, cx));
                    self._page_events =
                        Some(cx.subscribe(&page, |root, _, event, cx| match event {
                            ReplayEvent::Back => {
                                root.navigate(Route::Recordings, Arrival::Moved, cx)
                            }
                        }));
                    self.page = Page::Replay(page);
                }
                self.machines.update(cx, |page, cx| page.hide(cx));
                Route::Replay(id)
            }
        }
    }

    /// Shows the recordings, of `only` first.
    fn recordings_of(&mut self, only: Option<MachineName>, cx: &mut Context<Self>) {
        self.recordings_for = only;
        self.navigate(Route::Recordings, Arrival::Moved, cx);
    }

    /// Moves to the next theme round (system, light, dark) and keeps it.
    fn switch_theme(&mut self, window: &mut GpuiWindow, cx: &mut Context<Self>) {
        self.theme = self.theme.next();
        cx.set_global(Theme::chosen(self.theme, window.appearance()));
        if let Err(error) = self.settings.keep_theme(self.theme) {
            self.toast(
                ToastTone::Refused,
                format!("The theme holds until the window closes: {error}"),
                cx,
            );
        }
        window.refresh();
    }

    /// Reads the machines and the sessions again.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let machines = self.daemon.machines();
        let sessions = self.daemon.sessions();
        cx.spawn(async move |root, cx| {
            let (machines, sessions) = futures::join!(machines, sessions);
            let _ = root.update(cx, |root, cx| root.listed(machines, sessions, cx));
        })
        .detach();
    }

    fn listed(
        &mut self,
        machines: Result<Vec<Machine>, DaemonError>,
        sessions: Result<Vec<Session>, DaemonError>,
        cx: &mut Context<Self>,
    ) {
        let machines = match machines {
            Ok(machines) => Some(machines),
            Err(DaemonError::Unreachable(why)) => {
                self.link = Link::Closed(why);
                self.machines.update(cx, |page, cx| page.disconnected(cx));
                cx.notify();
                return;
            }
            // Read again in a moment.
            Err(DaemonError::Refused(_)) => None,
        };
        let listed = machines.is_some();
        self.machines
            .update(cx, |page, cx| page.listed(machines, sessions.ok(), cx));
        if listed {
            self.link = Link::Connected;
            if let Some(name) = self.waiting.take() {
                self.navigate(Route::Machine(name), Arrival::Followed, cx);
            }
        }
        cx.notify();
    }

    /// Takes in what the machines page tells the window.
    fn machines_event(
        &mut self,
        event: &MachinesEvent,
        window: &mut GpuiWindow,
        cx: &mut Context<Self>,
    ) {
        match event {
            MachinesEvent::Toast(tone, text) => self.toast(*tone, text.clone(), cx),
            MachinesEvent::Ask(question) => self.ask(question.clone(), window, cx),
            MachinesEvent::Go(route) => self.navigate(route.clone(), Arrival::Moved, cx),
            MachinesEvent::Recordings(only) => self.recordings_of(only.clone(), cx),
            MachinesEvent::Changed => self.refresh(cx),
        }
    }

    /// The machines page, shown.
    fn enter_machines(&mut self, cx: &mut Context<Self>) {
        self.page = Page::Machines;
        self.machines.update(cx, |page, cx| page.show(cx));
    }

    fn toast(&mut self, tone: ToastTone, text: String, cx: &mut Context<Self>) {
        self.next_toast += 1;
        let id = self.next_toast;
        // A refusal or an unknown outcome stays until it's dismissed.
        let dismiss = match tone {
            ToastTone::Done => Some(cx.spawn(async move |root, cx| {
                cx.background_executor().timer(TOAST_FOR).await;
                let _ = root.update(cx, |root, cx| root.dismiss(id, cx));
            })),
            ToastTone::Refused | ToastTone::Unknown => None,
        };
        self.toasts.push(Toast {
            id,
            tone,
            text: text.into(),
            _dismiss: dismiss,
        });
        cx.notify();
    }

    fn dismiss(&mut self, id: u64, cx: &mut Context<Self>) {
        self.toasts.retain(|toast| toast.id != id);
        cx.notify();
    }

    fn ask(&mut self, question: Question, window: &mut GpuiWindow, cx: &mut Context<Self>) {
        let names = self.machines.read(cx).names();
        let daemon = self.daemon.clone();
        let now = self.clock.local();
        let ask = cx.new(|cx| Ask::new(question, daemon, names, now, window, cx));
        let events = cx.subscribe_in(&ask, window, |root, _, event, window, cx| match event {
            AskEvent::Submit(name, order) => {
                root.close_ask(window, cx);
                let (name, order) = (name.clone(), order.clone());
                root.machines
                    .update(cx, |page, cx| page.run(name, order, cx));
            }
            AskEvent::Close => {
                root.close_ask(window, cx);
                cx.notify();
            }
        });
        self.ask = Some((ask, events));
        cx.notify();
    }

    fn close_ask(&mut self, window: &mut GpuiWindow, cx: &mut App) {
        self.ask = None;
        window.focus(&self.focus, cx);
    }

    fn shortcut(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if event.keystroke.key != "escape" {
            return;
        }
        if self.machines.update(cx, |page, cx| page.close_launcher(cx)) {
            return;
        }
        // Escape leaves the split, unless it's meant for a dialog (or a
        // screen someone drives, which the page sees to).
        if let (Page::Machines, None) = (&self.page, &self.ask) {
            self.machines.update(cx, |page, cx| page.unsplit(cx));
        }
    }
}

/// The window's frame, drawn: the gate before the daemon answers, the
/// header, and the toasts.
impl Root {
    fn gate(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let body = match &self.link {
            Link::Connecting | Link::Connected => vec![
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(ui::spinner("connecting", 16., theme.muted))
                    .child("Connecting…")
                    .into_any_element(),
            ],
            Link::Closed(why) => vec![
                div()
                    .mt(px(12.))
                    .text_size(px(22.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.text)
                    .child("Disconnected")
                    .into_any_element(),
                div().child(format!("{why}.")).into_any_element(),
                div()
                    .flex()
                    .gap(px(8.))
                    .child(
                        Button::new(
                            "reconnect",
                            theme,
                            Look::Primary,
                            None,
                            Some("Reconnect".into()),
                        )
                        .on_press(cx.listener(|root, _, _, cx| {
                            root.link = Link::Connecting;
                            root.refresh(cx);
                            cx.notify();
                        })),
                    )
                    .into_any_element(),
            ],
        };
        div().size_full().flex().justify_center().child(
            div()
                .mt(relative(0.18))
                .w(px(460.))
                .px(px(16.))
                .flex()
                .flex_col()
                .gap(px(14.))
                .text_color(theme.muted)
                .child(ui::logo(theme, HEADER))
                .children(body),
        )
    }

    /// The header: the logo and the views on the left; on the right, what
    /// belongs to the view shown, then facts about the whole app. Nothing in
    /// it depends on which panel was clicked.
    fn header(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let machines_on = matches!(self.page, Page::Machines);
        let views =
            div()
                .flex()
                .flex_none()
                .h_full()
                .border_r_1()
                .border_color(theme.line)
                .child(
                    current(
                        segment(
                            "view-machines",
                            None,
                            Some("Machines"),
                            Segment::Plain,
                            theme,
                        ),
                        machines_on,
                        theme,
                    )
                    .px(px(14.))
                    .border_r_1()
                    .border_color(theme.line)
                    .on_click(cx.listener(|root, _, _, cx| {
                        root.navigate(Route::Machines, Arrival::Moved, cx)
                    })),
                )
                .child(
                    current(
                        segment(
                            "view-recordings",
                            None,
                            Some("Recordings"),
                            Segment::Plain,
                            theme,
                        ),
                        !machines_on,
                        theme,
                    )
                    .px(px(14.))
                    .on_click(cx.listener(|root, _, _, cx| {
                        root.navigate(Route::Recordings, Arrival::Moved, cx)
                    })),
                );
        let indicators = div()
            .flex()
            .flex_none()
            .h_full()
            .font_family(MONO)
            .text_size(px(10.5))
            .text_color(theme.muted)
            .whitespace_nowrap()
            .child(
                end_cell(theme)
                    .text_color(theme.live)
                    .child(ui::dot(6., theme.live))
                    .child("LIVE"),
            )
            .child(
                segment(
                    "theme",
                    Some(match self.theme {
                        ThemeChoice::System => Icon::Monitor,
                        ThemeChoice::Light => Icon::Sun,
                        ThemeChoice::Dark => Icon::Moon,
                    }),
                    None,
                    Segment::Plain,
                    theme,
                )
                .border_l_1()
                .border_color(theme.line)
                .on_click(cx.listener(|root, _, window, cx| root.switch_theme(window, cx))),
            );
        div()
            .flex()
            .flex_none()
            .h(px(HEADER))
            .min_w_0()
            .border_b_1()
            .border_color(theme.line)
            .bg(theme.raised)
            .child(ui::logo(theme, HEADER - 1.))
            .child(views)
            .children(
                (!self.machines.read(cx).is_empty())
                    .then(|| self.machines.update(cx, |page, cx| page.slots(theme, cx))),
            )
            .child(div().flex_1())
            .child(indicators)
    }

    /// The toasts, newest last, each with its dismiss button.
    fn toasts(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        div()
            .absolute()
            .right(px(16.))
            .bottom(px(16.))
            .w(px(380.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .children(self.toasts.iter().map(|toast| {
                let stripe = match toast.tone {
                    ToastTone::Done => theme.success,
                    ToastTone::Refused => theme.danger,
                    ToastTone::Unknown => theme.warning,
                };
                let dismissed = toast.id;
                div()
                    .relative()
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .pl(px(14.))
                    .pr(px(6.))
                    .py(px(10.))
                    .border_1()
                    .border_color(theme.line_strong)
                    .bg(theme.overlay)
                    .overflow_hidden()
                    .text_size(px(13.))
                    .text_color(theme.text)
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .top_0()
                            .bottom_0()
                            .w(px(3.))
                            .bg(stripe),
                    )
                    .child(div().flex_1().pt(px(5.)).child(toast.text.clone()))
                    .child(
                        Button::new(
                            ElementId::NamedInteger("toast".into(), toast.id),
                            theme,
                            Look::Ghost,
                            Some(Icon::X),
                            None,
                        )
                        .on_press(cx.listener(move |root, _, _, cx| root.dismiss(dismissed, cx))),
                    )
            }))
    }
}

impl Render for Root {
    fn render(&mut self, _window: &mut GpuiWindow, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let root = div()
            .id("root")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|root, event, _, cx| root.shortcut(event, cx)))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .text_color(theme.text)
            .font_family(SANS)
            .text_size(px(14.));
        if self.link != Link::Connected {
            return root.child(self.gate(&theme, cx));
        }
        let main = match &self.page {
            Page::Machines => self.machines.clone().into_any_element(),
            Page::Recordings(page) => page.clone().into_any_element(),
            Page::Replay(page) => page.clone().into_any_element(),
        };
        let root = root.child(self.header(&theme, cx)).child(
            div().flex_1().min_w_0().min_h_0().flex().child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(main),
            ),
        );
        let root = root.when_some(self.ask.as_ref(), |root, (ask, _)| {
            root.child(deferred(
                div()
                    .id("backdrop")
                    .occlude()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme.backdrop)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|root, _, window, cx| {
                            root.close_ask(window, cx);
                            cx.notify();
                        }),
                    )
                    .child(ask.clone()),
            ))
        });
        root.child(deferred(self.toasts(&theme, cx)).with_priority(1))
    }
}

/// A machine state's square dot.
fn dot(tone: Tone, theme: &Theme) -> Div {
    match tone {
        Tone::Stopped => ui::dot(7., theme.faint),
        Tone::Running | Tone::Paused | Tone::Busy | Tone::Error => ui::dot(7., ink(tone, theme)),
    }
}
fn id(prefix: &str, name: &MachineName) -> ElementId {
    ElementId::Name(format!("{prefix}-{name}").into())
}
/// A button in a hairline row (the header's segments, a panel's head): as
/// high as the row, square, its label in small mono upper case. The caller
/// adds the hairline on the side it needs.
fn segment(
    id: impl Into<ElementId>,
    icon: Option<Icon>,
    label: Option<&str>,
    look: Segment,
    theme: &Theme,
) -> Stateful<Div> {
    let (fill, ink, hover_fill, hover_ink) = match look {
        Segment::Plain => (
            gpui::transparent_black(),
            theme.muted,
            theme.hover,
            theme.text,
        ),
        Segment::On => (theme.slab, theme.on_slab, theme.slab, theme.on_slab),
        Segment::Faint => (
            gpui::transparent_black(),
            theme.faint,
            theme.hover,
            theme.text,
        ),
        Segment::Signal => (
            theme.signal,
            theme.on_signal,
            theme.signal.blend(theme.bg.opacity(0.15)),
            theme.on_signal,
        ),
        Segment::SignalLine => (
            gpui::transparent_black(),
            theme.signal,
            theme.signal.opacity(0.12),
            theme.signal,
        ),
    };
    div()
        .id(id)
        .group(SEGMENT)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .h_full()
        .px(px(12.))
        .bg(fill)
        .text_color(ink)
        .font_family(MONO)
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .cursor_pointer()
        .hover(move |segment| segment.bg(hover_fill).text_color(hover_ink))
        .children(icon.map(|icon| {
            icon.svg(13., ink)
                .group_hover(SEGMENT, move |icon| icon.text_color(hover_ink))
        }))
        .children(label.map(ui::upper))
}
/// A header view, marked as the one shown when `on`: strong words over a
/// green underline, as the browser viewer marks it.
fn current(segment: Stateful<Div>, on: bool, theme: &Theme) -> Stateful<Div> {
    segment.relative().when(on, |segment| {
        segment.text_color(theme.strong).child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .h(px(2.))
                .bg(theme.live),
        )
    })
}
/// A cell at the right end of a row of facts.
fn end_cell(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .px(px(12.))
        .border_l_1()
        .border_color(theme.line)
}

/// The colour a machine state's label and dot are drawn in.
fn ink(tone: Tone, theme: &Theme) -> Hsla {
    match tone {
        Tone::Running => theme.success,
        Tone::Paused => theme.warning,
        Tone::Busy => theme.accent,
        Tone::Stopped => theme.muted,
        Tone::Error => theme.danger,
    }
}
