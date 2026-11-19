//! `doppel setup`'s plan: from what is on this Mac now, the next step
//! toward a golden image an agent can be given (`doppel mcp --from NAME`).
//! Pure: the command gathers [`Facts`], runs the [`Next`] step, and asks
//! again, until it is [`Next::Done`] or [`Next::Blocked`]. So a second run,
//! after an interruption or a fix, carries on where the first stopped.
//!
//! A machine is set up in phases the host can't all observe (whether its
//! agent was granted, say), so the command keeps a [`Record`] of the last
//! one finished. The record also marks the machine as setup's: a machine of
//! the same name that setup didn't make is never touched.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use doppel_host::guest_user::GuestUserName;
use doppel_machine::bundle::CheckpointLabel;
use doppel_machine::machine::MachineName;

use crate::doctor::{Checkpoint, Daemon, Power, Role, Service};

/// The oldest macOS that installs a machine with its account made
/// (`create --user`): host and restore image alike.
pub(crate) const MIN_MACOS: u32 = 27;

/// A macOS version's major number, from `sw_vers -productVersion`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MacOsMajor(u32);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} is not a macOS version")]
pub(crate) struct MacOsVersionError(String);

impl FromStr for MacOsMajor {
    type Err = MacOsVersionError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let version = s.trim();
        version
            .split('.')
            .next()
            .and_then(|major| major.parse().ok())
            .map(Self)
            .ok_or_else(|| MacOsVersionError(version.to_owned()))
    }
}

impl fmt::Display for MacOsMajor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "macOS {}", self.0)
    }
}

/// The last phase setup finished for a machine, as its [`Record`] says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Installing macOS began. A machine in this phase whose bundle exists
    /// is a failed install.
    Creating,
    /// macOS is installed and the account made; no agent yet.
    Created,
    /// The agent is installed as a LaunchAgent.
    Bootstrapped,
    /// The agent has Accessibility and Screen Recording.
    Granted,
}

impl Phase {
    /// Its name in the record.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Creating => "creating",
            Self::Created => "created",
            Self::Bootstrapped => "bootstrapped",
            Self::Granted => "granted",
        }
    }
}

impl FromStr for Phase {
    type Err = RecordError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "creating" => Ok(Self::Creating),
            "created" => Ok(Self::Created),
            "bootstrapped" => Ok(Self::Bootstrapped),
            "granted" => Ok(Self::Granted),
            other => Err(RecordError(format!("unknown phase {other:?}"))),
        }
    }
}

/// Which machine setup is making, as which guest user, and how far it got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Record {
    pub(crate) machine: MachineName,
    pub(crate) user: GuestUserName,
    pub(crate) phase: Phase,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("setup record: {0}")]
pub(crate) struct RecordError(String);

impl Record {
    /// The record as JSON, as the store keeps it.
    pub(crate) fn to_json(&self) -> String {
        serde_json::json!({
            "machine": self.machine.as_str(),
            "user": self.user.as_str(),
            "phase": self.phase.as_str(),
        })
        .to_string()
    }

    /// # Errors
    /// [`RecordError`] for anything but what [`Self::to_json`] writes.
    pub(crate) fn parse(json: &str) -> Result<Self, RecordError> {
        let value: serde_json::Value =
            serde_json::from_str(json).map_err(|error| RecordError(error.to_string()))?;
        let field = |name: &str| {
            value
                .get(name)
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| RecordError(format!("no {name:?}")))
        };
        Ok(Self {
            machine: field("machine")?.parse().map_err(
                |error: doppel_machine::machine::SpecError| RecordError(error.to_string()),
            )?,
            user: field("user")?.parse().map_err(
                |error: doppel_host::guest_user::GuestUserError| RecordError(error.to_string()),
            )?,
            phase: field("phase")?.parse()?,
        })
    }
}

/// The restore image to install from, if one is at hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Image {
    /// `--ipsw`.
    Given(PathBuf),
    /// Downloaded by an earlier step; removed once installed.
    Downloaded(PathBuf),
    None,
}

/// What the command found, before any decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Facts {
    pub(crate) host: MacOsMajor,
    pub(crate) daemon: Daemon,
    /// The golden image to make.
    pub(crate) name: MachineName,
    /// The record, if setup has one.
    pub(crate) record: Option<Record>,
    pub(crate) image: Image,
}

/// One step toward the golden image. The command runs it and then asks
/// [`Next::of`] again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    /// Have launchd start the daemon on demand (`daemon install`).
    InstallDaemon,
    /// Stop a daemon of another version; launchd starts this one's.
    RestartDaemon,
    /// Find the newest restore image and download it.
    Download,
    /// Install macOS from this image, with the account made on first boot.
    Create(PathBuf),
    /// Start the machine without waiting for an agent: it has none yet.
    Boot,
    /// Install the agent over SSH (`agent bootstrap`).
    Bootstrap,
    /// Shut the machine down, to grant.
    Stop,
    /// Write the agent's privacy grants into the stopped disk (`agent grant`).
    Grant,
    /// Start the machine and wait for its agent-ready desktop.
    Start,
    /// Make it a golden image (`golden prepare`), which stops it.
    Prepare,
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InstallDaemon => f.write_str("installing the daemon"),
            Self::RestartDaemon => f.write_str("restarting the daemon at this version"),
            Self::Download => f.write_str("downloading macOS"),
            Self::Create(ipsw) => write!(f, "installing macOS from {}", ipsw.display()),
            Self::Boot => f.write_str("starting the machine"),
            Self::Bootstrap => f.write_str("installing the agent"),
            Self::Stop => f.write_str("shutting the machine down"),
            Self::Grant => f.write_str("granting the agent its permissions"),
            Self::Start => f.write_str("starting the machine"),
            Self::Prepare => f.write_str("making it a golden image"),
        }
    }
}

/// Why setup can't go on by itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Blocked {
    HostTooOld(MacOsMajor),
    /// The daemon is installed, or another one is, and nothing answers.
    DaemonNotAnswering(Service),
    DaemonFailed(String),
    /// A machine of this name exists and setup didn't make it.
    NameTaken(MachineName),
    /// Installing macOS into it began and didn't finish.
    CreateUnfinished(MachineName),
    /// It's a golden image whose template is gone or unreadable.
    TemplateMissing {
        name: MachineName,
        template: CheckpointLabel,
    },
    /// Its lineage can't be read, with why.
    Unreadable {
        name: MachineName,
        why: String,
    },
}

impl fmt::Display for Blocked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HostTooOld(major) => write!(
                f,
                "this Mac runs {major}; setup needs macOS {MIN_MACOS} or later to make a machine's account. Update macOS, then run setup again"
            ),
            Self::DaemonNotAnswering(Service::Installed) => f.write_str(
                "the daemon is installed but doesn't answer: see `doppel daemon logs`, then run setup again",
            ),
            Self::DaemonNotAnswering(Service::InstalledForAnother) => f.write_str(
                "launchd runs another store's or build's daemon: `doppel daemon install` from this one replaces it",
            ),
            Self::DaemonNotAnswering(Service::NotInstalled) => {
                f.write_str("the daemon isn't installed: `doppel daemon install`")
            }
            Self::DaemonFailed(why) => write!(f, "the daemon failed: {why}"),
            Self::NameTaken(name) => write!(
                f,
                "a machine named '{name}' exists and setup didn't make it: pass --name to make another"
            ),
            Self::CreateUnfinished(name) => write!(
                f,
                "installing macOS into '{name}' didn't finish: delete it (doppel delete {name}) and run setup again, or pass --name"
            ),
            Self::TemplateMissing { name, template } => write!(
                f,
                "golden image '{name}' has lost its template '{template}': pass --name to make another"
            ),
            Self::Unreadable { name, why } => write!(f, "can't read machine '{name}': {why}"),
        }
    }
}

/// What to do now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Next {
    Run(Step),
    /// The golden image is ready, with this template.
    Done(CheckpointLabel),
    Blocked(Blocked),
}

impl Next {
    pub(crate) fn of(facts: &Facts) -> Self {
        if facts.host.0 < MIN_MACOS {
            return Self::Blocked(Blocked::HostTooOld(facts.host));
        }
        let machines = match &facts.daemon {
            Daemon::Answered(machines) => machines,
            Daemon::NotRunning(Service::NotInstalled) => return Self::Run(Step::InstallDaemon),
            Daemon::NotRunning(service @ (Service::Installed | Service::InstalledForAnother)) => {
                return Self::Blocked(Blocked::DaemonNotAnswering(*service));
            }
            Daemon::OtherVersion { .. } => return Self::Run(Step::RestartDaemon),
            Daemon::Failed(why) => return Self::Blocked(Blocked::DaemonFailed(why.clone())),
        };
        let name = &facts.name;
        let phase = facts
            .record
            .as_ref()
            .filter(|record| &record.machine == name)
            .map(|record| record.phase);
        let Some(machine) = machines.iter().find(|machine| &machine.name == name) else {
            return match &facts.image {
                Image::Given(path) | Image::Downloaded(path) => {
                    Self::Run(Step::Create(path.clone()))
                }
                Image::None => Self::Run(Step::Download),
            };
        };
        match &machine.role {
            Role::Golden {
                template,
                template_found: Checkpoint::Present,
                ..
            } => Self::Done(template.clone()),
            Role::Golden {
                template,
                template_found:
                    Checkpoint::Missing | Checkpoint::Incomplete | Checkpoint::Unknown(_),
                ..
            } => Self::Blocked(Blocked::TemplateMissing {
                name: name.clone(),
                template: template.clone(),
            }),
            Role::Unreadable(why) => Self::Blocked(Blocked::Unreadable {
                name: name.clone(),
                why: why.clone(),
            }),
            Role::Other => match (phase, machine.power) {
                (None, Power::Live | Power::Off) => Self::Blocked(Blocked::NameTaken(name.clone())),
                (Some(Phase::Creating), Power::Live | Power::Off) => {
                    Self::Blocked(Blocked::CreateUnfinished(name.clone()))
                }
                (Some(Phase::Created), Power::Off) => Self::Run(Step::Boot),
                (Some(Phase::Created), Power::Live) => Self::Run(Step::Bootstrap),
                (Some(Phase::Bootstrapped), Power::Live) => Self::Run(Step::Stop),
                (Some(Phase::Bootstrapped), Power::Off) => Self::Run(Step::Grant),
                (Some(Phase::Granted), Power::Off) => Self::Run(Step::Start),
                (Some(Phase::Granted), Power::Live) => Self::Run(Step::Prepare),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::Machine;

    const NAME: &str = "golden";

    fn facts(machines: Vec<Machine>, phase: Option<Phase>) -> Facts {
        Facts {
            host: MacOsMajor(27),
            daemon: Daemon::Answered(machines),
            name: NAME.parse().unwrap(),
            record: phase.map(|phase| Record {
                machine: NAME.parse().unwrap(),
                user: "doppel".parse().unwrap(),
                phase,
            }),
            image: Image::None,
        }
    }

    fn working(power: Power) -> Machine {
        Machine {
            name: NAME.parse().unwrap(),
            power,
            role: Role::Other,
        }
    }

    #[test]
    fn a_fresh_mac_installs_the_daemon_then_downloads_then_creates() {
        let mut fresh = facts(vec![], None);
        fresh.daemon = Daemon::NotRunning(Service::NotInstalled);
        assert_eq!(Next::of(&fresh), Next::Run(Step::InstallDaemon));
        fresh.daemon = Daemon::Answered(vec![]);
        assert_eq!(Next::of(&fresh), Next::Run(Step::Download));
        fresh.image = Image::Downloaded("/s/restore-images/26B131.ipsw".into());
        assert_eq!(
            Next::of(&fresh),
            Next::Run(Step::Create("/s/restore-images/26B131.ipsw".into()))
        );
    }

    #[test]
    fn a_given_image_is_used_without_downloading() {
        let mut given = facts(vec![], None);
        given.image = Image::Given("/i/a.ipsw".into());
        assert_eq!(
            Next::of(&given),
            Next::Run(Step::Create("/i/a.ipsw".into()))
        );
    }

    #[test]
    fn each_phase_and_power_has_one_next_step() {
        let cases = [
            (Phase::Created, Power::Off, Step::Boot),
            (Phase::Created, Power::Live, Step::Bootstrap),
            (Phase::Bootstrapped, Power::Live, Step::Stop),
            (Phase::Bootstrapped, Power::Off, Step::Grant),
            (Phase::Granted, Power::Off, Step::Start),
            (Phase::Granted, Power::Live, Step::Prepare),
        ];
        for (phase, power, step) in cases {
            assert_eq!(
                Next::of(&facts(vec![working(power)], Some(phase))),
                Next::Run(step),
                "{phase:?} {power:?}"
            );
        }
    }

    #[test]
    fn a_golden_image_with_its_template_is_done() {
        let golden = Machine {
            name: NAME.parse().unwrap(),
            power: Power::Off,
            role: Role::Golden {
                template: "ready".parse().unwrap(),
                template_found: Checkpoint::Present,
                agent: doppel_machine::lineage::TemplateAgent::Unrecorded,
            },
        };
        // With or without a record: one made by hand counts too.
        for phase in [None, Some(Phase::Granted)] {
            assert_eq!(
                Next::of(&facts(vec![golden.clone()], phase)),
                Next::Done("ready".parse().unwrap())
            );
        }
    }

    #[test]
    fn a_machine_setup_didnt_make_is_left_alone() {
        assert_eq!(
            Next::of(&facts(vec![working(Power::Live)], None)),
            Next::Blocked(Blocked::NameTaken(NAME.parse().unwrap()))
        );
        // A record for another machine doesn't count.
        let mut other = facts(vec![working(Power::Live)], Some(Phase::Granted));
        if let Some(record) = &mut other.record {
            record.machine = "other".parse().unwrap();
        }
        assert_eq!(
            Next::of(&other),
            Next::Blocked(Blocked::NameTaken(NAME.parse().unwrap()))
        );
    }

    #[test]
    fn an_unfinished_install_is_not_carried_on() {
        assert_eq!(
            Next::of(&facts(vec![working(Power::Off)], Some(Phase::Creating))),
            Next::Blocked(Blocked::CreateUnfinished(NAME.parse().unwrap()))
        );
    }

    #[test]
    fn another_daemon_version_is_restarted_and_an_old_host_refused() {
        let mut other = facts(vec![], None);
        other.daemon = Daemon::OtherVersion { ours: 3, theirs: 2 };
        assert_eq!(Next::of(&other), Next::Run(Step::RestartDaemon));
        let mut old = facts(vec![], None);
        old.host = "26.4".parse().unwrap();
        assert_eq!(
            Next::of(&old),
            Next::Blocked(Blocked::HostTooOld(MacOsMajor(26)))
        );
    }

    #[test]
    fn the_record_reads_back_what_it_writes() {
        let record = Record {
            machine: NAME.parse().unwrap(),
            user: "doppel".parse().unwrap(),
            phase: Phase::Bootstrapped,
        };
        assert_eq!(Record::parse(&record.to_json()).unwrap(), record);
        assert!(Record::parse(r#"{"machine":"golden","user":"doppel","phase":"done"}"#).is_err());
        assert!(Record::parse("{}").is_err());
    }

    #[test]
    fn versions_parse_to_their_major() {
        assert_eq!("27.2\n".parse::<MacOsMajor>().unwrap(), MacOsMajor(27));
        assert_eq!("27".parse::<MacOsMajor>().unwrap(), MacOsMajor(27));
        assert!("".parse::<MacOsMajor>().is_err());
        assert!("x.1".parse::<MacOsMajor>().is_err());
    }
}
