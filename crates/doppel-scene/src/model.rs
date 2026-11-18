//! What the window shows about machines and the agents driving them, and
//! what a person can do to them. Pure: the daemon's answers come in through
//! the [`Daemon`](crate::Daemon) port already parsed into these types; this
//! module says how they read and which actions each state allows.

use doppel_machine::bundle::CheckpointLabel;
use doppel_machine::lineage::{Origin, RecordedLineage, Role};
use doppel_machine::machine::MachineName;

/// A machine the daemon lists, where it is, and what it is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    pub name: MachineName,
    pub state: MachineState,
    pub lineage: RecordedLineage,
}

/// Which group of the machine switcher a machine is listed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Running,
    Stopped,
    /// Golden images, whatever their state: templates, not machines to use.
    Golden,
}

impl Group {
    pub const fn title(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Stopped => "Stopped",
            Self::Golden => "Golden images",
        }
    }
}

/// What the status bar says about a machine's lineage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provenance {
    /// `Template · ready`, or `From golden-27 · ready`.
    Said(String),
    /// Its lineage file couldn't be read.
    Unreadable,
}

/// Where a machine is: a live one has a screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineState {
    Stopped,
    Live(Run),
    /// Its status couldn't be read, and why.
    Unreadable(String),
}

/// How a live machine's VM is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Run {
    Starting,
    Running,
    Paused,
    /// Pausing, resuming, saving or restoring.
    Busy,
    Stopping,
    Error,
    /// A state the daemon reported that this build doesn't know.
    Unknown,
}

/// A session's id, as the daemon numbers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(pub u64);

/// One client driving machines: an agent through `doppel mcp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: SessionId,
    /// Who drives, in its own words; empty when it gave none.
    pub client: String,
    /// The machines its requests named, in first-use order.
    pub machines: Vec<MachineName>,
    /// Captions of the requests running or waiting now.
    pub running: Vec<String>,
    /// How many of its calls the daemon has answered.
    pub answered: u64,
    pub activity: SessionActivity,
}

/// Whether a session's calls run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionActivity {
    Active,
    /// Its next calls wait until it's resumed.
    Paused,
    /// A state a newer daemon added, as sent.
    Unknown(i32),
}

/// Holding a session's calls, or letting them go on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    Pause,
    Resume,
}

/// What a person can do to a machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Start,
    /// Start a golden image, once the person has heard it drifts from its
    /// template.
    StartGolden,
    /// A new machine from a golden image's template.
    FromTemplate,
    Stop,
    ForceStop,
    Pause,
    Resume,
    Checkpoint,
    RollBack,
    Restore,
    Fork,
    NewCopy,
    Delete,
}

/// What an action does, with what it needs: the request the daemon gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Order {
    Start,
    /// Asks the guest to shut down.
    Stop,
    /// Powers it off at once.
    ForceStop,
    Pause,
    Resume,
    /// Saves a checkpoint, then carries on.
    Checkpoint(CheckpointLabel),
    /// Goes back to a checkpoint while it runs.
    RollBack(CheckpointLabel),
    Restore(CheckpointLabel),
    /// A new stopped machine from a checkpoint, sharing its identity.
    Fork {
        label: CheckpointLabel,
        into: MachineName,
    },
    /// A new machine from a checkpoint under a new identity, started.
    NewCopy(CheckpointLabel),
    Delete,
}

/// How an action ended, for the person who asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Done {
        /// Anything worth saying about how it went.
        note: Option<String>,
        /// A machine it made, to show.
        made: Option<MachineName>,
    },
    Refused(String),
    /// It may or may not have happened: the person should look.
    Unknown(String),
}

/// The colour family a machine's state is shown in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Running,
    Paused,
    Busy,
    Stopped,
    Error,
}

impl Machine {
    /// The checkpoint new machines start from, when it's a golden image.
    pub fn template(&self) -> Option<&CheckpointLabel> {
        self.lineage.template()
    }

    pub fn group(&self) -> Group {
        match (self.template(), self.state.is_live()) {
            (Some(_), _) => Group::Golden,
            (None, true) => Group::Running,
            (None, false) => Group::Stopped,
        }
    }

    /// The actions it allows, in the order a menu shows them. A stopped
    /// golden image is for making new machines: that comes first, and
    /// starting it, which lets it drift from its template, asks first.
    pub fn actions(&self) -> &'static [Action] {
        match (&self.state, self.template()) {
            (MachineState::Stopped, Some(_)) => &[
                Action::FromTemplate,
                Action::Fork,
                Action::Restore,
                Action::StartGolden,
                Action::Delete,
            ],
            (MachineState::Stopped | MachineState::Live(_) | MachineState::Unreadable(_), _) => {
                self.state.actions()
            }
        }
    }

    /// Where it comes from, for its panel's footer; nothing for a working
    /// machine whose origin wasn't recorded or isn't a fork.
    pub fn provenance(&self) -> Option<Provenance> {
        let lineage = match &self.lineage {
            RecordedLineage::Known(lineage) => lineage,
            RecordedLineage::Unreadable(_) => return Some(Provenance::Unreadable),
        };
        match (&lineage.role, &lineage.origin) {
            (Role::Golden { template, .. }, _) => {
                Some(Provenance::Said(format!("Template · {template}")))
            }
            (Role::Temporary, Origin::Forked { from, checkpoint }) => Some(Provenance::Said(
                format!("Temporary · from {from} · {checkpoint}"),
            )),
            (Role::Temporary, Origin::Unrecorded | Origin::Installed | Origin::Imported) => {
                Some(Provenance::Said("Temporary".to_owned()))
            }
            (Role::Working, Origin::Forked { from, checkpoint }) => {
                Some(Provenance::Said(format!("From {from} · {checkpoint}")))
            }
            (Role::Working, Origin::Unrecorded | Origin::Installed | Origin::Imported) => None,
        }
    }
}

impl MachineState {
    #[must_use]
    pub const fn is_live(&self) -> bool {
        match self {
            Self::Live(_) => true,
            Self::Stopped | Self::Unreadable(_) => false,
        }
    }

    /// The actions this state allows, in the order a menu shows them. The
    /// daemon still refuses any that no longer fit by the time they arrive.
    pub fn actions(&self) -> &'static [Action] {
        match self {
            Self::Stopped => &[
                Action::Start,
                Action::NewCopy,
                Action::Fork,
                Action::Restore,
                Action::Delete,
            ],
            // A machine whose status can't be read can still be cleared away.
            Self::Unreadable(_) => &[Action::Delete],
            Self::Live(run) => match run {
                Run::Running => &[
                    Action::Pause,
                    Action::Stop,
                    Action::Checkpoint,
                    Action::RollBack,
                    Action::ForceStop,
                ],
                Run::Paused => &[Action::Resume, Action::Checkpoint, Action::ForceStop],
                Run::Starting | Run::Busy | Run::Stopping | Run::Error | Run::Unknown => {
                    &[Action::ForceStop]
                }
            },
        }
    }

    /// How it reads at a glance: a word and its colour.
    pub const fn look(&self) -> (&'static str, Tone) {
        match self {
            Self::Stopped => ("Stopped", Tone::Stopped),
            Self::Unreadable(_) => ("Unreadable", Tone::Error),
            Self::Live(run) => match run {
                Run::Running => ("Running", Tone::Running),
                Run::Paused => ("Paused", Tone::Paused),
                Run::Starting => ("Starting", Tone::Busy),
                Run::Busy => ("Busy", Tone::Busy),
                Run::Stopping => ("Stopping", Tone::Busy),
                Run::Error => ("Error", Tone::Error),
                Run::Unknown => ("Unknown", Tone::Error),
            },
        }
    }
}

impl Session {
    /// Who it is, as a sentence starts.
    pub fn who(&self) -> &str {
        if self.client.is_empty() {
            "An agent"
        } else {
            &self.client
        }
    }

    /// The call it's on, while it works: `live`, the call the activity feed
    /// shows it on, which sees every call as it starts; else the list of
    /// sessions, polled every few seconds, which misses quick ones.
    pub fn working<'a>(&'a self, live: Option<&'a str>) -> Option<&'a str> {
        match self.activity {
            SessionActivity::Active => live.or_else(|| self.running.first().map(String::as_str)),
            SessionActivity::Paused | SessionActivity::Unknown(_) => None,
        }
    }

    /// What it's doing now, in a few words: the call it's on, or why not.
    pub fn doing(&self, live: Option<&str>) -> String {
        match self.activity {
            SessionActivity::Active => self.working(live).unwrap_or("Idle").to_owned(),
            SessionActivity::Paused => "Held: its calls wait".to_owned(),
            SessionActivity::Unknown(raw) => format!("State {raw}"),
        }
    }

    /// The letter on its chip: its client's first, upper case.
    pub fn initial(&self) -> char {
        self.client
            .trim()
            .chars()
            .next()
            .map_or('#', |first| first.to_uppercase().next().unwrap_or(first))
    }
}

impl Action {
    /// Actions shown as buttons in a panel's header; the rest go in its menu.
    pub const fn shown(self) -> bool {
        match self {
            Self::Start | Self::FromTemplate | Self::Resume | Self::Pause | Self::Checkpoint => {
                true
            }
            Self::StartGolden
            | Self::Stop
            | Self::ForceStop
            | Self::RollBack
            | Self::Restore
            | Self::Fork
            | Self::NewCopy
            | Self::Delete => false,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Start => "Start",
            Self::StartGolden => "Start…",
            Self::FromTemplate => "New machine from it",
            Self::Stop => "Shut down",
            Self::ForceStop => "Force stop",
            Self::Pause => "Pause",
            Self::Resume => "Resume",
            Self::Checkpoint => "Save checkpoint",
            Self::RollBack => "Roll back…",
            Self::Restore => "Restore checkpoint…",
            Self::Fork => "Fork into…",
            Self::NewCopy => "New copy from checkpoint…",
            Self::Delete => "Delete…",
        }
    }

    /// It throws work away for good.
    pub const fn danger(self) -> bool {
        match self {
            Self::ForceStop | Self::RollBack | Self::Restore | Self::Delete => true,
            Self::Start
            | Self::StartGolden
            | Self::FromTemplate
            | Self::Stop
            | Self::Pause
            | Self::Resume
            | Self::Checkpoint
            | Self::Fork
            | Self::NewCopy => false,
        }
    }

    /// What a machine shows while it's under way.
    pub const fn under_way(self) -> &'static str {
        match self {
            Self::Start | Self::StartGolden => "Starting…",
            Self::FromTemplate => "Making a machine…",
            Self::Stop | Self::ForceStop => "Stopping…",
            Self::Pause => "Pausing…",
            Self::Resume => "Resuming…",
            Self::Checkpoint => "Saving a checkpoint…",
            Self::RollBack => "Rolling back…",
            Self::Restore => "Restoring…",
            Self::Fork => "Forking…",
            Self::NewCopy => "Making a copy…",
            Self::Delete => "Deleting…",
        }
    }

    /// Whether it ends what a person in control is doing, so control is
    /// handed back first.
    pub const fn ends_control(self) -> bool {
        match self {
            Self::Pause
            | Self::Resume
            | Self::Checkpoint
            | Self::Fork
            | Self::NewCopy
            | Self::FromTemplate => false,
            Self::Start
            | Self::StartGolden
            | Self::Stop
            | Self::ForceStop
            | Self::RollBack
            | Self::Restore
            | Self::Delete => true,
        }
    }
}

impl Order {
    pub const fn action(&self) -> Action {
        match self {
            Self::Start => Action::Start,
            Self::Stop => Action::Stop,
            Self::ForceStop => Action::ForceStop,
            Self::Pause => Action::Pause,
            Self::Resume => Action::Resume,
            Self::Checkpoint(_) => Action::Checkpoint,
            Self::RollBack(_) => Action::RollBack,
            Self::Restore(_) => Action::Restore,
            Self::Fork { .. } => Action::Fork,
            Self::NewCopy(_) => Action::NewCopy,
            Self::Delete => Action::Delete,
        }
    }

    /// What it did, once done.
    pub fn done(&self) -> String {
        match self {
            Self::Start => "started".into(),
            Self::Stop | Self::ForceStop => "stopped".into(),
            Self::Pause => "paused".into(),
            Self::Resume => "resumed".into(),
            Self::Checkpoint(label) => format!("saved checkpoint {label}"),
            Self::RollBack(label) => format!("rolled back to {label}"),
            Self::Restore(label) => format!("restored {label}"),
            Self::Fork { into, .. } => format!("forked into {into}"),
            Self::NewCopy(label) => format!("new machine started from {label}"),
            Self::Delete => "deleted".into(),
        }
    }
}

/// How many machines can run at once: Apple's limit on macOS machines,
/// which doppeld enforces. The header has a slot ("bay") for each.
pub(crate) const BAYS: usize = 2;

/// A bay of the stage: a live machine, or empty (by its position).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bay<'a> {
    Machine(&'a Machine),
    Empty(usize),
}

/// What each bay was given, by position: the machine in it, or the one
/// started there, which lands in it once it's live.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Bays {
    kept: [Option<MachineName>; BAYS],
}

impl Bays {
    /// Bays given `names` in order (`doppel view a b`); past the bays, none.
    pub fn of(names: impl IntoIterator<Item = MachineName>) -> Self {
        let mut kept: [Option<MachineName>; BAYS] = Default::default();
        for (slot, name) in kept.iter_mut().zip(names) {
            *slot = Some(name);
        }
        Self { kept }
    }

    /// The bays shown. A live machine keeps the bay it had; one newly live
    /// takes the bay it was started from, else the first empty one. A bay
    /// whose machine stopped is empty again.
    pub fn shown<'a>(&self, machines: &'a [Machine]) -> Vec<Bay<'a>> {
        let live = |name: &Option<MachineName>| {
            machines
                .iter()
                .find(|machine| Some(&machine.name) == name.as_ref() && machine.state.is_live())
        };
        let mut placed: Vec<Option<&'a Machine>> = self.kept.iter().map(live).collect();
        for machine in machines.iter().filter(|machine| machine.state.is_live()) {
            if placed
                .iter()
                .flatten()
                .any(|held| held.name == machine.name)
            {
                continue;
            }
            let own = (0..BAYS).find(|&index| {
                placed.get(index).is_some_and(Option::is_none)
                    && self.kept.get(index).and_then(Option::as_ref) == Some(&machine.name)
            });
            if let Some(free) = own.or_else(|| placed.iter().position(Option::is_none))
                && let Some(slot) = placed.get_mut(free)
            {
                *slot = Some(machine);
            }
        }
        placed
            .into_iter()
            .enumerate()
            .map(|(index, held)| held.map_or(Bay::Empty(index), Bay::Machine))
            .collect()
    }

    /// What to keep after `shown`: each bay's machine, or what was started
    /// there.
    #[must_use]
    pub(crate) fn kept_after(&self, shown: &[Bay<'_>]) -> Self {
        let mut kept = self.kept.clone();
        for (slot, bay) in kept.iter_mut().zip(shown) {
            match bay {
                Bay::Machine(machine) => *slot = Some(machine.name.clone()),
                Bay::Empty(_) => {}
            }
        }
        Self { kept }
    }

    /// With `name` started from bay `index`: it lands there once live.
    #[must_use]
    pub fn start_in(&self, index: usize, name: MachineName) -> Self {
        let mut kept = self.kept.clone();
        if let Some(slot) = kept.get_mut(index) {
            *slot = Some(name);
        }
        Self { kept }
    }

    /// The machines in `shown`'s bays.
    pub fn names(shown: &[Bay<'_>]) -> Vec<MachineName> {
        shown
            .iter()
            .filter_map(|bay| match bay {
                Bay::Machine(machine) => Some(machine.name.clone()),
                Bay::Empty(_) => None,
            })
            .collect()
    }
}

/// The machines on the stage: the picked slot's alone, or, split, both. A
/// picked slot with no machine shows the other's; none, nothing.
pub(crate) fn on_stage<'a>(shown: &[Bay<'a>], picked: usize, split: bool) -> Vec<&'a Machine> {
    let machines: Vec<&'a Machine> = shown
        .iter()
        .filter_map(|bay| match bay {
            Bay::Machine(machine) => Some(*machine),
            Bay::Empty(_) => None,
        })
        .collect();
    if split && machines.len() == BAYS {
        return machines;
    }
    match shown.get(picked) {
        Some(Bay::Machine(machine)) => vec![*machine],
        Some(Bay::Empty(_)) | None => machines.into_iter().take(1).collect(),
    }
}

/// The machines recorded as forked from `name`, in list order.
pub fn forks_of(machines: &[Machine], name: &MachineName) -> Vec<MachineName> {
    machines
        .iter()
        .filter(|machine| match &machine.lineage {
            RecordedLineage::Known(lineage) => match &lineage.origin {
                Origin::Forked { from, .. } => from == name,
                Origin::Unrecorded | Origin::Installed | Origin::Imported => false,
            },
            RecordedLineage::Unreadable(_) => false,
        })
        .map(|machine| machine.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use doppel_machine::lineage::Lineage;

    fn machine(name: &str, state: MachineState) -> Machine {
        Machine {
            name: name.parse().unwrap(),
            state,
            lineage: RecordedLineage::Known(Lineage::default()),
        }
    }

    fn golden(name: &str, state: MachineState) -> Machine {
        Machine {
            lineage: RecordedLineage::Known(Lineage::made(Origin::Installed).with_role(
                Role::Golden {
                    template: "ready".parse().unwrap(),
                    agent: doppel_machine::lineage::TemplateAgent::Unrecorded,
                },
            )),
            ..machine(name, state)
        }
    }

    #[test]
    fn golden_images_are_listed_apart_whatever_their_state() {
        assert_eq!(golden("g", MachineState::Stopped).group(), Group::Golden);
        assert_eq!(
            golden("g", MachineState::Live(Run::Running)).group(),
            Group::Golden
        );
        assert_eq!(
            machine("a", MachineState::Live(Run::Paused)).group(),
            Group::Running
        );
        assert_eq!(machine("a", MachineState::Stopped).group(), Group::Stopped);
        let unreadable = Machine {
            lineage: RecordedLineage::Unreadable("bad".into()),
            ..machine("u", MachineState::Stopped)
        };
        assert_eq!(unreadable.group(), Group::Stopped);
    }

    #[test]
    fn a_stopped_golden_image_makes_machines_and_starts_only_after_asking() {
        let stopped = golden("g", MachineState::Stopped);
        let actions = stopped.actions();
        assert_eq!(actions.first(), Some(&Action::FromTemplate));
        assert!(Action::FromTemplate.shown());
        assert!(!actions.contains(&Action::Start));
        assert!(actions.contains(&Action::StartGolden));
        assert!(!Action::StartGolden.shown());
        // Running anyway, it's handled as any running machine.
        assert_eq!(
            golden("g", MachineState::Live(Run::Running)).actions(),
            MachineState::Live(Run::Running).actions()
        );
        assert_eq!(
            machine("a", MachineState::Stopped).actions(),
            MachineState::Stopped.actions()
        );
    }

    #[test]
    fn the_status_bar_says_where_a_machine_comes_from() {
        assert_eq!(
            golden("g", MachineState::Stopped).provenance(),
            Some(Provenance::Said("Template · ready".into()))
        );
        let fork = Machine {
            lineage: RecordedLineage::Known(Lineage::forked(
                "golden-27".parse().unwrap(),
                "ready".parse().unwrap(),
            )),
            ..machine("copy", MachineState::Stopped)
        };
        assert_eq!(
            fork.provenance(),
            Some(Provenance::Said("From golden-27 · ready".into()))
        );
        let temporary = Machine {
            lineage: RecordedLineage::Known(
                Lineage::forked("golden-27".parse().unwrap(), "ready".parse().unwrap())
                    .with_role(Role::Temporary),
            ),
            ..machine("golden-27-ready-1", MachineState::Stopped)
        };
        assert_eq!(
            temporary.provenance(),
            Some(Provenance::Said(
                "Temporary · from golden-27 · ready".into()
            ))
        );
        assert_eq!(machine("a", MachineState::Stopped).provenance(), None);
        let unreadable = Machine {
            lineage: RecordedLineage::Unreadable("bad".into()),
            ..machine("u", MachineState::Stopped)
        };
        assert_eq!(unreadable.provenance(), Some(Provenance::Unreadable));
    }

    fn machines() -> Vec<Machine> {
        vec![
            machine("a", MachineState::Stopped),
            machine("b", MachineState::Live(Run::Running)),
            machine("c", MachineState::Live(Run::Paused)),
            machine("d", MachineState::Stopped),
        ]
    }

    fn stage(raw: &[&str]) -> Vec<MachineName> {
        raw.iter().map(|name| name.parse().unwrap()).collect()
    }

    fn held<'a>(shown: &[Bay<'a>]) -> Vec<Option<&'a str>> {
        shown
            .iter()
            .map(|bay| match bay {
                Bay::Machine(machine) => Some(machine.name.as_str()),
                Bay::Empty(_) => None,
            })
            .collect()
    }

    #[test]
    fn two_bays_hold_the_live_machines_in_list_order() {
        let all = machines();
        assert_eq!(held(&Bays::default().shown(&all)), [Some("b"), Some("c")]);
        assert_eq!(held(&Bays::default().shown(&[])), [None, None]);
        let three = vec![
            machine("x", MachineState::Live(Run::Running)),
            machine("y", MachineState::Live(Run::Running)),
            machine("z", MachineState::Live(Run::Running)),
        ];
        assert_eq!(held(&Bays::default().shown(&three)), [Some("x"), Some("y")]);
    }

    #[test]
    fn a_live_machine_keeps_its_bay_and_a_stopped_one_leaves_it() {
        let all = machines();
        assert_eq!(
            held(&Bays::of(stage(&["c", "b"])).shown(&all)),
            [Some("c"), Some("b")]
        );
        assert_eq!(
            held(&Bays::of(stage(&["a", "b"])).shown(&all)),
            [Some("c"), Some("b")]
        );
    }

    #[test]
    fn a_machine_started_from_a_bay_lands_in_it() {
        let bays = Bays::default().start_in(1, "dev".parse().unwrap());
        let dev = [machine("dev", MachineState::Live(Run::Starting))];
        assert_eq!(held(&bays.shown(&dev)), [None, Some("dev")]);
        let stopped = [machine("dev", MachineState::Stopped)];
        let shown = bays.shown(&stopped);
        assert_eq!(bays.kept_after(&shown), bays);
        assert!(Bays::names(&shown).is_empty());
    }

    #[test]
    fn the_stage_shows_the_picked_slot_or_both_split() {
        let two = [
            machine("a", MachineState::Live(Run::Running)),
            machine("b", MachineState::Live(Run::Running)),
        ];
        let names = |machines: Vec<&Machine>| -> Vec<String> {
            machines
                .iter()
                .map(|machine| machine.name.to_string())
                .collect()
        };
        let shown = Bays::default().shown(&two);
        assert_eq!(names(on_stage(&shown, 1, false)), ["b"]);
        assert_eq!(names(on_stage(&shown, 0, true)), ["a", "b"]);
        let one = [machine("a", MachineState::Live(Run::Running))];
        let shown = Bays::default().shown(&one);
        assert_eq!(names(on_stage(&shown, 1, false)), ["a"]);
        assert_eq!(names(on_stage(&shown, 1, true)), ["a"]);
        let none = [machine("a", MachineState::Stopped)];
        assert!(on_stage(&Bays::default().shown(&none), 0, false).is_empty());
    }

    #[test]
    fn each_state_allows_its_actions() {
        assert_eq!(
            MachineState::Live(Run::Paused).actions(),
            [Action::Resume, Action::Checkpoint, Action::ForceStop]
        );
        assert_eq!(
            MachineState::Unreadable("x".into()).actions(),
            [Action::Delete]
        );
        assert_eq!(
            MachineState::Live(Run::Starting).actions(),
            [Action::ForceStop]
        );
        assert!(MachineState::Stopped.actions().contains(&Action::Start));
    }

    #[test]
    fn a_session_reads_as_working_only_while_it_runs_calls() {
        let mut session = Session {
            id: SessionId(1),
            client: String::new(),
            machines: Vec::new(),
            running: vec!["click".into()],
            answered: 0,
            activity: SessionActivity::Active,
        };
        assert_eq!(session.doing(None), "click");
        assert_eq!(session.working(None), Some("click"));
        // The live feed's call wins over the polled list's.
        assert_eq!(
            session.doing(Some("Looking at the screen")),
            "Looking at the screen"
        );
        assert_eq!(session.who(), "An agent");
        assert_eq!(session.initial(), '#');
        session.activity = SessionActivity::Paused;
        assert_eq!(session.doing(Some("click")), "Held: its calls wait");
        assert_eq!(session.working(Some("click")), None);
        session.running.clear();
        session.activity = SessionActivity::Active;
        assert_eq!(session.doing(None), "Idle");
        session.activity = SessionActivity::Unknown(7);
        assert_eq!(session.doing(None), "State 7");
        session.client = " claude-code".into();
        assert_eq!(session.initial(), 'C');
    }
}
