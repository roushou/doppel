//! The window: a header with the machine switcher, the pages and the
//! layouts, the page shown, a status bar of facts about the current
//! machine, and dialogs and toasts over them. Each page is a module:
//! `machines` (the stage of machine panels, each with its screen, a rail of
//! the agents driving it and the tape of their calls), `recordings` and
//! `replay`. It reads the daemon's lists every two seconds (the daemon has
//! no machine events yet) and the current machine's report every five, and
//! watches what agents do for the overlays and tapes.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use doppel_machine::machine::MachineName;
use futures::StreamExt;
use futures::channel::oneshot;
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
use crate::inspect::{Inspect, Pointing};
use crate::model::{Action, Bay, Hold, Machine, Order, Session, Tone, Verdict};
use crate::panel::Panels;
use crate::report::Report;
use crate::route::Route;
use crate::screen::{self, ScreenEvent, ScreenView};
use crate::stage::{Split, Stage};
use crate::theme::{FONTS, MONO, SANS, Theme};
use crate::timeline;
use crate::ui::{self, Button, Look};
use crate::{Daemon, DaemonError, LocalClock, Settings, ThemeChoice};
use machines::ElementsShown;
use recordings::Recordings;

/// How far back a live timeline shows.
const WINDOW_MS: f64 = 60_000.0;
/// How often the lists are read again.
const REFRESH: Duration = Duration::from_secs(2);
/// How long to wait before watching activity again after the stream ends.
const REWATCH: Duration = Duration::from_secs(3);
/// How long a done toast stays.
const TOAST_FOR: Duration = Duration::from_millis(4500);
/// How often the staged machines' reports are read again.
const REPORT_EVERY: Duration = Duration::from_secs(5);
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

/// Opens the window.
pub(crate) struct Window;

impl Window {
    /// Opens the window in `cx`, once the platform has launched
    /// (`platform::launch`). False when it can't be opened.
    pub(crate) fn open(
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
}

/// Whether the window reaches the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Page {
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

/// What the main area shows.
enum View {
    Machines,
    Recordings(Entity<Recordings>),
}

/// An entry in a panel's menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuItem {
    Recordings,
    Act(Action),
}

/// What a panel's screen is looked at through: the screen alone; the
/// elements the agent last saw (their boxes over it, their tree in the
/// rail), with what is pointed at and picked; or the text read off it.
enum Lens {
    Screen,
    Elements(Inspect),
    Text,
}

/// A lens, as its radio offers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LensKind {
    Screen,
    Elements,
    Text,
}

impl LensKind {
    const ALL: [Self; 3] = [Self::Screen, Self::Elements, Self::Text];

    const fn label(self) -> &'static str {
        match self {
            Self::Screen => "screen",
            Self::Elements => "elements",
            Self::Text => "text",
        }
    }

    fn lens(self) -> Lens {
        match self {
            Self::Screen => Lens::Screen,
            Self::Elements => Lens::Elements(Inspect::new()),
            Self::Text => Lens::Text,
        }
    }
}

impl Lens {
    const fn kind(&self) -> LensKind {
        match self {
            Self::Screen => LensKind::Screen,
            Self::Elements(_) => LensKind::Elements,
            Self::Text => LensKind::Text,
        }
    }
}

/// Whether the machine switcher's list is open.
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

/// A machine's open screen, and the window's subscription to it.
struct Shown {
    view: Entity<ScreenView>,
    _events: Subscription,
}

/// What the window holds for a machine's panel besides its screen: its
/// lens (with the elements' scroll and layer).
struct Handles {
    lens: Lens,
}

impl Default for Handles {
    fn default() -> Self {
        Self { lens: Lens::Screen }
    }
}

struct Root {
    daemon: Arc<dyn Daemon>,
    clock: Arc<dyn LocalClock>,
    /// The timelines' zero.
    /// When the window opened, on the clock in ms.
    origin: f64,
    page: Page,
    view: View,
    /// How the bays and their machines are laid out.
    stage: Stage,
    machines: Vec<Machine>,
    sessions: Vec<Session>,
    /// What the window keeps about each machine.
    panels: Panels<Shown, Handles>,
    /// The panel whose menu is open.
    menu: Option<MachineName>,
    /// The menu a press outside last closed, and when.
    menu_closed: Option<(MachineName, f64)>,
    ask: Option<(Entity<Ask>, Subscription)>,
    toasts: Vec<Toast>,
    next_toast: u64,
    focus: FocusHandle,
    /// Redraws a little later, while a timeline shows calls but nothing moves.
    later: Option<Task<()>>,
    _poll: Task<()>,
    _reporting: Task<()>,
    _watch: Task<()>,
    _appearance: Subscription,
    settings: Arc<dyn Settings>,
    /// The theme chosen: the system's, light or dark.
    theme: ThemeChoice,
    /// The place last shown in the address bar; `None` before the first.
    route: Option<Route>,
    /// Follows Back and Forward.
    _address: Task<()>,
}

impl Root {
    fn new(
        daemon: Arc<dyn Daemon>,
        clock: Arc<dyn LocalClock>,
        settings: Arc<dyn Settings>,
        shown: Vec<MachineName>,
        window: &mut GpuiWindow,
        cx: &mut Context<Self>,
    ) -> Self {
        // Opened at a place: a link to a machine, or to a replay.
        let start = crate::platform::address::current().and_then(|path| Route::parse(&path));
        let shown = match (&start, shown.is_empty()) {
            (Some(Route::Machine(name)), true) => vec![name.clone()],
            (Some(_) | None, _) => shown,
        };
        let address = cx.spawn(async move |root, cx| {
            let mut changes = crate::platform::address::changes();
            while let Some(path) = changes.next().await {
                let moved = root.update(cx, |root, cx| {
                    let route = Route::parse(&path).unwrap_or(Route::Machines);
                    // Already where the address bar says: nothing to show.
                    root.route = Some(route.clone());
                    root.go(&route, cx);
                });
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
        let reporting = cx.spawn(async move |root, cx| {
            loop {
                cx.background_executor().timer(REPORT_EVERY).await;
                if root.update(cx, |root, cx| root.read_reports(cx)).is_err() {
                    return;
                }
            }
        });
        let watch = Self::watch(daemon.clone(), cx);
        let appearance = cx.observe_window_appearance(window, |root: &mut Self, window, cx| {
            cx.set_global(Theme::chosen(root.theme, window.appearance()));
            window.refresh();
        });
        let mut root = Self {
            origin: clock.millis(),
            daemon,
            clock,
            page: Page::Connecting,
            view: View::Machines,
            stage: Stage::of(shown),
            machines: Vec::new(),
            sessions: Vec::new(),
            panels: Panels::default(),
            menu: None,
            menu_closed: None,
            ask: None,
            toasts: Vec::new(),
            next_toast: 0,
            focus,
            later: None,
            _poll: poll,
            _reporting: reporting,
            _watch: watch,
            _appearance: appearance,
            theme: settings.theme(),
            settings,
            route: None,
            _address: address,
        };
        if let Some(start) = start {
            root.go(&start, cx);
        }
        root
    }

    /// Shows `route`'s place. A machine that isn't in a bay leaves the
    /// stage as it is.
    fn go(&mut self, route: &Route, cx: &mut Context<Self>) {
        match route {
            Route::Machines => self.machines_view(cx),
            Route::Machine(name) => match self.stage.slot_of(name, &self.machines) {
                Some(slot) => self.show_slot(slot, cx),
                None => self.machines_view(cx),
            },
            Route::Recordings | Route::Replay(_) => {
                self.recordings_view(None, cx);
                if let View::Recordings(page) = &self.view {
                    let id = match route {
                        Route::Replay(id) => Some(id.clone()),
                        Route::Machines | Route::Machine(_) | Route::Recordings => None,
                    };
                    page.update(cx, |page, cx| {
                        if page.replaying(cx) != id {
                            match id {
                                Some(id) => page.open(id, cx),
                                None => page.load(cx),
                            }
                        }
                    });
                }
            }
        }
    }

    /// Where the window is, as a place; `None` until it knows the machines.
    fn here(&self, cx: &App) -> Option<Route> {
        match (&self.view, &self.page) {
            (View::Recordings(page), _) => Some(
                page.read(cx)
                    .replaying(cx)
                    .map_or(Route::Recordings, Route::Replay),
            ),
            (View::Machines, Page::Connected) => Some(
                self.stage
                    .shows(&self.machines)
                    .first()
                    .map_or(Route::Machines, |machine| {
                        Route::Machine(machine.name.clone())
                    }),
            ),
            (View::Machines, Page::Connecting | Page::Closed(_)) => None,
        }
    }

    /// Shows where the window is in the address bar, once it has moved:
    /// the first place in place of the one the page opened at, each next
    /// one as an entry Back returns from.
    fn show_address(&mut self, cx: &App) {
        let Some(here) = self.here(cx) else {
            return;
        };
        if self.route.as_ref() == Some(&here) {
            return;
        }
        let entry = match self.route {
            Some(_) => Entry::Push,
            None => Entry::Replace,
        };
        crate::platform::address::show(&here.path(), entry);
        self.route = Some(here);
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

    /// Milliseconds on the timelines' clock.
    /// Takes in what a person did in `name`'s elements panel.
    fn inspected(&mut self, name: &MachineName, pointing: Pointing, cx: &mut Context<Self>) {
        let now = self.now();
        if pointing == Pointing::Look {
            self.look(name, now, cx);
            return;
        }
        let panel = self.panels.of(name);
        let seen = ElementsShown::of(panel.timeline(), panel.look(), now)
            .snapshot
            .ok();
        let Lens::Elements(inspect) = &mut panel.handles_mut().lens else {
            return;
        };
        let before = (inspect.pointed, inspect.picked);
        let changes = !matches!(pointing, Pointing::Hover(_) | Pointing::Leave(_));
        if !inspect.apply(pointing, seen.as_deref()) {
            panel.handles_mut().lens = Lens::Screen;
        } else if !changes && before == (inspect.pointed, inspect.picked) {
            // The pointer moved within the same element: nothing to redraw.
            return;
        }
        cx.notify();
    }

    /// Takes a person's own look at `name`'s elements: the agents driving
    /// it lose nothing (no turn, and the guest keeps none of it).
    fn look(&mut self, name: &MachineName, now: f64, cx: &mut Context<Self>) {
        self.panels.of(name).looking(now);
        cx.notify();
        let observed = self.daemon.observe(name);
        let name = name.clone();
        cx.spawn(async move |root, cx| {
            let observed = observed.await;
            let _ = root.update(cx, |root, cx| {
                // Only the latest look's answer counts.
                let outcome = match observed {
                    Ok(snapshot) => timeline::Looked::Seen(Arc::new(snapshot)),
                    Err(error) => timeline::Looked::Failed(error.to_string()),
                };
                if root.panels.of(&name).looked(now, outcome) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Reads `name`'s screen text for a person: the host's view of the
    /// display, so the agents driving it lose nothing.
    fn read_now(&mut self, name: &MachineName, cx: &mut Context<Self>) {
        let now = self.now();
        self.panels.of(name).reading(now);
        cx.notify();
        let read = self.daemon.observe_text(name);
        let name = name.clone();
        cx.spawn(async move |root, cx| {
            let read = read.await;
            let _ = root.update(cx, |root, cx| {
                // Only the latest read's answer counts.
                let read = read.map(Arc::new).map_err(|error| error.to_string());
                if root.panels.of(&name).read(now, read) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn now(&self) -> f64 {
        self.clock.millis() - self.origin
    }

    /// Watches what agents do, opening the stream again a little after it
    /// ends or fails; the events land on the timelines.
    fn watch(daemon: Arc<dyn Daemon>, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |root, cx| {
            loop {
                if let Ok(mut activity) = daemon.watch().await {
                    while let Some(Ok(event)) = activity.next().await {
                        let recorded = root.update(cx, |root, cx| {
                            let now = root.now();
                            root.panels.record(event, now);
                            cx.notify();
                        });
                        if recorded.is_err() {
                            return;
                        }
                    }
                }
                cx.background_executor().timer(REWATCH).await;
            }
        })
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
        match machines {
            Ok(machines) => {
                self.page = Page::Connected;
                self.machines = machines;
            }
            Err(DaemonError::Unreachable(why)) => {
                self.page = Page::Closed(why);
                self.panels.disconnected();
                cx.notify();
                return;
            }
            // Read again in a moment.
            Err(DaemonError::Refused(_)) => {}
        }
        if let Ok(sessions) = sessions {
            self.sessions = sessions;
        }
        self.sync_screens(cx);
        // A machine just on stage: its report needn't wait for the next read.
        if !self.panels.reports_for(&self.stage()) {
            self.read_reports(cx);
        }
        cx.notify();
    }

    /// Reads the report of every machine on stage, for its panel's footer.
    /// A failed read keeps the last one.
    fn read_reports(&mut self, cx: &mut Context<Self>) {
        let stage = self.stage();
        self.panels.keep_reports_of(&stage);
        for name in stage {
            let read = self.daemon.report(&name);
            cx.spawn(async move |root, cx| {
                let Ok(read) = read.await else {
                    return;
                };
                let _ = root.update(cx, |root, cx| {
                    let stage = root.stage();
                    if root.panels.reported(&name, Report::parse(&read), &stage) {
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    /// The machines in the bays now.
    fn stage(&self) -> Vec<MachineName> {
        self.stage.names(&self.machines)
    }

    /// Shows slot `index`'s machine alone.
    fn show_slot(&mut self, index: usize, cx: &mut Context<Self>) {
        self.stage.show(index);
        if !matches!(self.view, View::Machines) {
            self.machines_view(cx);
        }
        cx.notify();
    }

    fn machine(&self, name: &MachineName) -> Option<&Machine> {
        self.machines.iter().find(|machine| &machine.name == name)
    }

    /// Opens the screens of live machines on stage, and closes the rest:
    /// a screen that ended is opened again.
    fn sync_screens(&mut self, cx: &mut Context<Self>) {
        let on_stage = match self.view {
            View::Machines => self.stage(),
            View::Recordings(_) => Vec::new(),
        };
        let wanted: Vec<MachineName> = on_stage
            .into_iter()
            .filter(|name| {
                self.machine(name)
                    .is_some_and(|machine| machine.state.is_live())
            })
            .collect();
        let open = self
            .panels
            .sync_screens(&wanted, |shown| shown.view.read(cx).ended());
        for name in open {
            let opened = self.daemon.open_screen(&name);
            cx.spawn(async move |root, cx| {
                let opened = opened.await;
                let _ = root.update(cx, |root, cx| {
                    match opened {
                        Ok(screen) => {
                            let view = screen::open(screen, cx);
                            let machine = name.clone();
                            let events = cx.subscribe(&view, move |root, _, event, cx| {
                                root.screen_event(&machine, event, cx);
                            });
                            root.panels.of(&name).opened(Ok(Shown {
                                view,
                                _events: events,
                            }));
                        }
                        Err(error) => {
                            root.panels
                                .of(&name)
                                .opened(Err(format!("no screen: {error}")));
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
        }
    }

    fn screen_event(&mut self, name: &MachineName, event: &ScreenEvent, cx: &mut Context<Self>) {
        match event {
            ScreenEvent::Steering => {}
            ScreenEvent::Refused(why) => {
                self.toast(ToastTone::Refused, format!("{name}: {why}"), cx)
            }
            // Opened again with the next lists, if it's still live.
            ScreenEvent::Ended => {}
        }
        cx.notify();
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

    /// Whether the window has control of `name`.
    /// The inspection shown in `name`'s panel: its elements lens, while
    /// the machine is `live`.
    fn inspecting(&self, name: &MachineName, live: bool) -> Option<&Inspect> {
        match self.panels.get(name).map(|panel| &panel.handles().lens) {
            Some(Lens::Elements(inspect)) if live => Some(inspect),
            Some(Lens::Elements(_) | Lens::Screen | Lens::Text) | None => None,
        }
    }

    fn driving(&self, name: &MachineName, cx: &App) -> bool {
        self.panels
            .get(name)
            .and_then(|panel| panel.screen().open())
            .is_some_and(|shown| shown.view.read(cx).driving())
    }

    /// Holds a session's calls, or lets them go on.
    fn hold(&mut self, session: &Session, hold: Hold, cx: &mut Context<Self>) {
        let held = self.daemon.hold(session.id, hold);
        let who = session.who().to_owned();
        cx.spawn(async move |root, cx| {
            let held = held.await;
            let _ = root.update(cx, |root, cx| {
                if let Err(error) = held {
                    root.toast(ToastTone::Refused, format!("{who}: {error}"), cx);
                }
                root.refresh(cx);
            });
        })
        .detach();
    }

    /// Starts an action the person picked: one that needs more asks first.
    fn pick(
        &mut self,
        name: &MachineName,
        action: Action,
        window: &mut GpuiWindow,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        let Some(machine) = self.machine(name).cloned() else {
            return;
        };
        match Question::before(action, &machine) {
            Some(question) => self.ask(question, window, cx),
            None => {
                let order = match action {
                    Action::Start => Order::Start,
                    Action::Stop => Order::Stop,
                    Action::Pause => Order::Pause,
                    Action::Resume => Order::Resume,
                    // Asked about first; a golden image no longer golden
                    // by now is started as asked.
                    Action::StartGolden => Order::Start,
                    // Asked about first.
                    Action::FromTemplate
                    | Action::ForceStop
                    | Action::Checkpoint
                    | Action::RollBack
                    | Action::Restore
                    | Action::Fork
                    | Action::NewCopy
                    | Action::Delete => return,
                };
                self.run(name.clone(), order, cx);
            }
        }
    }

    fn ask(&mut self, question: Question, window: &mut GpuiWindow, cx: &mut Context<Self>) {
        let names = self
            .machines
            .iter()
            .map(|machine| machine.name.clone())
            .collect();
        let daemon = self.daemon.clone();
        let now = self.clock.local();
        let ask = cx.new(|cx| Ask::new(question, daemon, names, now, window, cx));
        let events = cx.subscribe_in(&ask, window, |root, _, event, window, cx| match event {
            AskEvent::Submit(name, order) => {
                root.close_ask(window, cx);
                root.run(name.clone(), order.clone(), cx);
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

    /// Carries out an order, handing control back first when it ends what
    /// the person is doing, and says how it went.
    fn run(&mut self, name: MachineName, order: Order, cx: &mut Context<Self>) {
        let action = order.action();
        if !self.panels.of(&name).begin(action) {
            return;
        }
        let handed_back = if action.ends_control() {
            self.panels
                .get(&name)
                .and_then(|panel| panel.screen().open())
                .map(|shown| shown.view.clone())
                .and_then(|view| view.update(cx, |view, cx| view.hand_back(cx)))
        } else {
            None
        };
        let daemon = self.daemon.clone();
        cx.spawn(async move |root, cx| {
            if let Some(handed_back) = handed_back {
                let _ = handed_back.await;
            }
            let verdict = daemon.run(&name, &order).await;
            let _ = root.update(cx, |root, cx| {
                root.panels.of(&name).finish();
                match verdict {
                    Verdict::Done { note, made } => {
                        let said = note.unwrap_or_else(|| order.done());
                        root.toast(ToastTone::Done, format!("{name}: {said}"), cx);
                        // A machine made running takes an empty bay by itself.
                        let _ = made;
                    }
                    Verdict::Refused(why) => {
                        root.toast(ToastTone::Refused, format!("{name}: {why}"), cx);
                    }
                    Verdict::Unknown(why) => root.toast(
                        ToastTone::Unknown,
                        format!("{name}: it may or may not have happened. {why}"),
                        cx,
                    ),
                }
                root.refresh(cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// Shows the machines.
    fn machines_view(&mut self, cx: &mut Context<Self>) {
        self.view = View::Machines;
        self.sync_screens(cx);
        cx.notify();
    }

    /// Shows the recordings, of `only` first when it names a machine.
    /// Leaving the machines hands back the screens the person controls,
    /// so no agent waits on a screen no one watches, and closes them.
    fn recordings_view(&mut self, only: Option<MachineName>, cx: &mut Context<Self>) {
        self.menu = None;
        let kept = match &self.view {
            View::Recordings(page) if page.read(cx).opened_for() == only.as_ref() => true,
            View::Recordings(_) | View::Machines => false,
        };
        if !kept {
            let page =
                cx.new(|cx| Recordings::new(self.daemon.clone(), self.clock.clone(), only, cx));
            self.view = View::Recordings(page);
        }
        let screens: Vec<Entity<ScreenView>> = self
            .panels
            .screens()
            .map(|shown| shown.view.clone())
            .collect();
        let handed: Vec<oneshot::Receiver<()>> = screens
            .into_iter()
            .filter_map(|view| view.update(cx, |view, cx| view.hand_back(cx)))
            .collect();
        if handed.is_empty() {
            self.sync_screens(cx);
        } else {
            cx.spawn(async move |root, cx| {
                for handed in handed {
                    let _ = handed.await;
                }
                let _ = root.update(cx, |root, cx| root.sync_screens(cx));
            })
            .detach();
        }
        cx.notify();
    }

    /// Starts `name` from bay `index`: it lands there once it's live.
    fn start_in(
        &mut self,
        index: usize,
        name: &MachineName,
        window: &mut GpuiWindow,
        cx: &mut Context<Self>,
    ) {
        self.stage.start_in(index, name.clone());
        self.pick(name, Action::Start, window, cx);
    }

    fn shortcut(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if event.keystroke.key != "escape" {
            return;
        }
        if self.stage.close_launcher() {
            cx.notify();
            return;
        }
        // Escape leaves the split, unless it's meant for a screen someone
        // drives or a dialog.
        let driving = self
            .panels
            .screens()
            .any(|shown| shown.view.read(cx).driving());
        if let (View::Machines, None, false) = (&self.view, &self.ask, driving)
            && self.stage.unsplit()
        {
            cx.notify();
        }
    }

    /// Keeps the clock moving: every frame while something moves, a few
    /// times a second while a timeline still shows a call, and not at all
    /// once every timeline is empty.
    fn tick(&mut self, now: f64, window: &mut GpuiWindow, cx: &mut Context<Self>) {
        let moving = self
            .panels
            .iter()
            .any(|(_, panel)| timeline::moving(panel.timeline(), now));
        if moving {
            window.request_animation_frame();
            return;
        }
        let showing = self
            .panels
            .iter()
            .any(|(_, panel)| now - timeline::latest(panel.timeline(), now) < WINDOW_MS);
        if showing && self.later.is_none() {
            self.later = Some(cx.spawn(async move |root, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let _ = root.update(cx, |root, cx| {
                    root.later = None;
                    cx.notify();
                });
            }));
        }
    }
}

/// The window's frame, drawn: the gate before the daemon answers, the
/// header, and the toasts.
impl Root {
    fn gate(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let body = match &self.page {
            Page::Connecting | Page::Connected => vec![
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(ui::spinner("connecting", 16., theme.muted))
                    .child("Connecting…")
                    .into_any_element(),
            ],
            Page::Closed(why) => vec![
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
                            root.page = Page::Connecting;
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

    /// The sessions whose requests named a machine.
    fn drivers<'a>(&'a self, name: &'a MachineName) -> impl Iterator<Item = &'a Session> {
        self.sessions
            .iter()
            .filter(move |session| session.machines.contains(name))
    }

    /// The header: the logo and the views on the left; on the right, what
    /// belongs to the view shown, then facts about the whole app. Nothing in
    /// it depends on which panel was clicked.
    fn header(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let machines_on = matches!(self.view, View::Machines);
        let views = div()
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
                .on_click(cx.listener(|root, _, _, cx| root.machines_view(cx))),
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
                .on_click(cx.listener(|root, _, _, cx| root.recordings_view(None, cx))),
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
            .children((!self.machines.is_empty()).then(|| self.slots(theme, cx)))
            .child(div().flex_1())
            .child(indicators)
    }

    /// The two slots (Apple's cap of two running macOS machines): a tab per
    /// running machine, a + for a free slot that opens what can start
    /// there, and with both running, a button to show them side by side.
    fn slots(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let machines_on = matches!(self.view, View::Machines);
        let shown = self.stage.bays(&self.machines);
        let stage: Vec<MachineName> = self
            .stage
            .shows(&self.machines)
            .into_iter()
            .map(|machine| machine.name.clone())
            .collect();
        let full = shown.iter().all(|bay| matches!(bay, Bay::Machine(_)));
        let mut row = div().flex().flex_none().h_full();
        for (index, bay) in shown.iter().enumerate() {
            match bay {
                Bay::Machine(machine) => {
                    let on = machines_on && stage.contains(&machine.name);
                    let dot = if self
                        .panels
                        .get(&machine.name)
                        .and_then(|panel| panel.screen().open())
                        .is_some_and(|shown| shown.view.read(cx).driving())
                    {
                        ui::dot(7., theme.signal)
                    } else {
                        dot(machine.state.look().1, theme)
                    };
                    row = row.child(
                        div()
                            .id(id("slot", &machine.name))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .h_full()
                            .px(px(14.))
                            .border_r_1()
                            .border_color(theme.line)
                            .font_family(MONO)
                            .text_size(px(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if on { theme.strong } else { theme.muted })
                            .relative()
                            .when(on, |slot| {
                                slot.bg(theme.bg).child(
                                    div()
                                        .absolute()
                                        .left_0()
                                        .right_0()
                                        .bottom_0()
                                        .h(px(2.))
                                        .bg(theme.live),
                                )
                            })
                            .cursor_pointer()
                            .hover(|slot| slot.bg(theme.hover))
                            .child(dot)
                            .child(machine.name.to_string())
                            .on_click(cx.listener(move |root, _, _, cx| root.show_slot(index, cx))),
                    );
                }
                Bay::Empty(_) => {
                    let open = self.stage.launching() == Some(index);
                    let mut free = div().relative().flex().h_full().child(
                        segment(
                            ElementId::NamedInteger("slot-free".into(), index as u64),
                            Some(Icon::Plus),
                            None,
                            if open { Segment::On } else { Segment::Plain },
                            theme,
                        )
                        .w(px(44.))
                        .border_r_1()
                        .border_color(theme.line)
                        .on_click(cx.listener(move |root, _, _, cx| {
                            root.stage.toggle_launcher(index);
                            cx.notify();
                        })),
                    );
                    if open {
                        free = free.child(
                            deferred(
                                div()
                                    .id("launcher")
                                    .occlude()
                                    .absolute()
                                    .top(px(HEADER - 1.))
                                    .left_0()
                                    .w(px(380.))
                                    .max_h(px(560.))
                                    .overflow_y_scroll()
                                    .p(px(14.))
                                    .border_1()
                                    .border_color(theme.line_strong)
                                    .bg(theme.overlay)
                                    .on_mouse_down_out(cx.listener(|root, _, _, cx| {
                                        // The menus inside it close it as they open; a click
                                        // elsewhere does too.
                                        if root.menu.is_none() && root.stage.close_launcher() {
                                            cx.notify();
                                        }
                                    }))
                                    .child(self.launcher(index, theme, cx)),
                            )
                            .with_priority(2),
                        );
                    }
                    row = row.child(free);
                }
            }
        }
        if full && machines_on {
            row = row.child(
                segment(
                    "split",
                    Some(Icon::Columns2),
                    None,
                    match self.stage.split() {
                        Split::On => Segment::On,
                        Split::Off => Segment::Plain,
                    },
                    theme,
                )
                .w(px(44.))
                .border_r_1()
                .border_color(theme.line)
                .on_click(cx.listener(|root, _, _, cx| {
                    root.stage.toggle_split();
                    cx.notify();
                })),
            );
        }
        row
    }

    /// A panel's footer: passive facts about its machine, glanced at, never
    /// acted on.
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
    fn render(&mut self, window: &mut GpuiWindow, cx: &mut Context<Self>) -> impl IntoElement {
        self.show_address(cx);
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
        if self.page != Page::Connected {
            return root.child(self.gate(&theme, cx));
        }
        let now = self.now();
        self.tick(now, window, cx);
        let width = f32::from(window.viewport_size().width);
        let narrow = width < NARROW;
        let main = match &self.view {
            View::Machines => self.stage_view(width, narrow, now, &theme, cx),
            View::Recordings(page) => page.clone().into_any_element(),
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
