//! `doppel record export`: a recorded session's video with what its agent
//! did burned in. The scene decides what is drawn when ([`Reel`]); the
//! VMM binary paints it on the frames and encodes them.

mod reel;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use doppel_desktop::ax::tree::AxSnapshot;
use doppel_desktop::geometry::Size;
use doppel_scene::film::{POINTER_MS, Reel};
use doppel_scene::recordings::{Recording, Video};
use doppel_scene::timeline::{Seen, Timeline};
use prost::Message as _;

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

/// The machine's run that `video` is part of: every video of its machine,
/// in the order they started, each with when it started in the session's
/// time. A rollback or a restart splits a run into several.
fn run_of<'a>(recording: &'a Recording, video: &Video) -> Result<Vec<(&'a Video, f64)>> {
    let mut run = recording
        .videos
        .iter()
        .filter(|other| other.machine == video.machine)
        .map(|other| {
            let Some(start) = other.start_ms else {
                bail!(
                    "{} didn't keep when it started, so its calls can't be lined up with it",
                    other.file.as_str()
                );
            };
            #[expect(clippy::cast_precision_loss, reason = "milliseconds of a recording")]
            Ok((other, start as f64))
        })
        .collect::<Result<Vec<_>>>()?;
    run.sort_by(|a, b| a.1.total_cmp(&b.1));
    Ok(run)
}

/// The reel for the run of `recording`'s machine starting at `from`, on a
/// display of `display`: its calls, from `from` until the last call's
/// pointer has faded. `snapshot` reads the snapshot a trace line kept, for
/// what the agent took in; one it can't read is left out.
pub(crate) fn reel_of(
    recording: &Recording,
    video: &Video,
    from: f64,
    fps: u32,
    snapshot: impl Fn(u32) -> Option<AxSnapshot>,
) -> Result<(Reel, Size)> {
    let Some(display) = video.points else {
        bail!(
            "{} didn't keep its display's size, so its calls can't be placed on it",
            video.file.as_str()
        );
    };
    let mut timeline = Timeline::from_trace(recording, &video.machine);
    timeline.fill_seen(|line| snapshot(line).map(|kept| Seen::Live(Arc::new(kept))));
    #[expect(clippy::cast_precision_loss, reason = "milliseconds of a recording")]
    let until = recording.traced_ms() as f64 + POINTER_MS;
    Ok((
        Reel::shoot(timeline.moments(), display, from, until, fps),
        display,
    ))
}

/// Writes the run `named` is part of (each video at `source`), with
/// `recording`'s overlay, to `out`, by way of the VMM binary at `vmm`.
pub(crate) fn export(
    recording: &Recording,
    named: Option<&str>,
    source: impl Fn(&Video) -> Result<PathBuf>,
    out: &Path,
    fps: u32,
    vmm: &Path,
    snapshot: impl Fn(u32) -> Option<AxSnapshot>,
) -> Result<Exported> {
    let video = chosen(recording, named)?;
    let run = run_of(recording, video)?;
    let from = run.first().map_or(0.0, |(_, start)| *start);
    let (shot, display) = reel_of(recording, video, from, fps, snapshot)?;
    let sources = run
        .iter()
        .map(|(video, start)| Ok((source(video)?, *start)))
        .collect::<Result<Vec<_>>>()?;
    let film = reel::to_wire(&shot, display, &sources);
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

#[cfg(test)]
mod tests {
    use doppel_scene::recordings::{RecordingId, VideoName};

    use super::*;

    fn video(file: &str, machine: &str, start_ms: Option<u64>) -> Video {
        Video {
            file: VideoName::new(file.into()),
            machine: machine.parse().unwrap(),
            start_ms,
            points: Size::new(1280.0, 800.0).ok(),
        }
    }

    fn recording(videos: Vec<Video>) -> Recording {
        Recording {
            id: RecordingId::new("r".into()),
            videos,
            trace: Vec::new(),
        }
    }

    #[test]
    fn a_run_is_every_video_of_its_machine_in_order() {
        let recording = recording(vec![
            video("live-002.mp4", "live", Some(54_020)),
            video("dev-001.mp4", "dev", Some(100)),
            video("live-001.mp4", "live", Some(339)),
        ]);
        let run = run_of(&recording, &recording.videos[0]).unwrap();
        let files: Vec<(&str, f64)> = run
            .iter()
            .map(|(video, start)| (video.file.as_str(), *start))
            .collect();
        assert_eq!(files, [("live-001.mp4", 339.0), ("live-002.mp4", 54_020.0)]);
    }

    #[test]
    fn a_run_needs_to_know_when_each_video_started() {
        let recording = recording(vec![
            video("live-001.mp4", "live", Some(339)),
            video("live-002.mp4", "live", None),
        ]);
        assert!(run_of(&recording, &recording.videos[0]).is_err());
    }
}
