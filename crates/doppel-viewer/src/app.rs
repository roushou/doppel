//! The window: the browser viewer's `App.svelte` and `Panel.svelte`. A
//! header with the machine switcher, the view and the layouts; the stage of
//! machine panels, each with its screen, a rail of the agents driving it and
//! the tape of their calls; a status bar of facts about the current machine;
//! dialogs and toasts over them. It reads the daemon's lists every two
//! seconds (the daemon has no machine events yet) and the current machine's
//! report every five, watches what agents do for the overlays and tapes, and
//! makes every daemon call from a thread of its own.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::{Duration, Instant};

use doppel_machine::machine::MachineName;
use futures::StreamExt;
use futures::channel::oneshot;
use gpui::{
    AnyElement, App, AppContext, Bounds, Context, Div, ElementId, Entity, FocusHandle, FontWeight,
    Hsla, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement, Render,
    SharedString, Stateful, StatefulInteractiveElement, Styled, Subscription, Task,
    TitlebarOptions, Window as GpuiWindow, WindowBounds, WindowOptions, deferred, div,
    prelude::FluentBuilder, px, relative, rgb, size,
};

use crate::ask::{Ask, AskEvent, Choice, Question};
use crate::elements::Named;
use crate::icons::{Assets, Icon};
use crate::inspect::{self, Act, Empty, Inspect, Looking, Pointing};
use crate::model::{
    Action, Bay, Group, Hold, Machine, MachineState, Order, Provenance, Run, Session,
    SessionActivity, Tone, Verdict, forks_of,
};
use crate::overlay;
use crate::panel::{Overlay, Panels};
use crate::reading;
use crate::replay::Recordings;
use crate::report::{self, Guest, Login, Report};
use crate::screen::{self, ScreenEvent, ScreenView};
use crate::stage::{Split, Stage};
use crate::steering::Steering;
use crate::theme::{FONTS, Hue, MONO, SANS, Theme};
use crate::timeline;
use crate::ui::{self, Button, Enabled, Look};
use crate::{Daemon, DaemonError, LocalClock, Settings, ThemeChoice};

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
    pub(crate) fn run(
        daemon: Arc<dyn Daemon>,
        clock: Arc<dyn LocalClock>,
        settings: Arc<dyn Settings>,
        shown: Vec<MachineName>,
    ) {
        gpui_platform::application()
            .with_assets(Assets)
            .run(move |cx: &mut App| {
                // Without them the window falls back to the system's fonts.
                let _ = cx
                    .text_system()
                    .add_fonts(FONTS.iter().map(|font| Cow::Borrowed(*font)).collect());
                cx.set_global(Theme::dark());
                // Opens zoomed to fill the screen: the machine's picture is
                // the point. Un-zoomed, it takes this centred size.
                let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
                let opened = cx.open_window(
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
                );
                if opened.is_err() {
                    cx.quit();
                    return;
                }
                cx.on_window_closed(|cx, _| cx.quit()).detach();
                cx.activate(true);
            });
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
    origin: Instant,
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
        Self {
            origin: clock.now(),
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
        }
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
        self.clock
            .now()
            .saturating_duration_since(self.origin)
            .as_secs_f64()
            * 1000.0
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

/// A machine state's square dot.
fn dot(tone: Tone, theme: &Theme) -> Div {
    match tone {
        Tone::Stopped => ui::dot(7., theme.faint),
        Tone::Running | Tone::Paused | Tone::Busy | Tone::Error => ui::dot(7., ink(tone, theme)),
    }
}

/// The icon of an action's button.
const fn action_icon(action: Action) -> Icon {
    match action {
        Action::Start | Action::StartGolden | Action::Resume => Icon::Play,
        Action::FromTemplate => Icon::Plus,
        Action::Stop => Icon::Power,
        Action::ForceStop => Icon::CircleStop,
        Action::Pause => Icon::Pause,
        Action::Checkpoint => Icon::Bookmark,
        Action::RollBack => Icon::UndoTwo,
        Action::Restore => Icon::RotateCcw,
        Action::Fork => Icon::GitFork,
        Action::NewCopy => Icon::Copy,
        Action::Delete => Icon::TrashTwo,
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

/// The take-control button's width: its longest label, "Taking control…",
/// in 11 px Geist Mono (0.6 em a glyph), with its icon, gap and padding.
const CONTROL_WIDTH: f32 = 15. * 6.6 + 13. + 6. + 2. * 16. + 2.;

/// A cell of a panel's footer, its words in small mono upper case.
fn cell(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .px(px(12.))
        .border_r_1()
        .border_color(theme.line)
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

/// "1 agent", "2 agents".
fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Renders the parts of the window.
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
    fn facts(&self, machine: &Machine, narrow: bool, now: f64, theme: &Theme) -> Div {
        let mut left: Vec<Div> = Vec::new();
        let mut right: Vec<Div> = Vec::new();
        let live = machine.state.is_live();
        let report = self
            .panels
            .get(&machine.name)
            .and_then(|panel| panel.report());
        if let (true, Some(report)) = (live, report) {
            match &report.guest {
                Guest::Answering {
                    login,
                    front,
                    display,
                } => {
                    if !narrow {
                        left.push(cell(theme).child(match login {
                            Login::Locked => "SCREEN LOCKED",
                            Login::Active | Login::Unknown => "GUEST READY",
                        }));
                    }
                    if let Some(front) = front {
                        left.push(cell(theme).text_color(theme.text).child(front.clone()));
                    }
                    if let (Some(display), false) = (display, narrow) {
                        left.push(cell(theme).child(ui::upper(&display.words())));
                    }
                }
                Guest::Unreachable { .. } => left.push(
                    cell(theme)
                        .text_color(theme.warning)
                        .child("GUEST AGENT UNREACHABLE"),
                ),
                Guest::NotRunning | Guest::Unknown => {
                    left.push(cell(theme).child("GUEST AGENT NOT UP YET"));
                }
            }
        }
        if let (Some(report), false) = (report, narrow) {
            left.push(cell(theme).child(ui::upper(&count(
                report.checkpoints,
                "checkpoint",
                "checkpoints",
            ))));
        }
        if !narrow {
            match (machine.provenance(), machine.template()) {
                (Some(Provenance::Said(said)), None) => {
                    left.push(cell(theme).child(ui::upper(&said)));
                }
                (Some(Provenance::Unreadable), _) => left.push(
                    cell(theme)
                        .text_color(theme.warning)
                        .child("LINEAGE UNREADABLE"),
                ),
                (Some(Provenance::Said(_)), Some(_)) | (None, _) => {}
            }
        }
        if live {
            if self.drivers(&machine.name).next().is_none() {
                right.push(end_cell(theme).child("NO AGENTS"));
            }
            let starts = self
                .panels
                .get(&machine.name)
                .map_or(&[][..], |panel| panel.timeline())
                .iter()
                .map(|moment| moment.start_ms);
            right.push(
                end_cell(theme).child(format!("{} / MIN", report::calls_per_minute(starts, now))),
            );
        }
        div()
            .flex()
            .flex_none()
            .h(px(FACTS))
            .min_w_0()
            .overflow_hidden()
            .border_t_1()
            .border_color(theme.line)
            .bg(theme.raised)
            .font_family(MONO)
            .text_size(px(10.5))
            .text_color(theme.muted)
            .whitespace_nowrap()
            .children(left)
            .child(div().flex_1())
            .children(right)
    }

    /// One agent's card in a panel's rail: who it is and what it's doing.
    fn card(
        &self,
        session: &Session,
        live: Option<&str>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hue = Hue::of(session.id.0);
        let paused = session.activity == SessionActivity::Paused;
        let held = session.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .px(px(14.))
            .py(px(10.))
            .border_b_1()
            .border_color(theme.line)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .min_w_0()
                    .font_family(MONO)
                    .text_size(px(11.))
                    .font_weight(FontWeight::MEDIUM)
                    .child(
                        div()
                            .flex_none()
                            .size(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(hue.fill)
                            .text_color(rgb(0x0010_141b))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(session.initial().to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.text)
                            .child(ui::upper(session.who())),
                    )
                    .child(
                        div()
                            .text_color(theme.muted)
                            .child(session.answered.to_string()),
                    )
                    .child(
                        div()
                            .id(ElementId::NamedInteger("hold".into(), session.id.0))
                            .flex_none()
                            .size(px(22.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .border_1()
                            .border_color(theme.line)
                            .cursor_pointer()
                            .hover(|button| button.bg(theme.hover))
                            .child(
                                if paused { Icon::Play } else { Icon::Pause }.svg(12., theme.muted),
                            )
                            .on_click(cx.listener(move |root, _, _, cx| {
                                root.hold(
                                    &held,
                                    if paused { Hold::Resume } else { Hold::Pause },
                                    cx,
                                );
                            })),
                    ),
            )
            .child(
                div()
                    .mt(px(4.))
                    .font_family(MONO)
                    .text_size(px(10.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.muted)
                    .child("NOW"),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .truncate()
                    .text_color(if paused { theme.warning } else { theme.text })
                    .child(session.doing(live)),
            )
            .into_any_element()
    }

    /// A machine's panel, `width` wide: its head row, its screen with the
    /// rail beside it (under it, when narrow), and the tape of its calls.
    fn panel(
        &mut self,
        machine: &Machine,
        width: f32,
        frame_height: Option<f32>,
        now: f64,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let name = machine.name.clone();
        let busy = self.panels.get(&name).and_then(|panel| panel.action());
        let shown = self
            .panels
            .get(&name)
            .and_then(|panel| panel.screen().open())
            .map(|shown| shown.view.clone());
        let driving = self.driving(&name, cx);
        let live = machine.state.is_live();
        let running = machine.state == MachineState::Live(Run::Running);
        let paused = machine.state == MachineState::Live(Run::Paused);
        let overlay_on = self
            .panels
            .get(&name)
            .is_none_or(|panel| panel.overlay() == Overlay::Shown);
        let allowed = machine.actions();
        let beside = width >= NARROW;
        let lens = self
            .panels
            .get(&name)
            .map_or(LensKind::Screen, |panel| panel.handles().lens.kind());
        let moments = self
            .panels
            .get(&name)
            .map(|panel| panel.timeline().to_vec())
            .unwrap_or_default();
        // What the elements tree shows: the agent's last snapshot, or the
        // person's own look, whichever is newer.
        let panel = ElementsShown::of(
            &moments,
            self.panels.get(&name).and_then(|panel| panel.look()),
            now,
        );
        let scene = timeline::scene_at(&moments, now);
        // The call each agent is on, as the activity feed shows it, and since when.
        let on_call = |session: &Session| {
            scene
                .of(session.id.0)
                .and_then(|agent| agent.current.as_ref())
                .map(|(call, since)| (call.describe(), *since))
        };
        if let Some(Lens::Elements(inspect)) = self
            .panels
            .get_mut(&name)
            .map(|kept| &mut kept.handles_mut().lens)
        {
            inspect.follow(panel.snapshot.as_deref().ok());
        }
        let act: Act = {
            let pointed = name.clone();
            let listener = cx.listener(move |root, pointing: &Pointing, _, cx| {
                root.inspected(&pointed, pointing.clone(), cx);
            });
            std::rc::Rc::new(move |pointing, window, cx| listener(&pointing, window, cx))
        };

        // The head row, in the same order in every panel: its name, its
        // role, its state, the lens, the overlay; then the actions, the
        // menu, and control.
        let (state_word, tone) = machine.state.look();
        let mut head = div()
            .relative()
            .flex()
            .flex_none()
            .h(px(HEAD))
            .min_w_0()
            .border_b_1()
            .border_color(theme.line)
            .bg(theme.raised)
            .child(
                div()
                    .flex()
                    .flex_shrink(1.)
                    .min_w(px(60.))
                    .max_w(px(width * 0.3))
                    .items_center()
                    .gap(px(8.))
                    .px(px(12.))
                    .border_r_1()
                    .border_color(theme.line)
                    .font_family(MONO)
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(div().min_w_0().truncate().child(name.to_string()))
                    // A role, not a warning: neutral.
                    .when(machine.template().is_some(), |cell| {
                        cell.child(
                            div()
                                .flex_none()
                                .px(px(5.))
                                .py(px(2.))
                                .border_1()
                                .border_color(theme.line_strong)
                                .text_size(px(9.5))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.muted)
                                .child("TEMPLATE"),
                        )
                    }),
            );
        let state = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .px(px(12.))
            .border_r_1()
            .border_color(theme.line)
            .font_family(MONO)
            .text_size(px(11.))
            .font_weight(FontWeight::MEDIUM)
            .whitespace_nowrap();
        head = match busy {
            Some(action) => head.child(
                state
                    .text_color(theme.accent)
                    .child(ui::spinner(id("panel-busy", &name), 11., theme.accent))
                    .child(ui::upper(action.under_way())),
            ),
            None => head.child(
                state
                    .text_color(ink(tone, theme))
                    .child(ui::dot(7., ink(tone, theme)))
                    .child(ui::upper(state_word)),
            ),
        };
        if live {
            head = head.child(
                div()
                    .flex()
                    .flex_none()
                    .h_full()
                    .border_r_1()
                    .border_color(theme.line)
                    .children(LensKind::ALL.into_iter().enumerate().map(|(index, kind)| {
                        let toggled = name.clone();
                        segment(
                            id(&format!("lens-{}", kind.label()), &name),
                            None,
                            Some(kind.label()),
                            if kind == lens {
                                Segment::On
                            } else {
                                Segment::Plain
                            },
                            theme,
                        )
                        .when(!beside, |segment| segment.px(px(8.)))
                        .when(index + 1 < LensKind::ALL.len(), |segment| {
                            segment.border_r_1().border_color(theme.line)
                        })
                        .on_click(cx.listener(move |root, _, _, cx| {
                            let same = root
                                .panels
                                .get(&toggled)
                                .map_or(LensKind::Screen, |panel| panel.handles().lens.kind())
                                == kind;
                            if !same {
                                root.panels.of(&toggled).handles_mut().lens = kind.lens();
                            }
                            cx.notify();
                        }))
                    })),
            );
            let toggled = name.clone();
            head = head.child(
                segment(
                    id("overlay", &name),
                    Some(if overlay_on { Icon::Eye } else { Icon::EyeOff }),
                    None,
                    if overlay_on {
                        Segment::Plain
                    } else {
                        Segment::Faint
                    },
                    theme,
                )
                .border_r_1()
                .border_color(theme.line)
                .on_click(cx.listener(move |root, _, _, cx| {
                    root.panels.of(&toggled).toggle_overlay();
                    cx.notify();
                })),
            );
        }
        head = head.child(div().flex_1().min_w_0());
        // Labelled actions. A machine with no screen shows its main action
        // in the middle of the panel instead, so it's offered once.
        let in_head = |action: &Action| {
            action.shown()
                && (live
                    || match action {
                        Action::Start | Action::FromTemplate => false,
                        Action::StartGolden
                        | Action::Stop
                        | Action::ForceStop
                        | Action::Pause
                        | Action::Resume
                        | Action::Checkpoint
                        | Action::RollBack
                        | Action::Restore
                        | Action::Fork
                        | Action::NewCopy
                        | Action::Delete => true,
                    })
        };
        for action in allowed.iter().copied().filter(in_head) {
            let picked = name.clone();
            let tool = segment(
                id(action.label(), &name),
                Some(action_icon(action)),
                beside.then(|| action.label()),
                Segment::Plain,
                theme,
            )
            .border_l_1()
            .border_color(theme.line);
            head =
                head.child(match busy {
                    None => tool.on_click(cx.listener(move |root, _, window, cx| {
                        root.pick(&picked, action, window, cx)
                    })),
                    Some(_) => tool.opacity(0.45).cursor_default(),
                });
        }
        let in_menu: Vec<MenuItem> = std::iter::once(MenuItem::Recordings)
            .chain(
                allowed
                    .iter()
                    .copied()
                    .filter(|action| !action.shown())
                    .map(MenuItem::Act),
            )
            .collect();
        head = head.child(self.menu_button(&name, in_menu, HEAD, theme, cx));
        if running {
            let steering = shown
                .as_ref()
                .and_then(|view| view.read(cx).steering().cloned());
            let label = steering.as_ref().map_or("Take control", Steering::button);
            let view = shown.clone();
            let control = segment(
                id("steer", &name),
                Some(if driving {
                    Icon::Hand
                } else {
                    Icon::MousePointer2
                }),
                Some(label),
                if driving {
                    Segment::SignalLine
                } else {
                    Segment::Signal
                },
                theme,
            )
            // One width for every label, so the head never shifts as
            // control changes hands; a border in both, for the same reason.
            .w(px(CONTROL_WIDTH))
            .border_1()
            .border_color(theme.signal);
            head = head.child(match view {
                Some(view) => control
                    .on_click(cx.listener(move |_, _, _, cx| {
                        view.update(cx, |view, cx| view.press(cx));
                    }))
                    .into_any_element(),
                None => control.opacity(0.45).cursor_default().into_any_element(),
            });
        }

        // The frame: the screen, or why there isn't one.
        let frame = div()
            .relative()
            .bg(theme.screen)
            .overflow_hidden()
            .map(|frame| match frame_height {
                Some(height) => frame.h(px(height)).flex_none(),
                None => frame.flex_1().min_h_0(),
            })
            .min_w_0()
            .when(beside, |frame| frame.flex_1());
        let empty = |icon: Icon, words: String| {
            div()
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(10.))
                .p(px(16.))
                .text_color(theme.faint)
                .child(icon.svg(28., theme.faint))
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(theme.muted)
                        .child(words),
                )
        };
        let frame = match &machine.state {
            MachineState::Live(_) => {
                // The screen and what's drawn over it sit under the rim,
                // with as much room under them, so the picture is centred.
                let outer = frame;
                let frame = div()
                    .absolute()
                    .top(px(RIM))
                    .left_0()
                    .right_0()
                    .bottom(px(RIM));
                let mut frame = match &shown {
                    Some(view) => frame.child(div().absolute().inset_0().child(view.clone())),
                    None => frame.child(
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
                                    .child(
                                        self.panels
                                            .get(&name)
                                            .and_then(|panel| panel.screen().failure())
                                            .map_or_else(|| "connecting…".into(), str::to_owned),
                                    ),
                            ),
                    ),
                };
                let picture = shown.as_ref().and_then(|view| view.read(cx).picture());
                if let (Some(inspect), Some(picture), Ok(snapshot)) =
                    (self.inspecting(&name, live), picture, &panel.snapshot)
                {
                    frame = frame.child(inspect::boxes(
                        snapshot,
                        &panel.named,
                        inspect,
                        picture,
                        panel.stale,
                        !driving,
                        &act,
                    ));
                }
                if let (LensKind::Text, Some(picture)) = (lens, picture) {
                    let read_now: reading::ReadNow = {
                        let asked = name.clone();
                        let listener = cx.listener(move |root, (): &(), _, cx| {
                            root.read_now(&asked, cx);
                        });
                        std::rc::Rc::new(move |window, cx| listener(&(), window, cx))
                    };
                    let text_look = self.panels.get(&name).and_then(|panel| panel.text_look());
                    let shown = timeline::reading(&moments, text_look, now);
                    let looking = match text_look.map(|look| &look.outcome) {
                        Some(timeline::TextLooked::Loading) => Looking::Busy,
                        Some(timeline::TextLooked::Read(_) | timeline::TextLooked::Failed(_))
                        | None => Looking::Ready,
                    };
                    let (drawn, whose, stale) = match &shown {
                        timeline::Reading::Agent(read) => (
                            reading::Drawn::Read(&read.read),
                            SharedString::from(format!("agent · {}", ago(now - read.since_ms))),
                            read.acted_since,
                        ),
                        timeline::Reading::Person { look, acted_since } => (
                            match &look.outcome {
                                timeline::TextLooked::Loading => reading::Drawn::Loading,
                                timeline::TextLooked::Read(read) => reading::Drawn::Read(read),
                                timeline::TextLooked::Failed(why) => reading::Drawn::Failed(why),
                            },
                            SharedString::from(format!("you · {}", ago(now - look.at_ms))),
                            *acted_since,
                        ),
                        timeline::Reading::Nothing => {
                            (reading::Drawn::Nothing, SharedString::default(), false)
                        }
                    };
                    frame = frame.child(reading::layer(
                        &format!("text-{name}"),
                        drawn,
                        picture,
                        whose,
                        stale,
                        Some(looking),
                        theme,
                        &read_now,
                    ));
                }
                if let (true, Some(picture)) = (overlay_on && !moments.is_empty(), picture) {
                    frame = frame.child(overlay::layer(&moments, picture, now));
                }
                if paused {
                    let resumed = name.clone();
                    frame = frame.child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(theme.screen.opacity(0.55))
                            .child(
                                Button::new(
                                    id("veil-resume", &name),
                                    theme,
                                    Look::Primary,
                                    Some(Icon::Play),
                                    Some("Resume".into()),
                                )
                                .enabled(Enabled::when(busy.is_none()))
                                .on_press(cx.listener(
                                    move |root, _, window, cx| {
                                        root.pick(&resumed, Action::Resume, window, cx);
                                    },
                                )),
                            ),
                    );
                }
                // Who has the machine: a rim round the screen and a tab
                // above it, orange for you, an agent's hue while it acts.
                // The agent whose call started last.
                let acting = self
                    .drivers(&name)
                    .filter_map(|driver| {
                        let live = on_call(driver);
                        let since = live.as_ref().map_or(f64::NEG_INFINITY, |(_, since)| *since);
                        driver
                            .working(live.as_ref().map(|(call, _)| call.as_str()))
                            .map(|call| {
                                (since, driver.id.0, driver.who().to_owned(), call.to_owned())
                            })
                    })
                    .max_by(|a, b| a.0.total_cmp(&b.0))
                    .map(|(_, session, who, call)| (session, who, call));
                let (rim, tab) = if driving {
                    let since = self.panels.of(&name).controlled_since(now);
                    let seconds = ((now - since) / 1000.0).max(0.0) as u64;
                    (
                        Some(theme.signal),
                        Some((
                            theme.signal,
                            theme.on_signal,
                            Some(Icon::MousePointer2),
                            format!("YOU {:02}:{:02}", seconds / 60, seconds % 60),
                        )),
                    )
                } else {
                    self.panels.of(&name).released();
                    match &acting {
                        Some((session, who, call)) => {
                            let hue = Hue::of(*session);
                            (
                                Some(hue.fill),
                                Some((
                                    hue.fill,
                                    rgb(0x0010_141b).into(),
                                    None,
                                    format!("{} · {call}", ui::upper(who)),
                                )),
                            )
                        }
                        None => (None, None),
                    }
                };
                // The rim hugs the picture, not the bars beside it, and the
                // tab sits on its top left corner; the whole frame until the
                // picture's place is known.
                // The rim hugs the picture, not the bars beside it, and the
                // tab sits on its top left corner; the whole frame until the
                // picture's place is known.
                outer
                    .child(frame)
                    .children(rim.map(|rim| {
                        let ring = div().absolute().border_2().border_color(rim);
                        match picture {
                            Some(picture) => ring
                                .top(px(RIM + picture.top))
                                .left(px(picture.left))
                                .w(px(picture.width))
                                .h(px(picture.height)),
                            None => ring.top(px(RIM)).left_0().right_0().bottom(px(RIM)),
                        }
                    }))
                    .children(tab.map(|(fill, ink, icon, words)| {
                        div()
                            .absolute()
                            .top(px(picture.map_or(0., |picture| picture.top)))
                            .left(px(picture.map_or(0., |picture| picture.left)))
                            .h(px(RIM))
                            .max_w(
                                picture.map_or(gpui::Length::from(relative(0.8)), |picture| {
                                    gpui::Length::from(px(picture.width * 0.8))
                                }),
                            )
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .px(px(10.))
                            .bg(fill)
                            .text_color(ink)
                            .font_family(MONO)
                            .text_size(px(10.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .children(icon.map(|icon| icon.svg(11., ink)))
                            .child(div().truncate().child(words))
                    }))
            }
            MachineState::Unreadable(why) => frame.child(empty(
                Icon::TriangleAlert,
                format!("Its status can't be read: {why}"),
            )),
            MachineState::Stopped => frame.child(self.stopped_card(machine, busy, theme, cx)),
        };

        // The rail: the agents driving it, and in the elements lens, the
        // elements tree under them.
        let drivers: Vec<Session> = self.drivers(&name).cloned().collect();
        let cards: Vec<AnyElement> = drivers
            .iter()
            .map(|driver| {
                let live = on_call(driver);
                self.card(
                    driver,
                    live.as_ref().map(|(call, _)| call.as_str()),
                    theme,
                    cx,
                )
            })
            .collect();
        let tree = self.inspecting(&name, live).map(|inspect| {
            div()
                .flex()
                .flex_1()
                .min_h(px(if frame_height.is_some() { 280. } else { 0. }))
                .child(inspect::tree(
                    &format!("tree-{name}"),
                    panel.snapshot.as_ref().map_err(Clone::clone),
                    &panel.named,
                    inspect,
                    panel.age.clone(),
                    panel.stale,
                    Some(panel.looking),
                    None,
                    theme,
                    &act,
                ))
        });
        let agents = div()
            .id(id("agents", &name))
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .map(|agents| {
                if tree.is_some() {
                    agents.flex_none().max_h(relative(0.5))
                } else {
                    agents.flex_1().min_h_0()
                }
            })
            .map(|agents| {
                if cards.is_empty() {
                    agents.child(
                        div()
                            .px(px(14.))
                            .py(px(12.))
                            .text_size(px(12.))
                            .text_color(theme.muted)
                            .child("No agent has used this machine yet."),
                    )
                } else {
                    agents.children(cards)
                }
            });
        // The rail is there only with something in it: agents, or the tree.
        let railed = live && (!drivers.is_empty() || tree.is_some());
        let rail = railed.then(|| {
            div()
                .flex()
                .flex_col()
                .min_h_0()
                .overflow_hidden()
                .border_color(theme.line)
                .bg(theme.raised)
                .map(|rail| {
                    if beside {
                        rail.flex_none().w(px(RAIL)).border_l_1()
                    } else {
                        rail.border_t_1().when(frame_height.is_none(), |rail| {
                            rail.flex_none().max_h(relative(0.45))
                        })
                    }
                })
                .child(
                    div()
                        .flex_none()
                        .px(px(14.))
                        .py(px(9.))
                        .border_b_1()
                        .border_color(theme.line)
                        .font_family(MONO)
                        .text_size(px(10.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.muted)
                        .child(format!("AGENTS · {}", drivers.len())),
                )
                .when(!drivers.is_empty(), |rail| rail.child(agents))
                .children(tree)
        });
        let body = div()
            .flex()
            .min_h_0()
            .min_w_0()
            .map(|body| {
                if beside {
                    body.flex_row()
                } else {
                    body.flex_col()
                }
            })
            .map(|body| match frame_height {
                Some(_) => body.flex_none(),
                None => body.flex_1(),
            })
            .child(frame)
            .children(rail);

        // The tape: the last minute of calls. Always there for a live
        // machine, so the screen never jumps when the first call comes.
        let quiet = moments.is_empty();
        let tape = live.then(|| {
            div()
                .flex_none()
                .pt(px(6.))
                .px(px(12.))
                .pb(px(2.))
                .relative()
                .border_t_1()
                .border_color(theme.line)
                .when(quiet, |tape| {
                    tape.child(
                        div()
                            .absolute()
                            .left(px(12.))
                            .top(px(8.))
                            .font_family(MONO)
                            .text_size(px(10.5))
                            .text_color(theme.faint)
                            .child("NO CALLS YET"),
                    )
                })
                .child(overlay::strip(
                    id("timeline", &name),
                    &moments,
                    overlay::Span {
                        from: now - WINDOW_MS,
                        to: now,
                        now,
                        at: None,
                    },
                    move |ms| {
                        let seconds = ((ms - now) / 1000.0).round();
                        if seconds == 0.0 {
                            "now".to_owned()
                        } else {
                            format!("{seconds} s")
                        }
                    },
                    theme,
                ))
        });

        let facts = self.facts(machine, width < FACTS_NARROW, now, theme);
        div()
            .id(id("panel", &name))
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .bg(theme.surface)
            .child(head)
            .child(body)
            .children(tape)
            .child(facts)
            .into_any_element()
    }

    /// What a stopped machine's panel shows instead of a screen: what it
    /// is, and its one main action. A golden image's makes a machine from
    /// it; starting it is in its menu, and asks first.
    fn stopped_card(
        &self,
        machine: &Machine,
        busy: Option<Action>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let name = machine.name.clone();
        let dim = theme.on_slab.opacity(0.6);
        let checkpoints = self
            .panels
            .get(&name)
            .and_then(|panel| panel.report())
            .map_or_else(|| "…".to_owned(), |report| report.checkpoints.to_string());
        let fact = |term: &str, value: AnyElement| {
            div()
                .flex()
                .gap(px(16.))
                .child(
                    div()
                        .w(px(110.))
                        .flex_none()
                        .text_size(px(10.5))
                        .text_color(dim)
                        .child(ui::upper(term)),
                )
                .child(div().flex_1().min_w_0().child(value))
        };
        let (title, said, action, second) = match machine.template() {
            Some(template) => {
                let forks = forks_of(&self.machines, &name);
                let made = if forks.is_empty() {
                    div().child("None yet").into_any_element()
                } else {
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(8.))
                        .children(forks.into_iter().map(|fork| {
                            let shown = fork.clone();
                            div()
                                .id(id("fork", &fork))
                                .underline()
                                .cursor_pointer()
                                .child(fork.to_string())
                                .on_click(cx.listener(move |root, _, _, cx| {
                                    if let Some(at) = root.stage.slot_of(&shown, &root.machines) {
                                        root.show_slot(at, cx);
                                    }
                                }))
                        }))
                        .into_any_element()
                };
                (
                    "TEMPLATE",
                    Some(format!(
                        "New machines start from its checkpoint {template}. Starting it lets it drift from that, so that's in the menu and asks first."
                    )),
                    Action::FromTemplate,
                    fact("Made from it", made),
                )
            }
            None => {
                let origin = match machine.provenance() {
                    Some(Provenance::Said(said)) => said,
                    Some(Provenance::Unreadable) => "Unreadable".to_owned(),
                    None => "Not recorded".to_owned(),
                };
                (
                    "STOPPED",
                    None,
                    Action::Start,
                    fact("Origin", div().child(origin).into_any_element()),
                )
            }
        };
        let started = name.clone();
        div().absolute().inset_0().flex().justify_center().child(
            div()
                .flex()
                .flex_col()
                .justify_center()
                .gap(px(14.))
                .w(px(420.))
                .max_w_full()
                .p(px(24.))
                .text_color(theme.on_slab)
                .child(
                    div()
                        .font_family(MONO)
                        .text_size(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(dim)
                        .child(title),
                )
                .children(said.map(|said| div().text_size(px(13.)).child(said)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .font_family(MONO)
                        .text_size(px(12.))
                        .child(fact(
                            "Checkpoints",
                            div().child(checkpoints).into_any_element(),
                        ))
                        .child(second),
                )
                .child(
                    div().flex().child(
                        Button::new(
                            id("empty-start", &name),
                            theme,
                            Look::Primary,
                            Some(action_icon(action)),
                            Some(action.label().into()),
                        )
                        .enabled(Enabled::when(busy.is_none()))
                        .on_press(cx.listener(
                            move |root, _, window, cx| {
                                root.pick(&started, action, window, cx);
                            },
                        )),
                    ),
                ),
        )
    }

    /// A machine's ⋯ button and, while open, its menu under it: `height`
    /// is the row it sits in.
    fn menu_button(
        &self,
        name: &MachineName,
        in_menu: Vec<MenuItem>,
        height: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let toggled = name.clone();
        let mut menu = div().relative().flex().flex_none().h_full().child(
            segment(
                id("menu", name),
                Some(Icon::Ellipsis),
                None,
                Segment::Plain,
                theme,
            )
            .border_l_1()
            .border_color(theme.line)
            .on_click(cx.listener(move |root, _, _, cx| {
                // A press on the button that closed its menu leaves it closed.
                let just_closed = root
                    .menu_closed
                    .take()
                    .is_some_and(|(name, at)| name == toggled && root.now() - at < 500.0);
                root.menu = if just_closed || root.menu.as_ref() == Some(&toggled) {
                    None
                } else {
                    Some(toggled.clone())
                };
                cx.notify();
            })),
        );
        if self.menu.as_ref() == Some(name) {
            let items = in_menu.into_iter().map(|item| {
                let picked = name.clone();
                let (label, icon, danger) = match item {
                    MenuItem::Recordings => ("Recordings", Icon::Film, false),
                    MenuItem::Act(action) => (action.label(), action_icon(action), action.danger()),
                };
                let ink = if danger { theme.danger } else { theme.text };
                div()
                    .id(id(&format!("item-{label}"), name))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(34.))
                    .px(px(12.))
                    .border_b_1()
                    .border_color(theme.line)
                    .font_family(MONO)
                    .text_size(px(11.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(ink)
                    .cursor_pointer()
                    .hover(|item| item.bg(theme.hover))
                    .child(icon.svg(14., ink))
                    .child(ui::upper(label))
                    .on_click(cx.listener(move |root, _, window, cx| match item {
                        MenuItem::Recordings => root.recordings_view(Some(picked.clone()), cx),
                        MenuItem::Act(action) => root.pick(&picked, action, window, cx),
                    }))
            });
            menu = menu.child(deferred(
                div()
                    .id(id("menu-list", name))
                    .occlude()
                    .absolute()
                    .top(px(height - 1.))
                    .right_0()
                    .min_w(px(220.))
                    .flex()
                    .flex_col()
                    .border_1()
                    .border_color(theme.line_strong)
                    .bg(theme.overlay)
                    .on_mouse_down_out(cx.listener(|root, _, _, cx| {
                        let now = root.now();
                        root.menu_closed = root.menu.take().map(|name| (name, now));
                        cx.notify();
                    }))
                    .children(items),
            ));
        }
        menu
    }

    /// What a free slot (`index`) can start: the machines not running,
    /// each with its main action (Start, or New machine from a template)
    /// and its menu, then New machine.
    fn launcher(&self, index: usize, theme: &Theme, cx: &mut Context<Self>) -> Div {
        const ROW: f32 = 40.0;
        let heading = |text: &str, n: usize| {
            div()
                .flex()
                .gap(px(8.))
                .mt(px(16.))
                .mb(px(6.))
                .font_family(MONO)
                .text_size(px(10.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.muted)
                .child(ui::upper(text))
                .child(div().text_color(theme.faint).child(n.to_string()))
        };
        let mut list = div().w_full().flex().flex_col();
        for group in [Group::Stopped, Group::Golden] {
            let members: Vec<&Machine> = self
                .machines
                .iter()
                .filter(|machine| !machine.state.is_live() && machine.group() == group)
                .collect();
            if members.is_empty() {
                continue;
            }
            list = list.child(heading(group.title(), members.len()));
            let mut rows = div().border_t_1().border_color(theme.line);
            for machine in members {
                let name = machine.name.clone();
                let allowed = machine.actions();
                let main = [Action::FromTemplate, Action::Start]
                    .into_iter()
                    .find(|action| allowed.contains(action));
                let in_menu: Vec<MenuItem> = std::iter::once(MenuItem::Recordings)
                    .chain(
                        allowed
                            .iter()
                            .copied()
                            .filter(|action| Some(*action) != main)
                            .map(MenuItem::Act),
                    )
                    .collect();
                let busy = self.panels.get(&name).and_then(|panel| panel.action());
                let row = div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .h(px(ROW))
                    .pl(px(12.))
                    .border_b_1()
                    .border_color(theme.line)
                    .bg(theme.raised)
                    .child(match busy {
                        Some(_) => {
                            ui::spinner(id("bay-busy", &name), 10., theme.accent).into_any_element()
                        }
                        None => dot(machine.state.look().1, theme).into_any_element(),
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(MONO)
                            .text_size(px(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.strong)
                            .child(name.to_string()),
                    )
                    .children(machine.template().map(|template| {
                        div()
                            .px(px(5.))
                            .py(px(3.))
                            .border_1()
                            .border_color(theme.line_strong)
                            .font_family(MONO)
                            .text_size(px(10.))
                            .text_color(theme.muted)
                            .child(template.to_string())
                    }))
                    .child(div().flex_1())
                    .children(busy.map(|doing| {
                        div()
                            .pr(px(8.))
                            .font_family(MONO)
                            .text_size(px(11.))
                            .text_color(theme.muted)
                            .child(doing.under_way())
                    }))
                    .children(main.filter(|_| busy.is_none()).map(|action| {
                        let started = name.clone();
                        div()
                            .id(id("bay-go", &name))
                            .flex_none()
                            .size(px(ROW))
                            .flex()
                            .items_center()
                            .justify_center()
                            .border_l_1()
                            .border_color(theme.line)
                            .cursor_pointer()
                            .hover(|button| button.bg(theme.hover))
                            .child(action_icon(action).svg(13., theme.live))
                            .on_click(cx.listener(move |root, _, window, cx| {
                                if action == Action::Start {
                                    root.start_in(index, &started, window, cx);
                                } else {
                                    root.pick(&started, action, window, cx);
                                }
                            }))
                    }))
                    .child(self.menu_button(&name, in_menu, ROW, theme, cx));
                rows = rows.child(row);
            }
            list = list.child(rows);
        }
        list = list.child(
            div()
                .id(ElementId::NamedInteger("bay-new".into(), index as u64))
                .mt(px(14.))
                .h(px(34.))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .border_1()
                .border_color(theme.line_strong)
                .font_family(MONO)
                .text_size(px(11.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.text)
                .cursor_pointer()
                .hover(|button| button.bg(theme.hover))
                .child(Icon::Plus.svg(12., theme.text))
                .child("NEW MACHINE")
                .on_click(cx.listener(|root, _, window, cx| {
                    root.ask(
                        Question::Choose {
                            choice: Choice::NewCopy,
                            machine: None,
                            template: None,
                        },
                        window,
                        cx,
                    );
                })),
        );
        list
    }

    fn stage_view(
        &mut self,
        width: f32,
        narrow: bool,
        now: f64,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.machines.is_empty() {
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .p(px(24.))
                .text_color(theme.faint)
                .child(Icon::Monitor.svg(36., theme.faint))
                .child(
                    div()
                        .mt(px(8.))
                        .text_size(px(16.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child("No machines yet"),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(4.))
                        .text_color(theme.muted)
                        .child("Make one on the Mac with")
                        .child(
                            div()
                                .font_family(MONO)
                                .text_size(px(12.5))
                                .child("doppel create"),
                        )
                        .child("; it shows here as soon as it exists."),
                )
                .into_any_element();
        }
        // One machine fills the stage, or both side by side when split.
        self.stage.settle(&self.machines);
        let stage: Vec<Machine> = self
            .stage
            .shows(&self.machines)
            .into_iter()
            .cloned()
            .collect();
        if stage.is_empty() {
            // Nothing running: what can start, where the screen will be.
            return div()
                .id("idle")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .justify_center()
                .pt(relative(0.12))
                .child(div().w(px(420.)).child(self.launcher(0, theme, cx)))
                .into_any_element();
        }
        if narrow && stage.len() > 1 {
            // Stacked: each at the screen's shape.
            let frame_height = (width - 1.) * 10. / 16.;
            let panels: Vec<AnyElement> = stage
                .iter()
                .map(|machine| {
                    div()
                        .w_full()
                        .flex()
                        .border_b_1()
                        .border_color(theme.line)
                        .child(self.panel(machine, width, Some(frame_height), now, theme, cx))
                        .into_any_element()
                })
                .collect();
            return div()
                .id("stacked")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(panels)
                .into_any_element();
        }
        let each = width / stage.len() as f32;
        let panels: Vec<AnyElement> = stage
            .iter()
            .enumerate()
            .map(|(index, machine)| {
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .when(index > 0, |panel| {
                        panel.border_l_1().border_color(theme.line)
                    })
                    .child(self.panel(machine, each, None, now, theme, cx))
                    .into_any_element()
            })
            .collect();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .children(panels)
            .into_any_element()
    }

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

/// How long ago, in words.
fn ago(ms: f64) -> String {
    let seconds = (ms / 1000.0).max(0.0).round();
    if seconds < 60.0 {
        format!("{seconds} s ago")
    } else {
        format!("{} min ago", (seconds / 60.0).round())
    }
}

/// What a machine's elements panel shows, worked out from its timeline and
/// the person's latest look.
struct ElementsShown {
    snapshot: Result<Arc<doppel_machine::ax::tree::AxSnapshot>, Empty>,
    /// The agent's refs for it; none for a person's look.
    named: Named,
    age: SharedString,
    stale: bool,
    looking: Looking,
}

impl ElementsShown {
    fn of(moments: &[timeline::Moment], look: Option<&timeline::Look>, now: f64) -> Self {
        let looking = match look.map(|look| &look.outcome) {
            Some(timeline::Looked::Loading) => Looking::Busy,
            Some(timeline::Looked::Seen(_) | timeline::Looked::Failed(_)) | None => Looking::Ready,
        };
        match timeline::showing(moments, look, now) {
            timeline::Showing::Agent(seen) => {
                let (snapshot, named) = match &seen.seen {
                    timeline::Seen::Live(snapshot) => (
                        Ok(snapshot.clone()),
                        Named::of(snapshot, seen.refs.as_deref()),
                    ),
                    timeline::Seen::Recorded(_) => (Err(Empty::None), Named::default()),
                };
                Self {
                    snapshot,
                    named,
                    age: format!("agent · {}", ago(now - seen.since_ms)).into(),
                    stale: seen.acted_since,
                    looking,
                }
            }
            timeline::Showing::Person { look, acted_since } => Self {
                snapshot: match &look.outcome {
                    timeline::Looked::Loading => Err(Empty::Loading),
                    timeline::Looked::Seen(snapshot) => Ok(snapshot.clone()),
                    timeline::Looked::Failed(why) => Err(Empty::Failed(why.clone())),
                },
                named: Named::default(),
                age: format!("you · {}", ago(now - look.at_ms)).into(),
                stale: acted_since,
                looking,
            },
            timeline::Showing::Nothing => Self {
                snapshot: Err(Empty::None),
                named: Named::default(),
                age: SharedString::default(),
                stale: false,
                looking,
            },
        }
    }
}
