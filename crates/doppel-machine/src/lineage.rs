//! What a machine is for, and where it came from: its role (a working
//! machine, or a golden image to fork from) and its origin (installed,
//! imported, forked from another's checkpoint).
//!
//! Recorded in the bundle's `lineage.pb` ([`crate::bundle::MachineBundle::lineage`]):
//! written inside the staging directory by the operations that make a
//! bundle, so it appears with the bundle; rewritten whole (temp file, then
//! rename) when the role changes. Machines made before it was recorded have
//! no file: they read as [`Lineage::default`], a working machine whose
//! origin is [`Origin::Unrecorded`]. Nothing guesses an origin.

use std::fmt;

use crate::bundle::CheckpointLabel;
use crate::machine::MachineName;

/// What a machine is for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Role {
    /// A machine to use: start it, change it, throw it away.
    #[default]
    Working,
    /// A template to fork from: new machines start from its `template`
    /// checkpoint. Starting it lets it drift from that checkpoint.
    Golden { template: CheckpointLabel },
}

/// How a machine came to be. Only what was recorded when it was made.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Origin {
    /// Its files don't say: unknown, and never guessed.
    #[default]
    Unrecorded,
    /// macOS installed into it from a restore image (`doppel create`).
    Installed,
    /// Adopted from existing VZ machine files (`doppel import`).
    Imported,
    /// Made from `checkpoint` of `from` (`fork`, `fork-run`, a new copy).
    /// `from` may have been deleted or renamed since.
    Forked {
        from: MachineName,
        checkpoint: CheckpointLabel,
    },
}

/// A machine's role and origin. Any pair is valid: a golden image may be
/// a fork, and a fork may be marked golden.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Lineage {
    pub role: Role,
    pub origin: Origin,
}

impl Lineage {
    /// A working machine made by `origin`.
    #[must_use]
    pub const fn made(origin: Origin) -> Self {
        Self {
            role: Role::Working,
            origin,
        }
    }

    /// A working machine forked from `checkpoint` of `from`: whatever the
    /// source's role, the fork is to be worked on.
    #[must_use]
    pub const fn forked(from: MachineName, checkpoint: CheckpointLabel) -> Self {
        Self::made(Origin::Forked { from, checkpoint })
    }

    /// The same machine in another role; its origin stays.
    #[must_use]
    pub fn with_role(self, role: Role) -> Self {
        Self { role, ..self }
    }

    /// The checkpoint new machines start from, when it is a golden image.
    #[must_use]
    pub const fn template(&self) -> Option<&CheckpointLabel> {
        match &self.role {
            Role::Golden { template } => Some(template),
            Role::Working => None,
        }
    }
}

/// A machine's lineage as read from its bundle. A file that can't be read
/// or decoded doesn't hide the machine or pass for a working one: it is
/// [`Self::Unreadable`], with why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordedLineage {
    /// Read, or absent (then [`Lineage::default`]).
    Known(Lineage),
    /// Why it couldn't be read.
    Unreadable(String),
}

impl RecordedLineage {
    /// The lineage, when it could be read.
    #[must_use]
    pub const fn known(&self) -> Option<&Lineage> {
        match self {
            Self::Known(lineage) => Some(lineage),
            Self::Unreadable(_) => None,
        }
    }

    /// The template checkpoint, when it is known to be a golden image.
    #[must_use]
    pub fn template(&self) -> Option<&CheckpointLabel> {
        self.known().and_then(Lineage::template)
    }
}

/// How `ls` and the viewers say it: `golden (template ready)`, `from
/// golden-27 · ready`, both, or nothing for a working machine whose
/// origin wasn't recorded or isn't a fork.
impl fmt::Display for Lineage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let role = match &self.role {
            Role::Golden { template } => Some(format!("golden (template {template})")),
            Role::Working => None,
        };
        let origin = match &self.origin {
            Origin::Forked { from, checkpoint } => Some(format!("from {from} · {checkpoint}")),
            Origin::Unrecorded | Origin::Installed | Origin::Imported => None,
        };
        let parts: Vec<String> = role.into_iter().chain(origin).collect();
        f.write_str(&parts.join(", "))
    }
}

impl fmt::Display for RecordedLineage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Known(lineage) => lineage.fmt(f),
            Self::Unreadable(why) => write!(f, "lineage unreadable: {why}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(s: &str) -> CheckpointLabel {
        s.parse().unwrap()
    }

    #[test]
    fn an_unrecorded_machine_is_a_working_one_of_unknown_origin() {
        let lineage = Lineage::default();
        assert_eq!(lineage.role, Role::Working);
        assert_eq!(lineage.origin, Origin::Unrecorded);
        assert_eq!(lineage.template(), None);
        assert_eq!(lineage.to_string(), "");
    }

    #[test]
    fn a_fork_is_a_working_machine_whatever_its_source() {
        let fork = Lineage::forked("golden-27".parse().unwrap(), label("ready"));
        assert_eq!(fork.role, Role::Working);
        assert_eq!(fork.to_string(), "from golden-27 · ready");
    }

    #[test]
    fn a_role_change_keeps_the_origin() {
        let golden = Lineage::made(Origin::Installed).with_role(Role::Golden {
            template: label("ready"),
        });
        assert_eq!(golden.origin, Origin::Installed);
        assert_eq!(golden.template(), Some(&label("ready")));
        assert_eq!(golden.to_string(), "golden (template ready)");
        let back = golden.with_role(Role::Working);
        assert_eq!(back, Lineage::made(Origin::Installed));
    }

    #[test]
    fn a_golden_fork_says_both() {
        let lineage =
            Lineage::forked("base".parse().unwrap(), label("clean")).with_role(Role::Golden {
                template: label("ready"),
            });
        assert_eq!(
            lineage.to_string(),
            "golden (template ready), from base · clean"
        );
    }

    #[test]
    fn an_unreadable_record_is_neither_golden_nor_working() {
        let unreadable = RecordedLineage::Unreadable("truncated".into());
        assert_eq!(unreadable.known(), None);
        assert_eq!(unreadable.template(), None);
        assert_eq!(unreadable.to_string(), "lineage unreadable: truncated");
    }
}
