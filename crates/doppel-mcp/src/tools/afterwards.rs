//! The screen shortly after an input: what an `act`, `click`, `type` or
//! `key` result shows, so the model sees its effect without another call.
//!
//! Most inputs show their effect within a moment; a model that has to ask
//! for it spends a whole round trip on every action. The runner looks once,
//! [`Afterwards::SETTLE`] after the input, and the result lists what changed
//! since the model's previous listing. Slower effects (a page loading, an app
//! launching) may not show yet; the result says so, and `wait_for` is for
//! those.

use std::time::Duration;

use doppel_desktop::ax::compact::{CompactionPolicy, Show, Tokens};
use doppel_desktop::ax::tree::{AxSnapshot, Menus, SnapshotScope, SnapshotSpec};
use doppel_machine::machine::MachineName;

use crate::refs::Refs;

/// What the runner saw after sending an input.
#[derive(Debug, Clone, PartialEq)]
pub enum Afterwards<E> {
    /// It didn't look: the input went through the host, where the guest
    /// agent may not be there to answer.
    NotLooked,
    /// The frontmost app, [`Self::SETTLE`] after the input.
    Seen(AxSnapshot),
    /// Looking failed. The input itself was sent.
    Unseen(E),
}

impl<E: std::fmt::Display> Afterwards<E> {
    /// How long after an input the runner looks.
    pub const SETTLE: Duration = Duration::from_millis(500);

    /// What the runner looks at: the frontmost app, without its menus.
    pub const LOOK: SnapshotSpec = SnapshotSpec {
        scope: SnapshotScope::FrontmostApp,
        menus: Menus::Exclude,
    };

    /// A listing of changes, smaller than a snapshot's default: it follows
    /// every input.
    const POLICY: CompactionPolicy = CompactionPolicy {
        budget: Tokens::SMALL_BUDGET,
        max_text: 160,
        show: Show::Changes,
    };

    /// `done` (what was sent), then what the screen shows now. A listing
    /// shown here becomes the machine's latest refs, as a snapshot's does.
    pub(crate) fn render(self, done: &str, machine: &MachineName, refs: &mut Refs) -> String {
        match self {
            Self::NotLooked => {
                format!("{done}. The effect is asynchronous: wait_for or snapshot to see it.")
            }
            Self::Seen(snapshot) => {
                let listing = refs.show(machine, &snapshot, Self::POLICY).text;
                format!("{done}. {} ms later:\n{listing}", Self::SETTLE.as_millis())
            }
            Self::Unseen(error) => format!(
                "{done}. Looking at the screen afterwards failed ({error}): snapshot to see the effect."
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::tests::ax::snapshot;

    fn dev() -> MachineName {
        "dev".parse().unwrap()
    }

    #[test]
    fn an_input_s_result_lists_what_changed_and_its_refs_resolve() {
        let mut refs = Refs::default();
        let first =
            Afterwards::<String>::Seen(snapshot(1, "Save")).render("pressed", &dev(), &mut refs);
        assert!(first.starts_with("pressed. 500 ms later"), "{first}");
        assert!(first.contains("[s1e2]"), "{first}");
        // The next look lists only what changed since.
        let second =
            Afterwards::<String>::Seen(snapshot(2, "Save")).render("typed", &dev(), &mut refs);
        assert!(second.contains("no changes since"), "{second}");
    }

    #[test]
    fn without_a_look_the_model_is_told_to_look_itself() {
        let mut refs = Refs::default();
        let host = Afterwards::<String>::NotLooked.render("clicked", &dev(), &mut refs);
        assert!(host.contains("wait_for or snapshot"), "{host}");
        let failed = Afterwards::Unseen("agent unreachable").render("clicked", &dev(), &mut refs);
        assert!(
            failed.contains("agent unreachable") && failed.contains("snapshot to see"),
            "{failed}"
        );
    }
}
