//! `doppel record export`: a recorded session's video with what its agent
//! did burned in. The scene decides what is drawn when ([`Reel`]); the
//! VMM binary paints it on the frames and encodes them.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use doppel_machine::ax::tree::AxSnapshot;
use doppel_machine::geometry::Size;
use doppel_scene::film::{POINTER_MS, Reel};
use doppel_scene::recordings::{Recording, Video};
use doppel_scene::timeline::{Seen, from_trace};
use prost::Message as _;

use crate::reel;

/// What an export made.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Exported {
    pub(crate) path: PathBuf,
    /// What the VMM said of it.
    pub(crate) summary: String,
}

/// The video to export: the one named, or the recording's first.
fn chosen<'a>(recording: &'a Recording, named: Option<&str>) -> Result<&'a Video> {
    let listed = || {
        recording
            .videos
            .iter()
            .map(|video| video.file.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match named {
        Some(name) => recording
            .videos
            .iter()
            .find(|video| video.file.as_str() == name)
            .with_context(|| format!("{name} isn't one of its videos ({})", listed())),
        None => recording
            .videos
            .first()
            .context("it has no video: the session touched no machine"),
    }
}

/// The reel for `video` of `recording`, the display it's drawn over, and
/// when the video started in the session's time:
/// its machine's calls, from the video's start until the last call's
/// pointer has faded. `snapshot` reads the snapshot a trace line kept, for
/// what the agent took in; one it can't read is left out.
pub(crate) fn reel_of(
    recording: &Recording,
    video: &Video,
    fps: u32,
    snapshot: impl Fn(u32) -> Option<AxSnapshot>,
) -> Result<(Reel, Size, f64)> {
    let Some(display) = video.points else {
        bail!(
            "{} didn't keep its display's size, so its calls can't be placed on it",
            video.file.as_str()
        );
    };
    let Some(from) = video.start_ms else {
        bail!(
            "{} didn't keep when it started, so its calls can't be lined up with it",
            video.file.as_str()
        );
    };
    let mut moments = from_trace(recording, &video.machine);
    for moment in &mut moments {
        if let Some(Seen::Recorded(line)) = moment.seen {
            moment.seen = snapshot(line).map(|kept| Seen::Live(Arc::new(kept)));
        }
    }
    #[expect(clippy::cast_precision_loss, reason = "milliseconds of a recording")]
    let until = recording.traced_ms() as f64 + POINTER_MS;
    #[expect(clippy::cast_precision_loss, reason = "milliseconds of a recording")]
    let from = from as f64;
    Ok((
        Reel::shoot(&moments, display, from, until, fps),
        display,
        from,
    ))
}

/// Writes `video` (at `source`) with `recording`'s overlay to `out`, by
/// way of the VMM binary at `vmm`.
pub(crate) fn export(
    recording: &Recording,
    named: Option<&str>,
    source: impl FnOnce(&Video) -> Result<PathBuf>,
    out: &Path,
    fps: u32,
    vmm: &Path,
    snapshot: impl Fn(u32) -> Option<AxSnapshot>,
) -> Result<Exported> {
    let video = chosen(recording, named)?;
    let (shot, display, video_from) = reel_of(recording, video, fps, snapshot)?;
    let film = reel::to_wire(&shot, display, video_from);
    let path = std::path::absolute(out)
        .with_context(|| format!("{} has no absolute path", out.display()))?;
    if path.exists() {
        bail!(
            "{} exists already: pick another name, or remove it",
            path.display()
        );
    }
    // Beside the export, so it's on the same disk, and gone once it's made.
    let reel_path = path.with_extension("reel.pb");
    std::fs::write(&reel_path, film.encode_to_vec())
        .with_context(|| format!("can't write {}", reel_path.display()))?;
    let ran = Command::new(vmm)
        .arg("export")
        .arg("--video")
        .arg(source(video)?)
        .arg("--reel")
        .arg(&reel_path)
        .arg("--out")
        .arg(&path)
        .output();
    let _ = std::fs::remove_file(&reel_path);
    let ran = ran.with_context(|| format!("can't run {}", vmm.display()))?;
    if !ran.status.success() {
        bail!(
            "the export failed: {}",
            String::from_utf8_lossy(&ran.stderr).trim()
        );
    }
    Ok(Exported {
        path,
        summary: String::from_utf8_lossy(&ran.stdout).trim().to_owned(),
    })
}
