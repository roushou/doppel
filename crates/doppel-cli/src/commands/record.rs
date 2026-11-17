//! `record`: a machine's display to a video file, and a recorded
//! session's video with what its agent did drawn over it.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use doppel_host::capabilities::{Clock as _, SystemClock};
use doppel_host::machines::{CALL_TIMEOUT, FINISH_RECORDING_TIMEOUT};
use doppel_host::vmm::RecordingStart;
use doppel_machine::deadline::Deadline;
use doppel_machine::machine::MachineName;

use crate::context::Context;
use crate::daemon_at::{DaemonAt, within};
use crate::report::Report;
use crate::{export, view};

/// Record a machine's display to a video file.
#[derive(Debug, Subcommand)]
pub(crate) enum RecordCommand {
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

impl RecordCommand {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        match self {
            Self::Start { name, file } => start(ctx, name, &file),
            Self::Stop { name } => {
                let by = Deadline::after(SystemClock.now(), FINISH_RECORDING_TIMEOUT);
                let summary = ctx
                    .machines()
                    .machine(&name)
                    .running(by)?
                    .stop_recording(None, by)?;
                Ok(Report::RecordingStopped { name, summary })
            }
            Self::Export {
                recording,
                out,
                video,
                fps,
            } => export(ctx, &recording, &out, video.as_deref(), fps),
        }
    }
}

fn start(ctx: &Context, name: MachineName, file: &std::path::Path) -> Result<Report> {
    let machines = ctx.machines();
    let path = std::path::absolute(file)
        .with_context(|| format!("{} has no absolute path", file.display()))?;
    let by = machines.deadline();
    match machines
        .machine(&name)
        .running(by)?
        .start_recording(&path, by)?
    {
        RecordingStart::Started => Ok(Report::RecordingStarted { name, path }),
        RecordingStart::AlreadyRecording(running) => {
            bail!("{name} is already recording to {}", running.display())
        }
    }
}

fn export(
    ctx: &Context,
    recording: &doppel_client::RecordingId,
    out: &std::path::Path,
    video: Option<&str>,
    fps: u32,
) -> Result<Report> {
    let at = ctx.daemon()?;
    if matches!(at, DaemonAt::Forwarded(_)) {
        bail!("its video is on the Mac the daemon runs on: export it there");
    }
    let client = at.connect()?;
    let opened = doppel_scene::recordings::Recording::of(
        client.open_recording(recording, within(CALL_TIMEOUT))?,
    );
    let source = |video: &doppel_scene::recordings::Video| {
        view::Videos::Here
            .path(
                || {
                    client
                        .data_root(within(CALL_TIMEOUT))
                        .map_err(|error| doppel_viewer::DaemonError::Unreachable(error.to_string()))
                },
                &opened.id,
                &video.file,
            )
            .map_err(|error| anyhow::anyhow!("{error}"))
    };
    let snapshot = |line| {
        client
            .seen_snapshot(recording, line, within(CALL_TIMEOUT))
            .ok()
    };
    let exported = export::export(&opened, video, source, out, fps, ctx.vmm(), snapshot)?;
    Ok(Report::Exported {
        path: exported.path,
        summary: exported.summary,
    })
}
