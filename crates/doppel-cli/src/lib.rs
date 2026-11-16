//! `doppel`: the outermost shell. Parses arguments and environment once,
//! wires capabilities, and prints results. `main.rs` only sets up the
//! process and runs [`Cli`].

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

mod copy;
mod daemon_at;
mod export;
mod fork_run;
mod launchd;
mod mcp;
mod reel;
mod report;
mod signals;
mod tokens;
mod view;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use doppel_client::CheckpointResult;
use doppel_host::capabilities::{Clock as _, OsEntropy, SystemClock, WallClock as _};
use doppel_host::files::LocalFilesystem;
use doppel_host::golden::plan::GuestUserName;
use doppel_host::launcher::VmmBinary;
use doppel_host::machines::{
    CALL_TIMEOUT, CHECKPOINT_TIMEOUT, CLAIM_TIMEOUT, FINISH_RECORDING_TIMEOUT, ImportSource,
    Machines,
};
use doppel_host::provision::FullName;
use doppel_host::store::Store;
use doppel_host::vmm::RecordingStart;
use doppel_machine::agent::VsockPort;
use doppel_machine::bundle::{CheckpointLabel, DataRoot, RuntimeDir};
use doppel_machine::checkpoint::{AfterCheckpoint, Checkpoint, GuestAfterFailure, GuestSync};
use doppel_machine::deadline::Deadline;
use doppel_machine::files::FileMode;
use doppel_machine::geometry::Point;
use doppel_machine::guest_path::GuestPath;
use doppel_machine::input::{Button, ClickCount, Input, KeyChord};
use doppel_machine::machine::{
    Access, CpuCount, DiskSize, DisplaySpec, Hardware, MacAddress, MachineName, MemorySize,
    NetworkSpec, ShareName, SharedDirectory,
};
use doppel_protocol::transport::Cancel;

use crate::copy::CopyEnd;
use crate::daemon_at::{DaemonAt, LIFECYCLE, within};
pub use crate::report::{GuestAfter, Report};

/// The `doppel` command line: what to do, and where machines are.
#[derive(Debug, Parser)]
#[command(
    name = "doppel",
    version,
    about = "A double of your Mac for AI agents."
)]
pub struct Cli {
    /// Where machines are stored.
    #[arg(long, env = "DOPPEL_HOME", global = true)]
    home: Option<PathBuf>,
    /// Directory for VMM sockets (kept short: unix socket paths max out at 103 bytes).
    #[arg(long, env = "DOPPEL_RUNTIME_DIR", global = true)]
    runtime_dir: Option<PathBuf>,
    /// The signed doppel-vmm binary. Defaults to the one next to `doppel`.
    #[arg(long, env = "DOPPEL_VMM", global = true)]
    vmm: Option<PathBuf>,
    /// Use the daemon listening on this socket instead of this store's:
    /// another Mac's, forwarded with `ssh -L`. Commands that go through a
    /// daemon then act on that Mac's machines; `view` reads their displays
    /// encoded.
    #[arg(long, global = true, value_name = "SOCKET")]
    host: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

/// Where a command does its work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// Through a daemon, which may be another Mac's.
    Daemon,
    /// On this Mac's store, or this Mac's daemon itself.
    ThisMac,
}

impl Command {
    const fn reach(&self) -> Reach {
        match self {
            Self::Ls
            | Self::Start { .. }
            | Self::Login { .. }
            | Self::Status { .. }
            | Self::View { .. }
            | Self::Stop { .. }
            | Self::Pause { .. }
            | Self::Resume { .. }
            | Self::Checkpoint { .. }
            | Self::Fork { .. }
            | Self::ForkRun { .. }
            | Self::Restore { .. }
            | Self::Sessions { .. }
            | Self::Mcp { .. } => Reach::Daemon,
            Self::Create { .. }
            | Self::Import { .. }
            | Self::SetPassword { .. }
            | Self::ForgetPassword { .. }
            | Self::Record(_)
            | Self::Checkpoints { .. }
            | Self::Screenshot { .. }
            | Self::Input { .. }
            | Self::Exec { .. }
            | Self::Cp { .. }
            | Self::Daemon(_)
            | Self::Agent(_)
            | Self::Golden(_)
            | Self::Debug(_) => Reach::ThisMac,
        }
    }
}

#[derive(Debug, clap::Args)]
struct HardwareArgs {
    #[arg(long, default_value_t = 4)]
    cpus: u32,
    #[arg(long, default_value_t = 4)]
    memory_gib: u32,
    #[arg(long, default_value_t = 64)]
    disk_gib: u32,
    /// Display width in points (pixels are 2×, Retina).
    #[arg(long, default_value_t = 1280)]
    width: u32,
    #[arg(long, default_value_t = 800)]
    height: u32,
    /// Fixed MAC address. Generated if omitted.
    #[arg(long, conflicts_with = "offline")]
    mac: Option<MacAddress>,
    /// No network device.
    #[arg(long)]
    offline: bool,
    /// Share a host directory read-only: NAME=/absolute/path. Repeatable.
    #[arg(long = "share", value_name = "NAME=PATH")]
    shares: Vec<ShareArg>,
    /// Share a host directory read-write: NAME=/absolute/path. Repeatable.
    #[arg(long = "share-rw", value_name = "NAME=PATH")]
    shares_rw: Vec<ShareArg>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Then {
    Resume,
    Stop,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List machines.
    Ls,
    /// Install macOS into a new machine from a restore image.
    ///
    /// With `--user`, macOS creates that account itself on the first boot
    /// (macOS 27 hosts and restore images): logged in automatically, Remote
    /// Login on, and a generated password stored as the machine's login
    /// password. The machine is left running. Without it, a person goes
    /// through Setup Assistant (`doppel view`).
    Create {
        name: MachineName,
        #[arg(long)]
        ipsw: PathBuf,
        /// The account's short name.
        #[arg(long)]
        user: Option<GuestUserName>,
        /// The account's full name. Defaults to the short name.
        #[arg(long, requires = "user")]
        full_name: Option<FullName>,
        #[command(flatten)]
        hardware: HardwareArgs,
    },
    /// Adopt existing VZ machine files (cloned, not moved).
    Import {
        name: MachineName,
        #[arg(long)]
        hardware_model: PathBuf,
        #[arg(long)]
        machine_identifier: PathBuf,
        #[arg(long)]
        aux: PathBuf,
        #[arg(long)]
        disk: PathBuf,
        #[command(flatten)]
        hardware: HardwareArgs,
    },
    /// Start a machine (resuming saved state if it has one) and wait for an
    /// agent-ready desktop: if the guest is locked, the stored password
    /// (`doppel set-password`) is typed once.
    Start {
        name: MachineName,
        /// Show the VM in a native window.
        #[arg(long)]
        window: bool,
        /// Return once the VM runs, without waiting for the desktop.
        #[arg(long)]
        no_wait: bool,
    },
    /// Wait for an agent-ready desktop on a running machine, typing the
    /// stored password once if the guest is locked.
    Login {
        name: MachineName,
    },
    /// Store the guest user's login password (read from stdin, not echoed)
    /// in the machine's bundle, readable by you only.
    SetPassword {
        name: MachineName,
    },
    /// Delete the stored login password.
    ForgetPassword {
        name: MachineName,
    },
    Status {
        name: MachineName,
    },
    /// Record a machine's display to a video file.
    #[command(subcommand)]
    Record(RecordCommand),
    /// Open the desktop window: machines, agents, screens and recordings. The named
    /// machines open on stage; otherwise the first running one.
    View {
        names: Vec<MachineName>,
        /// Read each display as H.264 from its VMM rather than from the
        /// memory it shares, as a viewer on another Mac must.
        #[arg(long)]
        encoded: bool,
    },
    Stop {
        name: MachineName,
        /// Power off immediately instead of asking the guest to shut down.
        #[arg(long)]
        force: bool,
    },
    Pause {
        name: MachineName,
    },
    Resume {
        name: MachineName,
    },
    /// Snapshot memory and disk of a running machine at one instant.
    Checkpoint {
        name: MachineName,
        label: CheckpointLabel,
        #[arg(long, value_enum, default_value = "resume")]
        then: Then,
    },
    /// List a machine's checkpoints.
    Checkpoints {
        name: MachineName,
    },
    /// Create a new machine that resumes from a checkpoint of another.
    Fork {
        source: MachineName,
        label: CheckpointLabel,
        into: MachineName,
    },
    /// Fork a checkpoint fresh COUNT times, start the forks side by side,
    /// and run the command once per fork with its name in `DOPPEL_MACHINE`
    /// (which `doppel mcp` targets): `doppel fork-run dev clean --count 2 --
    /// claude -p "…"`. The forks cold-boot from the checkpoint's disk with
    /// identities of their own. When every command has ended, they are
    /// stopped and kept.
    ForkRun {
        source: MachineName,
        label: CheckpointLabel,
        /// How many forks: at most the VMs macOS can run beside the
        /// running ones (two in all).
        #[arg(long, default_value_t = 2)]
        count: u32,
        /// Show the forks in `doppel view` while the commands run.
        #[arg(long)]
        view: bool,
        /// Have each command's `doppel mcp` record its session
        /// (`DOPPEL_RECORD=1`).
        #[arg(long)]
        record: bool,
        /// The program to run per fork, then its arguments.
        #[arg(required = true, last = true)]
        command: Vec<OsString>,
    },
    /// Roll a stopped machine back to a checkpoint, discarding its current disk.
    Restore {
        name: MachineName,
        label: CheckpointLabel,
        /// Required: the machine's current disk is replaced.
        #[arg(long, required = true)]
        discard_current: bool,
    },
    /// Save a host-side screenshot (works before login, no guest agent needed).
    Screenshot {
        name: MachineName,
        #[arg(long, short)]
        output: PathBuf,
    },
    /// Send input through the VM's own keyboard and pointer (works before login).
    Input {
        name: MachineName,
        #[command(subcommand)]
        action: InputAction,
    },
    /// Run a program in the guest through its agent; exits with the program's status.
    Exec {
        name: MachineName,
        /// Stop the program after this many milliseconds.
        #[arg(long, default_value_t = 10_000)]
        timeout_ms: u64,
        /// Absolute guest working directory.
        #[arg(long)]
        cwd: Option<GuestPath>,
        /// Absolute guest path of the program, then its arguments.
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Copy a file between the host and a guest: `doppel cp notes.txt dev:/tmp/notes.txt`
    /// or `doppel cp dev:/tmp/out.log ./out.log`. At most 32 MiB.
    Cp {
        source: CopyEnd,
        destination: CopyEnd,
        /// Replace the destination if it exists (by default an existing file is left alone).
        #[arg(long)]
        replace: bool,
        /// Octal permissions for a file written in the guest.
        #[arg(long, default_value = "644", value_parser = octal_mode)]
        mode: FileMode,
    },
    /// doppeld, the daemon that owns your machines (docs/doppeld.md): `ls`,
    /// `status`, `start`, `login`, `stop`, `pause`, `resume`, `checkpoint`,
    /// `fork`, `fork-run`, `restore`, `sessions` and `mcp` go through it. `install` has
    /// launchd start it when they need it.
    #[command(subcommand)]
    Daemon(DaemonCommand),
    /// The sessions open on the daemon: who drives which machines. With a
    /// subcommand, pause or resume one.
    Sessions {
        #[command(subcommand)]
        action: Option<SessionsCommand>,
    },
    /// Serve MCP over stdio, for AI agents (e.g. `claude mcp add doppel -- doppel mcp --machine dev`).
    /// The calls go to the daemon as one session.
    Mcp {
        /// Machine that tools target when a call names none.
        #[arg(long, env = "DOPPEL_MACHINE")]
        machine: Option<MachineName>,
        /// Record the session into the store's `recordings/`: a video per
        /// VM run of each machine the calls touch, with captions of the
        /// calls, and a trace of them (`trace.jsonl`).
        /// From the environment, `DOPPEL_RECORD=1` (or `true`, `yes`, `on`).
        #[arg(long, env = "DOPPEL_RECORD", value_parser = clap::builder::BoolishValueParser::new())]
        record: bool,
    },
    /// The guest agent.
    #[command(subcommand)]
    Agent(AgentCommand),
    /// Golden images: machines to fork from.
    #[command(subcommand)]
    Golden(GoldenCommand),
    /// Diagnostics.
    #[command(subcommand)]
    Debug(Debug),
}

#[derive(Debug, Subcommand)]
enum DaemonCommand {
    /// Run the daemon in the foreground until it's stopped (Ctrl-C).
    Run {
        /// Serve the socket launchd bound, as the installed LaunchAgent does.
        #[arg(long)]
        launchd: bool,
    },
    /// Install doppeld as a LaunchAgent: launchd listens on its socket from
    /// login and starts it on the first connection.
    Install,
    /// Unload and remove the LaunchAgent. Machines it started keep running.
    Uninstall,
    /// Whether the LaunchAgent is installed and the daemon runs.
    Status,
    /// Stop the running daemon; the next client starts it again.
    Stop,
    /// The end of the daemon's log.
    Logs {
        #[arg(long, default_value_t = 100)]
        lines: usize,
    },
    /// Issue a token for the browser viewer. It's printed once, on stdout;
    /// the daemon keeps only its digest.
    Token {
        /// How long it lasts: `90m`, `12h`, `7d` (at most 30 days).
        #[arg(long, default_value = "12h")]
        ttl: doppel_gateway::Ttl,
        /// What to call it, to tell tokens apart ("phone").
        #[arg(long)]
        label: Option<doppel_gateway::TokenLabel>,
    },
    /// The tokens that let their holders in now.
    Tokens,
    /// Revoke a token: it lets no one in from now on.
    Revoke {
        /// Its id, as `doppel daemon tokens` lists it.
        id: doppel_gateway::TokenId,
    },
    /// The browser viewer's listener: HTTPS and its WebSocket, for holders
    /// of a token.
    #[command(subcommand)]
    Web(WebCommand),
}

#[derive(Debug, Subcommand)]
enum WebCommand {
    /// Listen on every address, now and whenever the daemon runs.
    On {
        #[arg(long, default_value_t = 7443)]
        port: u16,
    },
    /// Stop listening, and close every browser's connection.
    Off,
    /// Whether it listens, where, and its certificate's fingerprint.
    Status,
}

/// Where the daemon's listening socket comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SocketSource {
    /// The daemon binds it.
    Bind,
    /// launchd bound it and passes it in.
    Launchd,
}

impl SocketSource {
    fn from_flag(launchd: bool) -> Self {
        if launchd { Self::Launchd } else { Self::Bind }
    }
}

#[derive(Debug, Subcommand)]
enum SessionsCommand {
    /// Hold a session's next calls, whatever machine they name; one already
    /// running finishes.
    Pause { id: u64 },
    /// Let a paused session's calls go on.
    Resume { id: u64 },
}

#[derive(Debug, Subcommand)]
enum RecordCommand {
    /// Start recording to FILE (MP4). The recording runs on after this
    /// command returns, until `record stop` or the machine stops.
    Start { name: MachineName, file: PathBuf },
    /// Stop the recording and complete its file.
    Stop { name: MachineName },
    /// Write a recorded session's video with what its agent did drawn
    /// over it, as the viewers show it, to OUT (MP4). RECORDING is a
    /// folder name in the data directory's `recordings/`.
    Export {
        recording: doppel_client::RecordingId,
        out: PathBuf,
        /// Which of its videos (`dev-002.mp4`); the first by default.
        #[arg(long)]
        video: Option<String>,
        /// Frames per second.
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=120))]
        fps: u32,
    },
}

#[derive(Debug, Subcommand)]
enum GoldenCommand {
    /// Make a running, logged-in machine a golden image: no sleep, no
    /// screensaver, auto-login (using the stored password through sudo),
    /// then a checkpoint that powers it off. Forks of that checkpoint
    /// reach an agent-ready desktop on their own. The machine is then
    /// recorded as a golden image with that checkpoint as its template.
    Prepare {
        name: MachineName,
        /// The checkpoint to take.
        #[arg(long, default_value = "ready")]
        checkpoint: CheckpointLabel,
    },
    /// Record a machine as a golden image: a template to fork from, whose
    /// new machines start from one of its checkpoints. `ls` and the viewers
    /// show it apart from working machines.
    Mark {
        name: MachineName,
        /// The checkpoint new machines start from; it must exist.
        #[arg(long, default_value = "ready")]
        checkpoint: CheckpointLabel,
    },
    /// Record a golden image as a working machine again.
    Unmark { name: MachineName },
}

#[derive(Debug, Subcommand)]
enum AgentCommand {
    /// Install `Doppel Agent.app` in the guest as a LaunchAgent (started at
    /// login, restarted if it exits), through the agent running there now.
    Install {
        name: MachineName,
        /// The zipped app from scripts/build.sh. Defaults to the one next to `doppel`.
        #[arg(long)]
        archive: Option<PathBuf>,
    },
    /// Install the agent in a machine that has none yet: log in over SSH
    /// with the stored password, start the app, then install it as `install`
    /// does. For a machine made with `doppel create … --user`, after its
    /// first boot. With an agent already answering, only installs.
    Bootstrap {
        name: MachineName,
        /// The guest account to log in as: `create --user`'s.
        #[arg(long)]
        user: GuestUserName,
        /// The zipped app from scripts/build.sh. Defaults to the one next to `doppel`.
        #[arg(long)]
        archive: Option<PathBuf>,
    },
}

/// The zipped agent app and its build id, beside it as `<archive>.build-id`.
fn agent_archive(archive: Option<PathBuf>) -> Result<doppel_host::bootstrap::AgentArchive> {
    let path = match archive {
        Some(path) => path,
        None => std::env::current_exe()?.with_file_name("doppel-agent.zip"),
    };
    let build_file = path.with_extension("build-id");
    let build = std::fs::read_to_string(&build_file)
        .with_context(|| build_file.display().to_string())?
        .trim()
        .parse()?;
    Ok(doppel_host::bootstrap::AgentArchive { path, build })
}

#[derive(Debug, Subcommand)]
enum InputAction {
    /// Click at guest display points.
    Click {
        x: f64,
        y: f64,
        #[arg(long)]
        right: bool,
        #[arg(long, default_value_t = 1)]
        count: u32,
    },
    /// Type text (US keyboard layout through the host path).
    Type { text: String },
    /// Press a key chord, e.g. return, cmd+space, cmd+shift+4.
    Key { chord: KeyChord },
}

impl InputAction {
    fn parse(self) -> Result<Input> {
        Ok(match self {
            Self::Click { x, y, right, count } => Input::Click {
                at: Point::new(x, y)?,
                button: if right { Button::Right } else { Button::Left },
                count: ClickCount::try_from(count)?,
            },
            Self::Type { text } => Input::Type(text),
            Self::Key { chord } => Input::Key(chord),
        })
    }
}

#[derive(Debug, Subcommand)]
enum Debug {
    /// Echo round trips through a guest vsock port over a handed-over socket.
    VsockEcho {
        name: MachineName,
        #[arg(long, default_value_t = 5000)]
        port: u32,
        #[arg(long, default_value_t = 100)]
        count: u32,
    },
    /// Take an AX snapshot through the guest agent: print the compacted listing,
    /// and optionally save the raw snapshot (protobuf) as a test fixture.
    AxDump {
        name: MachineName,
        #[arg(long, value_enum, default_value_t = DumpScope::Window)]
        scope: DumpScope,
        #[arg(long, value_enum, default_value_t = DumpMenus::Exclude)]
        menus: DumpMenus,
        #[arg(long, default_value_t = 4000)]
        max_tokens: u32,
        /// Write the agent's raw AxSnapshot here.
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum DumpScope {
    /// The frontmost app's focused window.
    Window,
    /// Every window of the frontmost app.
    App,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum DumpMenus {
    Exclude,
    Include,
}

impl HardwareArgs {
    fn parse(self) -> Result<Hardware> {
        let network = if self.offline {
            NetworkSpec::None
        } else {
            let mac = match self.mac {
                Some(mac) => mac,
                None => MacAddress::generate(&mut OsEntropy).context("generating a MAC address")?,
            };
            NetworkSpec::Nat { mac }
        };
        let share = |share: &ShareArg, access| -> Result<SharedDirectory> {
            Ok(SharedDirectory::new(
                share.name.clone(),
                share.path.clone(),
                access,
            )?)
        };
        let shares = self
            .shares
            .iter()
            .map(|s| share(s, Access::ReadOnly))
            .chain(self.shares_rw.iter().map(|s| share(s, Access::ReadWrite)))
            .collect::<Result<_>>()?;
        Ok(Hardware {
            cpus: CpuCount::new(self.cpus)?,
            memory: MemorySize::from_gib(self.memory_gib)?,
            disk: DiskSize::from_gib(self.disk_gib)?,
            display: DisplaySpec::retina(self.width, self.height)?,
            network,
            shares,
        })
    }
}

/// A host directory to share, as `--share` takes it: `NAME=PATH`.
#[derive(Debug, Clone)]
struct ShareArg {
    name: ShareName,
    path: PathBuf,
}

impl std::str::FromStr for ShareArg {
    type Err = anyhow::Error;

    fn from_str(raw: &str) -> Result<Self> {
        let (name, path) = raw
            .split_once('=')
            .with_context(|| format!("expected NAME=PATH, got {raw:?}"))?;
        Ok(Self {
            name: name.parse()?,
            path: PathBuf::from(path),
        })
    }
}

/// A guest file's permissions, as `--mode` takes them: octal, `0o` optional.
fn octal_mode(raw: &str) -> Result<FileMode> {
    let bits = u32::from_str_radix(raw.strip_prefix("0o").unwrap_or(raw), 8)
        .with_context(|| format!("{raw:?} is not octal"))?;
    Ok(FileMode::new(bits)?)
}

impl Cli {
    /// Runs the command it was given.
    ///
    /// # Errors
    /// The command failed, with why.
    pub fn run(self) -> Result<Report> {
        run(self)
    }
}

#[expect(clippy::too_many_lines, reason = "one arm per command")]
fn run(cli: Cli) -> Result<Report> {
    if cli.host.is_some() && cli.command.reach() == Reach::ThisMac {
        bail!("--host is for commands that go through a daemon; this one works on this Mac only");
    }
    let home = match cli.home {
        Some(home) => home,
        None => std::env::home_dir()
            .context("no home directory")?
            .join("Library/Application Support/Doppel"),
    };
    let runtime = cli
        .runtime_dir
        .unwrap_or_else(|| std::env::temp_dir().join("doppel"));
    let vmm_path = match cli.vmm {
        Some(path) => path,
        None => std::env::current_exe()?.with_file_name("doppel-vmm"),
    };
    let service = {
        let (home, runtime, vmm_path) = (home.clone(), runtime.clone(), vmm_path.clone());
        move || -> Result<launchd::Service> {
            let absolute = |path: &PathBuf| std::path::absolute(path).context("an absolute path");
            let home = absolute(&home)?;
            let program = [
                std::env::current_exe()?.into_os_string(),
                "--home".into(),
                home.clone().into_os_string(),
                "--runtime-dir".into(),
                absolute(&runtime)?.into_os_string(),
                "--vmm".into(),
                absolute(&vmm_path)?.into_os_string(),
                "daemon".into(),
                "run".into(),
                "--launchd".into(),
            ];
            let agent = launchd::LaunchAgent {
                program: program.into(),
                socket: home.join("doppeld.socket"),
                log: home.join("doppeld.log"),
            };
            let user_home = std::env::home_dir().context("no home directory")?;
            Ok(launchd::Service::new(
                agent,
                &user_home,
                nix::unistd::getuid().as_raw(),
            ))
        }
    };
    // As given, for a `doppel` this one runs.
    let given = [("--home", home.clone()), ("--runtime-dir", runtime.clone())];
    let store = Store::new(DataRoot::new(home.clone())?, RuntimeDir::new(runtime)?);

    let vmm = vmm_path.clone();
    let machines = Machines::new(
        store,
        VmmBinary::new(vmm_path),
        SystemClock,
        LocalFilesystem,
    );
    // Resolved when a command needs it: `--home` may not exist yet.
    let daemon = || -> Result<DaemonAt> {
        Ok(match &cli.host {
            Some(socket) => DaemonAt::Forwarded(socket.clone()),
            None => DaemonAt::Store(machines.store().daemon_socket()?.path().to_path_buf()),
        })
    };
    // Waits for an agent-ready desktop; fails unless it got one.
    let await_desktop =
        |client: &doppel_client::Client<SystemClock>, n: MachineName| -> Result<Report> {
            eprintln!("{n}: waiting for the desktop…");
            let outcome = client.await_desktop(&n, within(LIFECYCLE))?;
            if outcome.is_ready() {
                Ok(Report::Ready { name: n, outcome })
            } else {
                bail!("{n}: not ready: {outcome}")
            }
        };
    Ok(match cli.command {
        Command::Ls => Report::Machines(daemon()?.connect()?.machines(within(CALL_TIMEOUT))?),
        Command::Create {
            name: raw,
            ipsw,
            user,
            full_name,
            hardware,
        } => {
            use doppel_host::provision::{Account, Setup};
            use doppel_machine::login::LoginPassword;
            let n = raw;
            let setup = match user {
                None => Setup::Assistant,
                Some(user) => Setup::Provision(Account {
                    full_name: match full_name {
                        Some(full_name) => full_name,
                        None => user.as_str().parse()?,
                    },
                    user,
                    password: LoginPassword::generate(&mut OsEntropy)
                        .context("generating a password")?,
                }),
            };
            let claim = machines.claim(&n, machines.within(CLAIM_TIMEOUT))?;
            let mut last = -1i64;
            let created = claim.create(hardware.parse()?, &ipsw, &setup, |fraction| {
                let pct = (fraction * 100.0) as i64;
                if pct / 5 != last / 5 {
                    last = pct;
                    eprintln!("installing macOS: {pct}%");
                }
            })?;
            Report::Created {
                name: n,
                state: created,
                account: match setup {
                    Setup::Provision(account) => Some(account.user),
                    Setup::Assistant => None,
                },
            }
        }
        Command::Import {
            name: raw,
            hardware_model,
            machine_identifier,
            aux,
            disk,
            hardware,
        } => {
            let n = raw;
            machines.claim(&n, machines.within(CLAIM_TIMEOUT))?.import(
                hardware.parse()?,
                &ImportSource {
                    hardware_model,
                    machine_identifier,
                    aux,
                    disk,
                },
            )?;
            Report::Imported(n)
        }
        Command::Start {
            name: raw,
            window,
            no_wait,
        } => {
            let n = raw;
            let client = daemon()?.connect()?;
            let (state, next) = client.start(
                &n,
                if window {
                    doppel_client::Window::Shown
                } else {
                    doppel_client::Window::Hidden
                },
                within(LIFECYCLE),
            )?;
            let started = Report::Started {
                name: n.clone(),
                state,
                next,
            };
            if no_wait {
                started
            } else {
                // Said before the wait, which may be long.
                print!("{started}");
                await_desktop(&client, n)?
            }
        }
        Command::Login { name: raw } => await_desktop(&daemon()?.connect()?, raw)?,
        Command::SetPassword { name: raw } => {
            use doppel_host::login::{PasswordFile, SecretPrompt};
            use doppel_machine::login::LoginPassword;
            let n = raw;
            let _claim = machines.claim(&n, machines.within(CLAIM_TIMEOUT))?;
            let bundle = machines.store().bundle(&n);
            if !bundle.dir().is_dir() {
                bail!("no machine '{n}'");
            }
            let line = SecretPrompt.read_line(&format!("{n}: guest login password: "))?;
            let password = LoginPassword::parse(&line)?;
            let path = PasswordFile.write(&bundle, &password)?;
            Report::PasswordSaved { name: n, path }
        }
        Command::ForgetPassword { name: raw } => {
            let n = raw;
            let _claim = machines.claim(&n, machines.within(CLAIM_TIMEOUT))?;
            match doppel_host::login::PasswordFile.forget(&machines.store().bundle(&n))? {
                true => Report::PasswordDeleted(n),
                false => Report::NoPassword(n),
            }
        }
        Command::Record(RecordCommand::Start { name: raw, file }) => {
            let n = raw;
            let path = std::path::absolute(&file)
                .with_context(|| format!("{} has no absolute path", file.display()))?;
            let by = machines.deadline();
            match machines
                .machine(&n)
                .running(by)?
                .start_recording(&path, by)?
            {
                RecordingStart::Started => Report::RecordingStarted { name: n, path },
                RecordingStart::AlreadyRecording(running) => {
                    bail!("{n} is already recording to {}", running.display())
                }
            }
        }
        Command::Record(RecordCommand::Stop { name: raw }) => {
            let n = raw;
            let by = Deadline::after(SystemClock.now(), FINISH_RECORDING_TIMEOUT);
            let summary = machines.machine(&n).running(by)?.stop_recording(None, by)?;
            Report::RecordingStopped { name: n, summary }
        }
        Command::Record(RecordCommand::Export {
            recording,
            out,
            video,
            fps,
        }) => {
            let at = daemon()?;
            if matches!(at, DaemonAt::Forwarded(_)) {
                bail!("its video is on the Mac the daemon runs on: export it there");
            }
            let client = at.connect()?;
            let opened = view::recording(client.open_recording(&recording, within(CALL_TIMEOUT))?);
            let source = |video: &doppel_scene::recordings::Video| {
                view::Videos::Here
                    .path(
                        || {
                            client.data_root(within(CALL_TIMEOUT)).map_err(|error| {
                                doppel_viewer::DaemonError::Unreachable(error.to_string())
                            })
                        },
                        &opened.id,
                        &video.file,
                    )
                    .map_err(|error| anyhow::anyhow!("{error}"))
            };
            let snapshot = |line| {
                client
                    .seen_snapshot(&recording, line, within(CALL_TIMEOUT))
                    .ok()
            };
            let exported =
                export::export(&opened, video.as_deref(), source, &out, fps, &vmm, snapshot)?;
            Report::Exported {
                path: exported.path,
                summary: exported.summary,
            }
        }
        Command::View {
            names: raw,
            encoded,
        } => {
            let names = raw;
            let at = daemon()?;
            // Shared memory can't cross to another Mac.
            let frames = match (&at, encoded) {
                (DaemonAt::Forwarded(_), _) | (DaemonAt::Store(_), true) => view::Frames::Encoded,
                (DaemonAt::Store(_), false) => view::Frames::Shared,
            };
            // Replay reads videos from the store: on this Mac only.
            let videos = match &at {
                DaemonAt::Store(_) => view::Videos::Here,
                DaemonAt::Forwarded(_) => view::Videos::Elsewhere,
            };
            view::View::new(names, frames, videos, &home).run(at.connect()?, at.socket())?;
            Report::Nothing
        }
        Command::Status { name: raw } => {
            let n = raw;
            let status = daemon()?.connect()?.status(&n, within(CALL_TIMEOUT))?;
            Report::Status { name: n, status }
        }
        Command::Stop { name: raw, force } => {
            let n = raw;
            let report = daemon()?.connect()?.stop(
                &n,
                if force {
                    doppel_client::StopMode::Force
                } else {
                    doppel_client::StopMode::Graceful
                },
                within(LIFECYCLE),
            )?;
            Report::Stopped { name: n, report }
        }
        Command::Pause { name: raw } => {
            daemon()?.connect()?.pause(&raw, within(LIFECYCLE))?;
            Report::Paused(raw)
        }
        Command::Resume { name: raw } => {
            let resumed = daemon()?.connect()?.resume(&raw, within(LIFECYCLE))?;
            Report::Resumed { name: raw, resumed }
        }
        Command::Checkpoint {
            name: raw,
            label: raw_label,
            then,
        } => {
            let (n, l) = (raw, raw_label);
            let then = match then {
                Then::Resume => AfterCheckpoint::Resume,
                Then::Stop => AfterCheckpoint::Stop,
            };
            let report = daemon()?
                .connect()?
                .checkpoint(&n, &l, then, within(LIFECYCLE))?;
            for cleanup in &report.cleanup {
                let action = &cleanup.action;
                match &cleanup.result {
                    Ok(()) => eprintln!("cleanup {action}: ok"),
                    Err(e) => eprintln!("cleanup {action}: FAILED: {e}"),
                }
            }
            match report.result {
                CheckpointResult::Completed {
                    memory_bytes,
                    left_paused: None,
                    sync_skipped,
                } => Report::Checkpointed {
                    name: n,
                    label: l,
                    memory_bytes,
                    paused_for: report.paused_for,
                    guest: GuestAfter::AsAsked { sync_skipped },
                },
                CheckpointResult::Completed {
                    memory_bytes,
                    left_paused: Some(why),
                    ..
                } => Report::Checkpointed {
                    name: n,
                    label: l,
                    memory_bytes,
                    paused_for: report.paused_for,
                    guest: GuestAfter::LeftPaused(why),
                },
                CheckpointResult::Failed { at, error, guest } => {
                    let guest = match guest {
                        GuestAfterFailure::Untouched => "guest was not paused",
                        GuestAfterFailure::ResumeRequested => {
                            "guest resume requested (see cleanup)"
                        }
                    };
                    bail!("checkpoint failed while {at:?}: {error}; {guest}");
                }
                CheckpointResult::Unknown { at, error } => bail!(
                    "checkpoint outcome UNKNOWN: the VMM stopped answering while {at:?} ({error}). Check `doppel status {n}`"
                ),
                CheckpointResult::InProgress { .. } => {
                    bail!("checkpoint procedure ended without a terminal state")
                }
            }
        }
        Command::Checkpoints { name: raw } => {
            Report::Checkpoints(machines.machine(&raw).checkpoints()?)
        }
        Command::Fork {
            source,
            label: raw_label,
            into,
        } => {
            let (s, l, i) = (source, raw_label, into);
            daemon()?.connect()?.fork(&s, &l, &i, within(LIFECYCLE))?;
            Report::Forked {
                source: s,
                label: l,
                into: i,
            }
        }
        Command::ForkRun {
            source,
            label: raw_label,
            count,
            view,
            record,
            command,
        } => {
            let mut doppel = vec![std::env::current_exe()?.into_os_string()];
            for (flag, value) in given
                .into_iter()
                .chain(cli.host.clone().map(|host| ("--host", host)))
            {
                doppel.extend([flag.into(), std::path::absolute(value)?.into_os_string()]);
            }
            fork_run::ForkRun {
                source,
                label: raw_label,
                count,
                command,
                watch: if view {
                    fork_run::Watch::View
                } else {
                    fork_run::Watch::Unwatched
                },
                recorded: if record {
                    fork_run::Recorded::Recorded
                } else {
                    fork_run::Recorded::Unrecorded
                },
                doppel,
            }
            .run(&daemon()?)?;
            Report::Nothing
        }
        Command::Restore {
            name: raw,
            label: raw_label,
            discard_current: _,
        } => {
            let (n, l) = (raw, raw_label);
            daemon()?.connect()?.restore(&n, &l, within(LIFECYCLE))?;
            Report::Restored { name: n, label: l }
        }
        Command::Screenshot { name: raw, output } => {
            let by = machines.deadline();
            let image = machines.machine(&raw).running(by)?.screenshot(0, by)?;
            std::fs::write(&output, &image.data).with_context(|| output.display().to_string())?;
            Report::Screenshot {
                note: image.coordinate_note(),
                path: output,
            }
        }
        Command::Input { name: raw, action } => {
            let by = machines.deadline();
            machines
                .machine(&raw)
                .running(by)?
                .host_input(&action.parse()?, by)?;
            Report::InputSent(raw)
        }
        Command::Exec {
            name: raw,
            timeout_ms,
            cwd,
            command,
        } => {
            use doppel_machine::exec::{Completeness, ExecRequest, Termination};
            let (program, arguments) = command.split_first().context("give the program to run")?;
            let mut request = ExecRequest::new(
                program.parse()?,
                arguments.to_vec(),
                Duration::from_millis(timeout_ms),
                ExecRequest::MAX_OUTPUT,
            )?;
            if let Some(cwd) = cwd {
                request = request.in_directory(cwd);
            }
            let by = machines.deadline();
            let output = machines
                .machine(&raw)
                .running(by)?
                .agent(by)?
                .exec(&request, &Cancel::never())?;
            std::io::stdout().write_all(&output.stdout)?;
            std::io::stderr().write_all(&output.stderr)?;
            if output.completeness != Completeness::Full {
                eprintln!("doppel exec: output truncated");
            }
            match output.termination {
                Termination::Exited(code) => Report::Exit(code),
                Termination::Signaled(signal) => {
                    eprintln!("doppel exec: killed by signal {signal}");
                    Report::Exit(128 + signal)
                }
                Termination::TimedOut => {
                    bail!("timed out after {timeout_ms} ms; the program was stopped")
                }
            }
        }
        Command::Cp {
            source,
            destination,
            replace,
            mode,
        } => {
            use crate::copy::Copy;
            use doppel_machine::files::{MAX_FILE_BYTES, ReadRequest, WritePolicy, WriteRequest};
            let copy = Copy::between(source, destination)?;
            let policy = if replace {
                WritePolicy::Replace
            } else {
                WritePolicy::CreateNew
            };
            match copy {
                Copy::Upload { from, machine, to } => {
                    let data = std::fs::read(&from).with_context(|| from.display().to_string())?;
                    let bytes = data.len();
                    let request = WriteRequest::new(to, data, mode, policy)?;
                    let by = machines.deadline();
                    machines
                        .machine(&machine)
                        .running(by)?
                        .agent(by)?
                        .write_file(&request, by)?;
                    Report::Uploaded {
                        machine,
                        path: request.path,
                        bytes,
                    }
                }
                Copy::Download { machine, from, to } => {
                    let by = machines.deadline();
                    let data = machines
                        .machine(&machine)
                        .running(by)?
                        .agent(by)?
                        .read_file(&ReadRequest::new(from, MAX_FILE_BYTES)?, by)?;
                    let mut options = std::fs::OpenOptions::new();
                    match policy {
                        WritePolicy::CreateNew => options.write(true).create_new(true),
                        WritePolicy::Replace => options.write(true).create(true).truncate(true),
                    };
                    let mut file = options.open(&to).map_err(|error| {
                        let hint = if error.kind() == std::io::ErrorKind::AlreadyExists {
                            " (use --replace to overwrite)"
                        } else {
                            ""
                        };
                        anyhow::anyhow!("{}: {error}{hint}", to.display())
                    })?;
                    file.write_all(&data)
                        .with_context(|| to.display().to_string())?;
                    Report::Downloaded {
                        path: to,
                        bytes: data.len(),
                    }
                }
            }
        }
        Command::Golden(GoldenCommand::Prepare {
            name: raw,
            checkpoint,
        }) => {
            let (n, l) = (raw, checkpoint);
            // Owned from the first setting to the checkpoint: nothing else
            // may change the guest in between.
            let claim = machines.claim(&n, machines.within(CLAIM_TIMEOUT))?;
            let ready = machines.machine(&n).await_desktop(
                doppel_machine::login::LoginPolicy::DEFAULT,
                &Cancel::never(),
            )?;
            if !ready.is_ready() {
                bail!("{n}: not ready: {ready}");
            }
            machines
                .machine(&n)
                .golden_prepare(|step| eprintln!("{n}: {step}…"))?;
            eprintln!("{n}: settings applied; checkpointing as '{l}' and stopping…");
            let report = claim.checkpoint(
                &l,
                AfterCheckpoint::Stop,
                machines.within(CHECKPOINT_TIMEOUT),
            )?;
            match report.result {
                Checkpoint::Completed {
                    guest_sync: GuestSync::Skipped(reason),
                    ..
                } => bail!(
                    "{n}: checkpoint '{l}' was saved without syncing the guest ({reason}): its disk may lack the auto-login settings, so cold boots from it can stop at the login window. Don't fork it; prepare again under a new label"
                ),
                Checkpoint::Completed {
                    memory_bytes,
                    guest_sync: GuestSync::Synced,
                    ..
                } => {
                    claim.mark_golden(&l)?;
                    Report::GoldenPrepared {
                        name: n,
                        label: l,
                        memory_bytes,
                    }
                }
                other @ (Checkpoint::InProgress { .. }
                | Checkpoint::Failed { .. }
                | Checkpoint::Unknown { .. }) => {
                    bail!("{n}: checkpoint '{l}' didn't complete: {other:?}")
                }
            }
        }
        Command::Golden(GoldenCommand::Mark {
            name: raw,
            checkpoint,
        }) => {
            let (n, l) = (raw, checkpoint);
            let lineage = machines
                .claim(&n, machines.within(CLAIM_TIMEOUT))?
                .mark_golden(&l)?;
            Report::GoldenMarked {
                name: n,
                label: l,
                lineage,
            }
        }
        Command::Golden(GoldenCommand::Unmark { name: raw }) => {
            let n = raw;
            machines
                .claim(&n, machines.within(CLAIM_TIMEOUT))?
                .mark_working()?;
            Report::GoldenUnmarked(n)
        }
        Command::Agent(AgentCommand::Install { name: raw, archive }) => {
            let archive = agent_archive(archive)?;
            let bytes =
                std::fs::read(&archive.path).with_context(|| archive.path.display().to_string())?;
            let installed = machines
                .machine(&raw)
                .install_agent(bytes, archive.build, |step| {
                    eprintln!("{raw}: {step}…");
                })?;
            Report::AgentInstalled {
                name: raw,
                over_ssh: None,
                installed,
            }
        }
        Command::Agent(AgentCommand::Bootstrap {
            name: raw,
            user,
            archive,
        }) => {
            use doppel_host::bootstrap::Started;
            let archive = agent_archive(archive)?;
            let done = machines.machine(&raw).bootstrap_agent(
                user,
                &archive,
                &std::env::current_exe()?,
                |progress| eprintln!("{raw}: {progress}…"),
            )?;
            if let Some((dir, error)) = &done.leftover {
                eprintln!("{raw}: warning: couldn't remove {dir} in the guest: {error}");
            }
            Report::AgentInstalled {
                name: raw,
                over_ssh: match done.started {
                    Started::AlreadyRunning => None,
                    Started::OverSsh(address) => Some(address),
                },
                installed: done.installed,
            }
        }
        Command::Daemon(DaemonCommand::Install) => {
            let service = service()?;
            service.install()?;
            Report::Service(service.status()?)
        }
        Command::Daemon(DaemonCommand::Uninstall) => {
            service()?.uninstall()?;
            Report::DaemonUninstalled
        }
        Command::Daemon(DaemonCommand::Status) => Report::Service(service()?.status()?),
        Command::Daemon(DaemonCommand::Stop) => match service()?.stop()? {
            Some(pid) => Report::DaemonStopped(pid),
            None => Report::DaemonNotRunning,
        },
        Command::Daemon(DaemonCommand::Logs { lines }) => {
            Report::Verbatim(service()?.log_tail(lines)?)
        }
        Command::Daemon(DaemonCommand::Token { ttl, label }) => {
            tokens::issue(
                &daemon()?,
                ttl,
                label.as_ref(),
                &SystemClock,
                machines.deadline(),
            )?;
            // Printed by `issue`: a secret never sits in a report.
            Report::Nothing
        }
        Command::Daemon(DaemonCommand::Web(command)) => {
            let client = daemon()?.connect()?;
            let status = match command {
                WebCommand::On { port } => client.configure_web(Some(port), machines.deadline())?,
                WebCommand::Off => client.configure_web(None, machines.deadline())?,
                WebCommand::Status => client.web_status(machines.deadline())?,
            };
            match status {
                doppel_client::WebStatus::Off => Report::WebOff,
                doppel_client::WebStatus::Listening {
                    port,
                    fingerprint,
                    names,
                } => Report::WebListening {
                    port,
                    fingerprint,
                    names,
                },
                doppel_client::WebStatus::Failed { port, reason } => {
                    bail!("doppeld: web: not listening on port {port}: {reason}")
                }
            }
        }
        Command::Daemon(DaemonCommand::Tokens) => Report::Tokens {
            tokens: daemon()?.connect()?.tokens(machines.deadline())?,
            now: SystemClock.wall(),
        },
        Command::Daemon(DaemonCommand::Revoke { id }) => Report::Revoked(
            daemon()?
                .connect()?
                .revoke_token(&id, machines.deadline())?,
        ),
        Command::Daemon(DaemonCommand::Run { launchd: flag }) => {
            let signals = signals::StopSignals::block()?;
            let daemon = doppel_daemon::Daemon::new(
                machines,
                Box::new(doppel_daemon::EntropyMacs::new(OsEntropy)),
            );
            let listener = match SocketSource::from_flag(flag) {
                SocketSource::Bind => daemon.listen()?,
                SocketSource::Launchd => daemon.adopt(launchd::Launchd::socket()?)?,
            };
            eprintln!("doppeld: listening on {}", listener.path().display());
            signals.stop(daemon.stopper())?;
            daemon.serve(&listener)?;
            eprintln!("doppeld: stopped");
            Report::Nothing
        }
        Command::Mcp { machine, record } => {
            let default = machine;
            let record = if record {
                doppel_client::Record::IntoStore
            } else {
                doppel_client::Record::Off
            };
            mcp::Relay::connect(&daemon()?, record)?.serve(default)?;
            Report::Nothing
        }
        Command::Sessions {
            action: Some(SessionsCommand::Pause { id }),
        } => {
            daemon()?
                .connect()?
                .pause_session(id, machines.deadline())?;
            Report::SessionPaused(id)
        }
        Command::Sessions {
            action: Some(SessionsCommand::Resume { id }),
        } => {
            daemon()?
                .connect()?
                .resume_session(id, machines.deadline())?;
            Report::SessionResumed(id)
        }
        Command::Sessions { action: None } => {
            Report::Sessions(daemon()?.connect()?.sessions(machines.deadline())?)
        }
        Command::Debug(Debug::AxDump {
            name: raw,
            scope,
            menus,
            max_tokens,
            out,
        }) => {
            use doppel_host::capabilities::Clock as _;
            use doppel_machine::ax::compact::{CompactSnapshot, CompactionPolicy, Tokens};
            use doppel_machine::ax::refs::Generation;
            use doppel_machine::ax::tree::{AxSnapshot, Menus, Scope, SnapshotRequest};
            use prost::Message as _;

            let by = machines.deadline();
            let mut agent = machines.machine(&raw).running(by)?.agent(by)?;
            let scope = match scope {
                DumpScope::Window => Scope::FocusedWindow,
                DumpScope::App => Scope::App(
                    agent
                        .status(by)?
                        .frontmost
                        .context("the guest has no frontmost app")?
                        .pid,
                ),
            };
            let menus = match menus {
                DumpMenus::Exclude => Menus::Exclude,
                DumpMenus::Include => Menus::Include,
            };
            let clock = SystemClock;
            let started = clock.now();
            let wire = agent.ax_snapshot_wire(&SnapshotRequest::new(scope, menus), by)?;
            let walked = clock.now().duration_since(started);
            if let Some(out) = &out {
                std::fs::write(out, wire.encode_to_vec())
                    .with_context(|| format!("writing {}", out.display()))?;
            }
            let snapshot = AxSnapshot::try_from(wire)?;
            let compact = CompactSnapshot::build(
                &snapshot,
                Generation::FIRST,
                CompactionPolicy {
                    budget: Tokens::budget(max_tokens)?,
                    ..CompactionPolicy::default()
                },
            );
            eprintln!(
                "{} AX nodes in {:.0} ms ({}); {} shown, {} omitted, ~{}{}",
                snapshot.tree.len(),
                walked.as_secs_f64() * 1000.0,
                snapshot.truncation,
                compact.shown,
                compact.omitted,
                compact.tokens,
                out.map(|o| format!("; raw snapshot saved to {}", o.display()))
                    .unwrap_or_default()
            );
            Report::Verbatim(compact.text)
        }
        Command::Debug(Debug::VsockEcho {
            name: raw,
            port,
            count,
        }) => {
            let by = machines.deadline();
            let mut stream = machines
                .machine(&raw)
                .running(by)?
                .open_agent_channel(VsockPort::new(port), by)?;
            stream.set_read_timeout(Some(Duration::from_secs(5)))?;
            let clock = SystemClock;
            let mut samples = Vec::new();
            let payload = [7u8; 64];
            for _ in 0..count {
                let started = doppel_host::capabilities::Clock::now(&clock);
                stream.write_all(&(payload.len() as u32).to_le_bytes())?;
                stream.write_all(&payload)?;
                let mut back = [0u8; 68];
                stream.read_exact(&mut back)?;
                if back[4..] != payload {
                    bail!("echo mismatch");
                }
                samples.push(doppel_host::capabilities::Clock::now(&clock).duration_since(started));
            }
            samples.sort();
            let p = |q: f64| {
                samples[((samples.len() as f64 * q) as usize).min(samples.len() - 1)].as_secs_f64()
                    * 1000.0
            };
            Report::Echoed {
                count: samples.len(),
                p50_ms: p(0.5),
                p99_ms: p(0.99),
            }
        }
    })
}
