//! What a console keeps about each machine it shows: its screen's state, the
//! action a person started on it, the overlay, the person's own looks at
//! its elements and text, what agents did on it, when the person took
//! control, and its report. One [`Panel`] per machine, in [`Panels`].
//!
//! Generic over what the console holds for an open screen (`S`) and for
//! the rest of a panel (`H`: its toolkit's handles), so this module stays
//! free of any toolkit and its decisions can be tested with `()`.

use std::collections::BTreeMap;
use std::sync::Arc;

use doppel_desktop::ax::tree::AxSnapshot;

use doppel_machine::machine::MachineName;

use crate::model::Action;
use crate::report::Report;
use crate::timeline::{ActivityEvent, Look, TextLook, TextRead, Ticket, Timeline};

/// A machine's screen, as the console has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen<S> {
    Closed,
    /// Being opened; with why the last try failed, if one did, to show
    /// meanwhile.
    Opening(Option<String>),
    Open(S),
    /// The last try failed, and why. Tried again when next wanted.
    Failed(String),
}

impl<S> Screen<S> {
    /// The open screen, if it is open.
    pub const fn open(&self) -> Option<&S> {
        match self {
            Self::Open(screen) => Some(screen),
            Self::Closed | Self::Opening(_) | Self::Failed(_) => None,
        }
    }

    /// Why the screen couldn't be opened, while that is the latest word.
    pub fn failure(&self) -> Option<&str> {
        match self {
            Self::Failed(why) | Self::Opening(Some(why)) => Some(why),
            Self::Closed | Self::Opening(None) | Self::Open(_) => None,
        }
    }
}

/// Whether the agents' marks are drawn over a panel's screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Overlay {
    #[default]
    Shown,
    Hidden,
}

/// Everything a console keeps about one machine.
#[derive(Debug)]
pub struct Panel<S, H> {
    /// The action a person started, until its verdict.
    action: Option<Action>,
    screen: Screen<S>,
    overlay: Overlay,
    look: Option<Look>,
    text_look: Option<TextLook>,
    /// The ticket of the latest ask, look or read.
    asks: Ticket,
    /// What agents did on it since the console began watching.
    timeline: Timeline,
    /// When the person took control, for the timer on its frame.
    took: Option<f64>,
    report: Option<Report>,
    /// When it last heard of an agent's call: the latest mark it was
    /// recorded at, or none if it never heard of one.
    heard: Option<Heard>,
    handles: H,
}

impl<S, H: Default> Default for Panel<S, H> {
    fn default() -> Self {
        Self {
            action: None,
            screen: Screen::Closed,
            overlay: Overlay::default(),
            look: None,
            text_look: None,
            asks: Ticket::default(),
            timeline: Timeline::new(),
            took: None,
            report: None,
            heard: None,
            handles: H::default(),
        }
    }
}

impl<S, H> Panel<S, H> {
    /// The action under way, if one is.
    pub const fn action(&self) -> Option<Action> {
        self.action
    }

    /// Starts `action`, unless one is under way: then `false`, and nothing
    /// changes.
    #[must_use]
    pub fn begin(&mut self, action: Action) -> bool {
        if self.action.is_some() {
            return false;
        }
        self.action = Some(action);
        true
    }

    /// The action under way has its verdict.
    pub fn finish(&mut self) {
        self.action = None;
    }

    pub const fn screen(&self) -> &Screen<S> {
        &self.screen
    }

    /// The answer to opening the screen: open, or why not.
    pub fn opened(&mut self, opened: Result<S, String>) {
        self.screen = match opened {
            Ok(screen) => Screen::Open(screen),
            Err(why) => Screen::Failed(why),
        };
    }

    pub const fn overlay(&self) -> Overlay {
        self.overlay
    }

    pub fn toggle_overlay(&mut self) {
        self.overlay = match self.overlay {
            Overlay::Shown => Overlay::Hidden,
            Overlay::Hidden => Overlay::Shown,
        };
    }

    /// The person's latest look at the elements.
    pub const fn look(&self) -> Option<&Look> {
        self.look.as_ref()
    }

    /// A look asked at `at`, its answer to come: the ticket its answer
    /// is taken with.
    pub fn looking(&mut self, at: f64) -> Ticket {
        let ticket = self.next_ticket();
        self.look = Some(Look::new(ticket, at));
        ticket
    }

    /// The answer to the look with `ticket`. Only the latest look's answer
    /// counts: `false` for an earlier one's, which changes nothing.
    pub fn looked(&mut self, ticket: Ticket, outcome: Result<Arc<AxSnapshot>, String>) -> bool {
        self.look
            .as_mut()
            .is_some_and(|look| look.answer(ticket, outcome))
    }

    /// The person's latest read of the screen's text.
    pub const fn text_look(&self) -> Option<&TextLook> {
        self.text_look.as_ref()
    }

    /// A read asked at `at`, its answer to come: the ticket its answer is
    /// taken with.
    pub fn reading(&mut self, at: f64) -> Ticket {
        let ticket = self.next_ticket();
        self.text_look = Some(TextLook::new(ticket, at));
        ticket
    }

    /// The answer to the read with `ticket`. Only the latest read's answer
    /// counts: `false` for an earlier one's.
    pub fn read(&mut self, ticket: Ticket, outcome: Result<Arc<TextRead>, String>) -> bool {
        self.text_look
            .as_mut()
            .is_some_and(|look| look.answer(ticket, outcome))
    }

    fn next_ticket(&mut self) -> Ticket {
        self.asks = self.asks.next();
        self.asks
    }

    pub const fn timeline(&self) -> &Timeline {
        &self.timeline
    }

    /// When the person took control, the first time this is asked while
    /// they have it: `now` then.
    pub fn controlled_since(&mut self, now: f64) -> f64 {
        *self.took.get_or_insert(now)
    }

    /// The person no longer has control.
    pub fn released(&mut self) {
        self.took = None;
    }

    pub const fn report(&self) -> Option<&Report> {
        self.report.as_ref()
    }

    pub const fn handles(&self) -> &H {
        &self.handles
    }

    pub fn handles_mut(&mut self) -> &mut H {
        &mut self.handles
    }
}

/// How far the panels had heard of agents' calls, at some moment: later
/// marks compare greater. Taken when a listing of machines is asked for, so
/// the listing's answer can tell which panels it speaks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Heard(u64);

/// The console's panels, by machine. A machine has one once anything is
/// kept about it.
#[derive(Debug)]
pub struct Panels<S, H> {
    panels: BTreeMap<MachineName, Panel<S, H>>,
    /// Calls recorded so far, on any panel.
    heard: u64,
}

impl<S, H> Default for Panels<S, H> {
    fn default() -> Self {
        Self {
            panels: BTreeMap::new(),
            heard: 0,
        }
    }
}

impl<S, H: Default> Panels<S, H> {
    /// `name`'s panel, made empty if it had none.
    pub fn of(&mut self, name: &MachineName) -> &mut Panel<S, H> {
        self.panels.entry(name.clone()).or_default()
    }

    /// Records what an agent did, on its machine's timeline.
    pub fn record(&mut self, event: ActivityEvent, now: f64) {
        self.heard += 1;
        let heard = Heard(self.heard);
        let panel = self.of(event.machine());
        panel.timeline.record(event, now);
        panel.heard = Some(heard);
    }

    /// Opens the screens of `wanted`, and closes the rest and those that
    /// `ended`. Returns the screens to open now: they are opening.
    pub fn sync_screens(
        &mut self,
        wanted: &[MachineName],
        ended: impl Fn(&S) -> bool,
    ) -> Vec<MachineName> {
        for (name, panel) in &mut self.panels {
            let keep = wanted.contains(name);
            let close = match &panel.screen {
                Screen::Open(screen) => !keep || ended(screen),
                Screen::Failed(_) => !keep,
                // Its answer lands whatever happens meanwhile.
                Screen::Closed | Screen::Opening(_) => false,
            };
            if close {
                panel.screen = Screen::Closed;
            }
        }
        let mut open = Vec::new();
        for name in wanted {
            let panel = self.of(name);
            let failed = match &mut panel.screen {
                Screen::Open(_) | Screen::Opening(_) => continue,
                Screen::Closed => None,
                Screen::Failed(why) => Some(std::mem::take(why)),
            };
            panel.screen = Screen::Opening(failed);
            open.push(name.clone());
        }
        open
    }

    /// Keeps the report of `name` only while it is on `stage`: `false`
    /// when it isn't, and the report is dropped.
    pub fn reported(&mut self, name: &MachineName, report: Report, stage: &[MachineName]) -> bool {
        if !stage.contains(name) {
            return false;
        }
        self.of(name).report = Some(report);
        true
    }
}

impl<S, H> Panels<S, H> {
    pub fn get(&self, name: &MachineName) -> Option<&Panel<S, H>> {
        self.panels.get(name)
    }

    pub fn get_mut(&mut self, name: &MachineName) -> Option<&mut Panel<S, H>> {
        self.panels.get_mut(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&MachineName, &Panel<S, H>)> {
        self.panels.iter()
    }

    /// The open screens.
    pub fn screens(&self) -> impl Iterator<Item = &S> {
        self.panels.values().filter_map(|panel| panel.screen.open())
    }

    /// The daemon can't be reached: no screen stays open, and no action
    /// is under way any more.
    pub fn disconnected(&mut self) {
        for panel in self.panels.values_mut() {
            if panel.screen.open().is_some() {
                panel.screen = Screen::Closed;
            }
            panel.action = None;
        }
    }

    /// How far the panels have heard, now: mark a listing asked for now
    /// with it.
    pub const fn heard(&self) -> Heard {
        Heard(self.heard)
    }

    /// Forgets the machines a listing asked for at `asked` leaves out (a
    /// temporary machine deleted when its session ended), so a new machine
    /// given the same name starts with an empty timeline, not with the
    /// calls, and the agents' colours, of the one before it.
    ///
    /// A panel that heard of a call after `asked` is kept: its machine may
    /// be newer than the listing. One whose calls all came before was named
    /// by a machine that existed then and is gone now, even if no listing
    /// ever showed it (a console opened while it was being deleted hears of
    /// its calls from the daemon's history).
    pub fn keep_listed(&mut self, listed: &[MachineName], asked: Heard) {
        self.panels.retain(|name, panel| {
            listed.contains(name) || panel.heard.is_some_and(|heard| heard > asked)
        });
    }

    /// Drops the reports of machines no longer on `stage`.
    pub fn keep_reports_of(&mut self, stage: &[MachineName]) {
        for (name, panel) in &mut self.panels {
            if !stage.contains(name) {
                panel.report = None;
            }
        }
    }

    /// Whether every machine on `stage` has its report.
    pub fn reports_for(&self, stage: &[MachineName]) -> bool {
        stage
            .iter()
            .all(|name| self.get(name).is_some_and(|panel| panel.report.is_some()))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use doppel_machine::machine::MachineName;

    use super::{Overlay, Panels, Screen};
    use crate::model::Action;
    use crate::report::{Guest, Report};
    use doppel_machine::activity as told;

    use crate::timeline::{ActivityEvent, Answer, TextRead, Ticket};

    type Kept = Panels<&'static str, ()>;

    fn name(raw: &str) -> MachineName {
        raw.parse().unwrap()
    }

    fn report() -> Report {
        Report {
            guest: Guest::NotRunning,
            checkpoints: 0,
        }
    }

    #[test]
    fn one_action_at_a_time() {
        let mut panels = Kept::default();
        let panel = panels.of(&name("a"));
        assert!(panel.begin(Action::Stop));
        assert!(!panel.begin(Action::Start));
        assert_eq!(panel.action(), Some(Action::Stop));
        panel.finish();
        assert!(panel.begin(Action::Start));
    }

    #[test]
    fn only_the_latest_looks_answer_lands() {
        let mut panels = Kept::default();
        let panel = panels.of(&name("a"));
        assert!(!panel.looked(Ticket::default(), Err("early".into())));
        // Two asks in the same millisecond: only the second's answer lands.
        let first = panel.looking(1.);
        let second = panel.looking(1.);
        assert!(!panel.looked(first, Err("stale".into())));
        assert!(matches!(
            panel.look().map(|look| &look.outcome),
            Some(Answer::Loading)
        ));
        assert!(panel.looked(second, Err("no".into())));
        assert!(matches!(
            panel.look().map(|look| &look.outcome),
            Some(Answer::Failed(_))
        ));

        let first = panel.reading(3.);
        let second = panel.reading(4.);
        assert!(!panel.read(first, Err("stale".into())));
        let read = Arc::new(TextRead {
            lines: Vec::new(),
            query: None,
            found: Vec::new(),
        });
        assert!(panel.read(second, Ok(read)));
        assert!(matches!(
            panel.text_look().map(|look| &look.outcome),
            Some(Answer::Got(_))
        ));
    }

    #[test]
    fn wanted_screens_open_once_and_a_failure_shows_until_the_retry_answers() {
        let mut panels = Kept::default();
        let (a, b) = (name("a"), name("b"));
        assert_eq!(
            panels.sync_screens(&[a.clone(), b.clone()], |_| false),
            vec![a.clone(), b.clone()]
        );
        // Already opening: not asked again.
        assert!(
            panels
                .sync_screens(&[a.clone(), b.clone()], |_| false)
                .is_empty()
        );
        panels.of(&a).opened(Ok("screen"));
        panels.of(&b).opened(Err("refused".into()));
        assert_eq!(panels.screens().collect::<Vec<_>>(), [&"screen"]);
        // The failed one is tried again, its failure still shown.
        assert_eq!(
            panels.sync_screens(&[a.clone(), b.clone()], |_| false),
            vec![b.clone()]
        );
        assert_eq!(panels.of(&b).screen().failure(), Some("refused"));
        panels.of(&b).opened(Ok("other"));
        assert_eq!(panels.of(&b).screen().failure(), None);
        // An ended screen opens again; an unwanted one closes.
        assert_eq!(
            panels.sync_screens(std::slice::from_ref(&a), |shown| *shown == "screen"),
            vec![a.clone()]
        );
        assert_eq!(panels.of(&b).screen(), &Screen::Closed);
    }

    #[test]
    fn losing_the_daemon_closes_screens_and_ends_actions() {
        let mut panels = Kept::default();
        let a = name("a");
        let _ = panels.sync_screens(std::slice::from_ref(&a), |_| false);
        panels.of(&a).opened(Ok("screen"));
        assert!(panels.of(&a).begin(Action::Pause));
        panels.disconnected();
        assert_eq!(panels.of(&a).screen(), &Screen::Closed);
        assert_eq!(panels.of(&a).action(), None);
    }

    #[test]
    fn reports_are_kept_for_the_stage_only() {
        let mut panels = Kept::default();
        let (a, b) = (name("a"), name("b"));
        let stage = [a.clone(), b.clone()];
        assert!(!panels.reports_for(&stage));
        assert!(!panels.reported(&a, report(), std::slice::from_ref(&b)));
        assert!(panels.reported(&a, report(), &stage));
        assert!(panels.reported(&b, report(), &stage));
        assert!(panels.reports_for(&stage));
        panels.keep_reports_of(std::slice::from_ref(&b));
        assert!(panels.get(&a).is_some_and(|panel| panel.report().is_none()));
        assert!(panels.reports_for(&[b]));
    }

    #[test]
    fn control_time_counts_from_when_it_was_first_asked() {
        let mut panels = Kept::default();
        let panel = panels.of(&name("a"));
        assert_eq!(panel.controlled_since(5.), 5.);
        assert_eq!(panel.controlled_since(9.), 5.);
        panel.released();
        assert_eq!(panel.controlled_since(9.), 9.);
    }

    #[test]
    fn the_overlay_toggles() {
        let mut panels = Kept::default();
        let panel = panels.of(&name("a"));
        assert_eq!(panel.overlay(), Overlay::Shown);
        panel.toggle_overlay();
        assert_eq!(panel.overlay(), Overlay::Hidden);
        panel.toggle_overlay();
        assert_eq!(panel.overlay(), Overlay::Shown);
    }

    fn call_on(machine: &str) -> ActivityEvent {
        ActivityEvent::of(told::Activity {
            session: 1.into(),
            request: 1,
            machine: name(machine),
            client: "agent".into(),
            caption: String::new(),
            phase: told::Phase::Started(told::OnScreen {
                why: None,
                gesture: told::Gesture::Unknown(0),
                target: None,
                callout: None,
            }),
        })
    }

    #[test]
    fn a_machine_a_listing_leaves_out_is_forgotten_and_its_name_starts_afresh() {
        let mut panels = Kept::default();
        panels.of(&name("a")).toggle_overlay();
        panels.of(&name("b"));
        panels.keep_listed(&[name("b")], panels.heard());
        assert!(panels.get(&name("a")).is_none());
        assert!(panels.get(&name("b")).is_some());
        assert_eq!(panels.of(&name("a")).overlay, Overlay::default(), "afresh");
    }

    #[test]
    fn calls_heard_of_a_machine_no_listing_showed_are_forgotten_once_one_leaves_it_out() {
        let mut panels = Kept::default();
        // Told from the daemon's history as the console opened, while the
        // machine was being deleted: the first listing already leaves it out.
        panels.record(call_on("gone"), 0.0);
        let asked = panels.heard();
        panels.keep_listed(&[], asked);
        assert!(panels.get(&name("gone")).is_none());
    }

    #[test]
    fn a_machine_heard_of_after_the_listing_was_asked_for_is_kept() {
        let mut panels = Kept::default();
        let asked = panels.heard();
        panels.record(call_on("new"), 0.0);
        panels.keep_listed(&[], asked);
        assert_eq!(
            panels
                .get(&name("new"))
                .map(|panel| panel.timeline().moments().len()),
            Some(1)
        );
    }
}
