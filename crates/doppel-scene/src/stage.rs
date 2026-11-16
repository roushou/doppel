//! What the viewer's stage shows: the bays' machines, the slot picked, whether
//! both bays show side by side, and the free bay whose launcher is open.

use doppel_machine::machine::MachineName;

use crate::model::{Bay, Bays, Machine, on_stage};

/// How the stage is laid out. A slot is a bay's position in the header.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stage {
    bays: Bays,
    /// The slot whose machine the stage shows.
    picked: usize,
    split: Split,
    /// The free slot whose launcher is open, if one is.
    launching: Option<usize>,
}

/// Whether both bays' machines show side by side, when both run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Split {
    #[default]
    Off,
    On,
}

impl Stage {
    /// A stage whose bays are given `names` in order (`doppel view a b`).
    pub fn of(names: impl IntoIterator<Item = MachineName>) -> Self {
        Self {
            bays: Bays::of(names),
            ..Self::default()
        }
    }

    /// The bays shown among `machines`.
    pub fn bays<'a>(&self, machines: &'a [Machine]) -> Vec<Bay<'a>> {
        self.bays.shown(machines)
    }

    /// Keeps each bay's machine as it is shown now, so a machine keeps its
    /// bay once it's live.
    pub fn settle(&mut self, machines: &[Machine]) {
        self.bays = self.bays.kept_after(&self.bays.shown(machines));
    }

    /// The machines in the bays.
    pub fn names(&self, machines: &[Machine]) -> Vec<MachineName> {
        Bays::names(&self.bays.shown(machines))
    }

    /// The machines the stage shows: the picked slot's, or both when split.
    pub fn shows<'a>(&self, machines: &'a [Machine]) -> Vec<&'a Machine> {
        on_stage(
            &self.bays.shown(machines),
            self.picked,
            self.split == Split::On,
        )
    }

    /// The slot holding `name`, if one does.
    pub fn slot_of(&self, name: &MachineName, machines: &[Machine]) -> Option<usize> {
        self.bays
            .shown(machines)
            .iter()
            .position(|bay| matches!(bay, Bay::Machine(machine) if &machine.name == name))
    }

    pub const fn split(&self) -> Split {
        self.split
    }

    pub const fn launching(&self) -> Option<usize> {
        self.launching
    }

    /// Shows slot `index`'s machine alone.
    pub fn show(&mut self, index: usize) {
        self.picked = index;
        self.split = Split::Off;
        self.launching = None;
    }

    /// Starts `name` from slot `index`: shown there alone, it lands in that
    /// bay once it's live.
    pub fn start_in(&mut self, index: usize, name: MachineName) {
        self.bays = self.bays.start_in(index, name);
        self.show(index);
    }

    /// Opens slot `index`'s launcher, or closes it if it's open.
    pub fn toggle_launcher(&mut self, index: usize) {
        self.launching = match self.launching {
            Some(open) if open == index => None,
            Some(_) | None => Some(index),
        };
    }

    /// Closes the open launcher: `false` if none was.
    pub fn close_launcher(&mut self) -> bool {
        self.launching.take().is_some()
    }

    pub fn toggle_split(&mut self) {
        self.split = match self.split {
            Split::Off => Split::On,
            Split::On => Split::Off,
        };
    }

    /// Leaves the split: `false` if it wasn't split.
    pub fn unsplit(&mut self) -> bool {
        let was = self.split == Split::On;
        self.split = Split::Off;
        was
    }
}

#[cfg(test)]
mod tests {
    use doppel_machine::machine::MachineName;

    use super::{Split, Stage};
    use doppel_machine::lineage::RecordedLineage;

    use crate::model::{Machine, MachineState, Run};

    fn machine(name: &str, state: MachineState) -> Machine {
        Machine {
            name: name.parse().unwrap(),
            state,
            lineage: RecordedLineage::Known(Default::default()),
        }
    }

    fn running(name: &str) -> Machine {
        machine(name, MachineState::Live(Run::Running))
    }

    fn name(raw: &str) -> MachineName {
        raw.parse().unwrap()
    }

    fn shown(stage: &Stage, machines: &[Machine]) -> Vec<String> {
        stage
            .shows(machines)
            .iter()
            .map(|machine| machine.name.to_string())
            .collect()
    }

    #[test]
    fn the_picked_slot_shows_alone_unless_split_with_both_running() {
        let both = [running("a"), running("b")];
        let mut stage = Stage::default();
        assert_eq!(shown(&stage, &both), ["a"]);
        stage.show(1);
        assert_eq!(shown(&stage, &both), ["b"]);
        stage.toggle_split();
        assert_eq!(stage.split(), Split::On);
        assert_eq!(shown(&stage, &both), ["a", "b"]);
        // Split with one running shows the one.
        assert_eq!(shown(&stage, &[running("a")]), ["a"]);
        assert!(stage.unsplit());
        assert!(!stage.unsplit());
        assert_eq!(shown(&stage, &both), ["b"]);
    }

    #[test]
    fn showing_a_slot_leaves_the_split_and_closes_the_launcher() {
        let mut stage = Stage::default();
        stage.toggle_split();
        stage.toggle_launcher(1);
        stage.show(0);
        assert_eq!(stage.split(), Split::Off);
        assert_eq!(stage.launching(), None);
    }

    #[test]
    fn a_launcher_toggles_and_another_slots_replaces_it() {
        let mut stage = Stage::default();
        stage.toggle_launcher(0);
        assert_eq!(stage.launching(), Some(0));
        stage.toggle_launcher(1);
        assert_eq!(stage.launching(), Some(1));
        stage.toggle_launcher(1);
        assert_eq!(stage.launching(), None);
        assert!(!stage.close_launcher());
        stage.toggle_launcher(0);
        assert!(stage.close_launcher());
        assert_eq!(stage.launching(), None);
    }

    #[test]
    fn a_machine_started_from_a_slot_lands_there_and_is_shown() {
        let mut stage = Stage::default();
        stage.toggle_launcher(1);
        stage.start_in(1, name("dev"));
        assert_eq!(stage.launching(), None);
        let live = [running("other"), running("dev")];
        assert_eq!(stage.slot_of(&name("dev"), &live), Some(1));
        assert_eq!(shown(&stage, &live), ["dev"]);
    }

    #[test]
    fn a_settled_machine_keeps_its_bay_when_another_starts() {
        let mut stage = Stage::default();
        stage.settle(&[running("b")]);
        let both = [running("a"), running("b")];
        assert_eq!(stage.slot_of(&name("b"), &both), Some(0));
        assert_eq!(stage.names(&both), [name("b"), name("a")]);
        // Stopped machines leave the stage.
        assert!(
            stage
                .names(&[machine("b", MachineState::Stopped)])
                .is_empty()
        );
    }
}
