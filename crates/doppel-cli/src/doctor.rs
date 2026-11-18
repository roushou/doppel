//! `doppel doctor`'s judgement: whether this Mac can serve an agent a
//! machine of its own (`doppel mcp --from GOLDEN`), and if not, what to do.
//! Pure: the command gathers [`Facts`]; [`Diagnosis::of`] decides.

use std::fmt;
use std::path::PathBuf;

use doppel_machine::bundle::CheckpointLabel;
use doppel_machine::machine::MachineName;

/// What the command found, before any judgement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Facts {
    pub(crate) vmm: PathBuf,
    pub(crate) vmm_found: Program,
    pub(crate) agent_archive: PathBuf,
    pub(crate) agent_archive_found: Archive,
    pub(crate) daemon: Daemon,
    /// Apple's limit on VMs running at once.
    pub(crate) max_running: usize,
}

/// Whether a program is there to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Program {
    Present,
    Missing,
    NotExecutable,
    /// Its metadata couldn't be read, with why.
    Unreadable(String),
}

/// The guest agent's archive beside `doppel`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Archive {
    /// With its build id.
    Present(String),
    Missing,
    /// It or its build id couldn't be read, with why.
    Unreadable(String),
}

/// What the daemon said, if anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Daemon {
    Answered(Vec<Machine>),
    /// Nothing answers on its socket.
    NotRunning(Service),
    /// It answers in another protocol version: an older or newer build.
    OtherVersion {
        ours: u32,
        theirs: u32,
    },
    /// It answered, or was reached, and then failed, with why.
    Failed(String),
}

/// Whether launchd starts this daemon on demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Service {
    Installed,
    /// launchd starts another store's or build's daemon instead.
    InstalledForAnother,
    NotInstalled,
}

/// One machine, as far as doctor cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Machine {
    pub(crate) name: MachineName,
    pub(crate) power: Power,
    pub(crate) role: Role,
}

/// Whether a machine holds one of the VMs Apple lets run at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Power {
    /// Its VMM runs: paused and saving count.
    Live,
    Off,
}

/// A machine's role, with what matters for a golden image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Role {
    Golden {
        template: CheckpointLabel,
        template_found: Checkpoint,
    },
    /// A working or temporary machine.
    Other,
    /// Its lineage couldn't be read, with why: it may be a golden image.
    Unreadable(String),
}

/// Whether a golden image still has its template checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Checkpoint {
    Present,
    Missing,
    /// Its checkpoints couldn't be read, with why.
    Unknown(String),
}

/// What doctor checks, in the order it says them. Their names are part of
/// `--json`'s output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Check {
    Vmm,
    AgentArchive,
    Daemon,
    GoldenImage,
    Room,
}

impl Check {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Vmm => "vmm",
            Self::AgentArchive => "agent-archive",
            Self::Daemon => "daemon",
            Self::GoldenImage => "golden-image",
            Self::Room => "room",
        }
    }
}

/// One check's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    Pass(String),
    /// Works, with something to know or fix.
    Warn {
        problem: String,
        fix: String,
    },
    /// Keeps an agent from getting a machine.
    Fail {
        problem: String,
        fix: String,
    },
    /// Not checked: another check it needs failed.
    Skipped(Check),
}

impl Verdict {
    pub(crate) const fn status(&self) -> &'static str {
        match self {
            Self::Pass(_) => "pass",
            Self::Warn { .. } => "warn",
            Self::Fail { .. } => "fail",
            Self::Skipped(_) => "skipped",
        }
    }

    /// What was found, in a sentence.
    pub(crate) fn detail(&self) -> String {
        match self {
            Self::Pass(detail)
            | Self::Warn {
                problem: detail, ..
            }
            | Self::Fail {
                problem: detail, ..
            } => detail.clone(),
            Self::Skipped(needs) => format!("needs {}", needs.name()),
        }
    }

    /// What to do about it, when there is something.
    pub(crate) fn fix(&self) -> Option<&str> {
        match self {
            Self::Warn { fix, .. } | Self::Fail { fix, .. } => Some(fix),
            Self::Pass(_) | Self::Skipped(_) => None,
        }
    }
}

/// A golden image an agent can be given a fork of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Golden {
    pub(crate) name: MachineName,
    pub(crate) template: CheckpointLabel,
}

/// Every check's verdict, and the golden images ready to fork. Ready when
/// nothing failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Diagnosis {
    pub(crate) checks: Vec<(Check, Verdict)>,
    pub(crate) goldens: Vec<Golden>,
}

impl Diagnosis {
    pub(crate) fn of(facts: &Facts) -> Self {
        let mut goldens = Vec::new();
        let (golden, room) = match &facts.daemon {
            Daemon::Answered(machines) => {
                let (verdict, ready) = golden_image(machines);
                goldens = ready;
                (verdict, room(machines, facts.max_running))
            }
            Daemon::NotRunning(_) | Daemon::OtherVersion { .. } | Daemon::Failed(_) => (
                Verdict::Skipped(Check::Daemon),
                Verdict::Skipped(Check::Daemon),
            ),
        };
        Self {
            checks: vec![
                (Check::Vmm, vmm(facts)),
                (Check::AgentArchive, agent_archive(facts)),
                (Check::Daemon, daemon(&facts.daemon)),
                (Check::GoldenImage, golden),
                (Check::Room, room),
            ],
            goldens,
        }
    }

    /// Whether `doppel mcp --from` can give an agent a machine now.
    pub(crate) fn is_ready(&self) -> bool {
        !self
            .checks
            .iter()
            .any(|(_, verdict)| matches!(verdict, Verdict::Fail { .. } | Verdict::Skipped(_)))
    }

    /// The `--json` form: `ready`, the `golden_images` to pass to
    /// `--from`, and each check's `status`, `detail` and `fix`.
    pub(crate) fn json(&self) -> serde_json::Value {
        let checks: Vec<_> = self
            .checks
            .iter()
            .map(|(check, verdict)| {
                let mut entry = serde_json::json!({
                    "check": check.name(),
                    "status": verdict.status(),
                    "detail": verdict.detail(),
                });
                if let Some(fix) = verdict.fix() {
                    entry["fix"] = fix.into();
                }
                entry
            })
            .collect();
        let goldens: Vec<_> = self
            .goldens
            .iter()
            .map(|golden| {
                serde_json::json!({
                    "name": golden.name.to_string(),
                    "template": golden.template.to_string(),
                })
            })
            .collect();
        serde_json::json!({
            "ready": self.is_ready(),
            "golden_images": goldens,
            "checks": checks,
        })
    }
}

impl fmt::Display for Diagnosis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (check, verdict) in &self.checks {
            let mark = match verdict {
                Verdict::Pass(_) => "✓",
                Verdict::Warn { .. } => "!",
                Verdict::Fail { .. } => "✗",
                Verdict::Skipped(_) => "-",
            };
            writeln!(f, "{mark} {:<14} {}", check.name(), verdict.detail())?;
            if let Some(fix) = verdict.fix() {
                writeln!(f, "  {:<14} fix: {fix}", "")?;
            }
        }
        match self.goldens.first() {
            Some(golden) if self.is_ready() => {
                writeln!(f, "\nready: doppel mcp --from {}", golden.name)
            }
            Some(_) | None => writeln!(f, "\nnot ready"),
        }
    }
}

fn vmm(facts: &Facts) -> Verdict {
    let path = facts.vmm.display();
    let fix = "build it with scripts/build.sh, or point --vmm (DOPPEL_VMM) at it".to_owned();
    match &facts.vmm_found {
        Program::Present => Verdict::Pass(path.to_string()),
        Program::Missing => Verdict::Fail {
            problem: format!("{path} is missing"),
            fix,
        },
        Program::NotExecutable => Verdict::Fail {
            problem: format!("{path} is not executable"),
            fix,
        },
        Program::Unreadable(why) => Verdict::Fail {
            problem: format!("{path}: {why}"),
            fix,
        },
    }
}

fn agent_archive(facts: &Facts) -> Verdict {
    let path = facts.agent_archive.display();
    // Forks of a golden image carry the agent already: only new machines
    // need the archive.
    let fix = "build it with scripts/build.sh; `doppel agent install` and `bootstrap` need it for machines without an agent".to_owned();
    match &facts.agent_archive_found {
        Archive::Present(build) => Verdict::Pass(format!("{path} (build {build})")),
        Archive::Missing => Verdict::Warn {
            problem: format!("{path} is missing"),
            fix,
        },
        Archive::Unreadable(why) => Verdict::Warn {
            problem: format!("{path}: {why}"),
            fix,
        },
    }
}

fn daemon(daemon: &Daemon) -> Verdict {
    match daemon {
        Daemon::Answered(machines) => Verdict::Pass(format!(
            "answering, {} machine{}",
            machines.len(),
            if machines.len() == 1 { "" } else { "s" }
        )),
        Daemon::NotRunning(Service::NotInstalled) => Verdict::Fail {
            problem: "not running, and launchd doesn't start it".to_owned(),
            fix: "doppel daemon install".to_owned(),
        },
        Daemon::NotRunning(Service::InstalledForAnother) => Verdict::Fail {
            problem: "not running; launchd starts another store's or build's daemon".to_owned(),
            fix: "doppel daemon install, to start this one instead".to_owned(),
        },
        Daemon::NotRunning(Service::Installed) => Verdict::Fail {
            problem: "not running, though launchd should start it".to_owned(),
            fix: "doppel daemon logs shows why; doppel daemon install loads it again".to_owned(),
        },
        Daemon::OtherVersion { ours, theirs } => Verdict::Fail {
            problem: format!("speaks protocol v{theirs}; this doppel speaks v{ours}"),
            fix: "doppel daemon stop: launchd starts this build's on the next call".to_owned(),
        },
        Daemon::Failed(why) => Verdict::Fail {
            problem: why.clone(),
            fix: "doppel daemon logs shows why".to_owned(),
        },
    }
}

/// The verdict, and the golden images ready to fork, in listing order.
fn golden_image(machines: &[Machine]) -> (Verdict, Vec<Golden>) {
    let mut ready = Vec::new();
    let mut unusable = Vec::new();
    for machine in machines {
        let name = &machine.name;
        match &machine.role {
            Role::Golden {
                template,
                template_found: Checkpoint::Present,
            } => ready.push(Golden {
                name: name.clone(),
                template: template.clone(),
            }),
            Role::Golden {
                template,
                template_found: Checkpoint::Missing,
            } => unusable.push(format!("{name}: its template '{template}' is gone")),
            Role::Golden {
                template_found: Checkpoint::Unknown(why),
                ..
            } => unusable.push(format!("{name}: its checkpoints can't be read ({why})")),
            Role::Unreadable(why) => {
                unusable.push(format!("{name}: its role can't be read ({why})"));
            }
            Role::Other => {}
        }
    }
    let names = || {
        ready
            .iter()
            .map(|golden| format!("{} (template {})", golden.name, golden.template))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let verdict = match (ready.is_empty(), unusable.is_empty()) {
        (false, true) => Verdict::Pass(names()),
        (false, false) => Verdict::Warn {
            problem: format!("{}; but {}", names(), unusable.join("; ")),
            fix: "doppel golden mark NAME --checkpoint LABEL with a checkpoint it has, or doppel golden unmark NAME".to_owned(),
        },
        (true, true) => Verdict::Fail {
            problem: "none: doppel mcp --from needs one".to_owned(),
            fix: "on a running, logged-in machine with the agent: doppel golden prepare NAME"
                .to_owned(),
        },
        (true, false) => Verdict::Fail {
            problem: format!("none usable: {}", unusable.join("; ")),
            fix: "doppel golden prepare NAME again".to_owned(),
        },
    };
    (verdict, ready)
}

fn room(machines: &[Machine], max: usize) -> Verdict {
    let live: Vec<_> = machines
        .iter()
        .filter(|machine| machine.power == Power::Live)
        .map(|machine| machine.name.to_string())
        .collect();
    let running = format!("{} of {max} VMs running", live.len());
    if live.len() < max {
        Verdict::Pass(running)
    } else {
        Verdict::Fail {
            problem: format!("{running} ({}): no new machine can start", live.join(", ")),
            fix: "doppel stop NAME".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(raw: &str) -> MachineName {
        raw.parse().unwrap()
    }

    fn label(raw: &str) -> CheckpointLabel {
        raw.parse().unwrap()
    }

    fn golden(raw: &str, found: Checkpoint) -> Machine {
        Machine {
            name: name(raw),
            power: Power::Off,
            role: Role::Golden {
                template: label("ready"),
                template_found: found,
            },
        }
    }

    fn working(raw: &str, power: Power) -> Machine {
        Machine {
            name: name(raw),
            power,
            role: Role::Other,
        }
    }

    fn facts(daemon: Daemon) -> Facts {
        Facts {
            vmm: "/opt/doppel/doppel-vmm".into(),
            vmm_found: Program::Present,
            agent_archive: "/opt/doppel/doppel-agent.zip".into(),
            agent_archive_found: Archive::Present("abc123".into()),
            daemon,
            max_running: 2,
        }
    }

    fn verdict(diagnosis: &Diagnosis, check: Check) -> &Verdict {
        &diagnosis
            .checks
            .iter()
            .find(|(c, _)| *c == check)
            .unwrap()
            .1
    }

    #[test]
    fn a_mac_with_a_usable_golden_image_and_room_is_ready() {
        let d = Diagnosis::of(&facts(Daemon::Answered(vec![
            golden("golden-27", Checkpoint::Present),
            working("dev", Power::Live),
        ])));
        assert!(d.is_ready(), "{d}");
        assert_eq!(
            d.goldens,
            vec![Golden {
                name: name("golden-27"),
                template: label("ready")
            }]
        );
    }

    #[test]
    fn without_a_daemon_the_checks_that_need_it_are_skipped_and_not_ready() {
        let d = Diagnosis::of(&facts(Daemon::NotRunning(Service::NotInstalled)));
        assert!(!d.is_ready());
        assert_eq!(
            verdict(&d, Check::Daemon).fix(),
            Some("doppel daemon install")
        );
        assert_eq!(
            verdict(&d, Check::GoldenImage),
            &Verdict::Skipped(Check::Daemon)
        );
        assert_eq!(verdict(&d, Check::Room), &Verdict::Skipped(Check::Daemon));
    }

    #[test]
    fn a_missing_agent_archive_only_warns() {
        let mut f = facts(Daemon::Answered(vec![golden("g", Checkpoint::Present)]));
        f.agent_archive_found = Archive::Missing;
        let d = Diagnosis::of(&f);
        assert_eq!(verdict(&d, Check::AgentArchive).status(), "warn");
        assert!(d.is_ready());
    }

    #[test]
    fn a_missing_vmm_fails() {
        let mut f = facts(Daemon::Answered(vec![golden("g", Checkpoint::Present)]));
        f.vmm_found = Program::NotExecutable;
        assert!(!Diagnosis::of(&f).is_ready());
    }

    #[test]
    fn a_golden_image_without_its_template_is_not_one_to_fork() {
        let d = Diagnosis::of(&facts(Daemon::Answered(vec![golden(
            "g",
            Checkpoint::Missing,
        )])));
        assert_eq!(verdict(&d, Check::GoldenImage).status(), "fail");
        assert!(d.goldens.is_empty());

        let d = Diagnosis::of(&facts(Daemon::Answered(vec![
            golden("g", Checkpoint::Missing),
            golden("h", Checkpoint::Present),
        ])));
        assert_eq!(verdict(&d, Check::GoldenImage).status(), "warn");
        assert!(d.is_ready());
    }

    #[test]
    fn an_unreadable_role_is_never_taken_for_a_working_machine() {
        let d = Diagnosis::of(&facts(Daemon::Answered(vec![Machine {
            name: name("g"),
            power: Power::Off,
            role: Role::Unreadable("bad lineage".into()),
        }])));
        let golden = verdict(&d, Check::GoldenImage);
        assert_eq!(golden.status(), "fail");
        assert!(golden.detail().contains("can't be read"), "{golden:?}");
    }

    #[test]
    fn no_golden_image_fails_with_how_to_make_one() {
        let d = Diagnosis::of(&facts(Daemon::Answered(vec![working("dev", Power::Off)])));
        let golden = verdict(&d, Check::GoldenImage);
        assert_eq!(golden.status(), "fail");
        assert!(golden.fix().unwrap().contains("doppel golden prepare"));
    }

    #[test]
    fn the_running_limit_reached_leaves_no_room() {
        let machines = vec![
            golden("g", Checkpoint::Present),
            working("a", Power::Live),
            working("b", Power::Live),
        ];
        let d = Diagnosis::of(&facts(Daemon::Answered(machines)));
        assert_eq!(verdict(&d, Check::Room).status(), "fail");
        assert!(!d.is_ready());
    }

    #[test]
    fn json_is_pinned() {
        let mut f = facts(Daemon::Answered(vec![
            golden("golden-27", Checkpoint::Present),
            working("dev", Power::Live),
        ]));
        f.agent_archive_found = Archive::Missing;
        insta::assert_snapshot!("ready", format!("{:#}", Diagnosis::of(&f).json()));
        insta::assert_snapshot!(
            "no_daemon",
            format!(
                "{:#}",
                Diagnosis::of(&facts(Daemon::NotRunning(Service::Installed))).json()
            )
        );
    }

    #[test]
    fn text_says_each_check_and_what_to_run() {
        let d = Diagnosis::of(&facts(Daemon::Answered(vec![golden(
            "golden-27",
            Checkpoint::Present,
        )])));
        let text = d.to_string();
        assert!(text.contains("✓ vmm"), "{text}");
        assert!(
            text.ends_with("ready: doppel mcp --from golden-27\n"),
            "{text}"
        );
    }
}
