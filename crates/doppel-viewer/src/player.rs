//! Plays a recording's video: AVFoundation's asset reader decodes the MP4
//! into the NV12 pixel buffers the live screens use, on a thread of its
//! own, and hands each frame over when its presentation time comes. The
//! window sends play, pause and seek, and reads where the playhead is.

#![expect(
    unsafe_code,
    reason = "AVFoundation's bindings are unsafe, and CoreVideo buffers cross as raw pointers"
)]

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use core_foundation::base::TCFType;
use core_video::pixel_buffer::{CVPixelBuffer, CVPixelBufferRef};
use futures::channel::mpsc;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2_av_foundation::{
    AVAsset, AVAssetReader, AVAssetReaderTrackOutput, AVAssetTrack, AVMediaTypeVideo,
};
use objc2_core_media::{CMTime, CMTimeRange, kCMTimePositiveInfinity};
use objc2_foundation::{NSDictionary, NSString, NSURL};

use crate::LocalClock;
use crate::decode::Decoder;
use crate::pixels::Buffer;

/// What the window asks of the player.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Command {
    Play,
    Pause,
    /// To this many seconds in, playing on if it was.
    Seek(f64),
}

/// Where the playhead is: moving from `media` seconds at `since`, or held.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Head {
    media: f64,
    since: Instant,
    playing: bool,
}

/// What the player thread leaves for the window.
#[derive(Debug)]
struct Shared {
    head: Head,
    /// The video's length in seconds, once it's open.
    duration: Option<f64>,
    frame: Option<Buffer>,
    /// The video played to its end.
    ended: bool,
    failed: Option<String>,
}

/// A video playing on a thread of its own. Dropping it stops the thread.
pub(crate) struct Player {
    commands: Sender<Command>,
    shared: Arc<Mutex<Shared>>,
    clock: Arc<dyn LocalClock>,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player").finish_non_exhaustive()
    }
}

/// Why a video can't be played.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
enum PlayError {
    #[error("the video has no picture track")]
    NoTrack,
    #[error("the video can't be read: {0}")]
    Reader(String),
}

impl Player {
    /// Opens `path` and shows its frame at `seconds`; `play` starts it.
    /// The window is woken through `wake` as frames come.
    pub(crate) fn open(
        path: PathBuf,
        seconds: f64,
        play: bool,
        clock: Arc<dyn LocalClock>,
        wake: mpsc::Sender<()>,
    ) -> Self {
        let (commands, received) = std::sync::mpsc::channel();
        let shared = Arc::new(Mutex::new(Shared {
            head: Head {
                media: seconds,
                since: clock.now(),
                playing: false,
            },
            duration: None,
            frame: None,
            ended: false,
            failed: None,
        }));
        let thread = Playback {
            path,
            commands: received,
            shared: shared.clone(),
            clock: clock.clone(),
            wake,
        };
        let spawned = std::thread::Builder::new()
            .name("doppel-viewer-player".into())
            .spawn(move || thread.run());
        if spawned.is_err() {
            shared.lock().unwrap_or_else(PoisonError::into_inner).failed =
                Some("no thread to play the video on".into());
        }
        let _ = commands.send(Command::Seek(seconds));
        if play {
            let _ = commands.send(Command::Play);
        }
        Self {
            commands,
            shared,
            clock,
        }
    }

    pub(crate) fn play(&self) {
        let _ = self.commands.send(Command::Play);
    }

    pub(crate) fn pause(&self) {
        let _ = self.commands.send(Command::Pause);
    }

    pub(crate) fn seek(&self, seconds: f64) {
        let _ = self.commands.send(Command::Seek(seconds));
    }

    fn shared(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Where the playhead is now, in seconds.
    pub(crate) fn position(&self) -> f64 {
        let shared = self.shared();
        let head = shared.head;
        let at = if head.playing {
            head.media
                + self
                    .clock
                    .now()
                    .saturating_duration_since(head.since)
                    .as_secs_f64()
        } else {
            head.media
        };
        shared.duration.map_or(at, |duration| at.min(duration))
    }

    pub(crate) fn playing(&self) -> bool {
        self.shared().head.playing
    }

    pub(crate) fn duration(&self) -> Option<f64> {
        self.shared().duration
    }

    pub(crate) fn frame(&self) -> Option<Buffer> {
        self.shared().frame.clone()
    }

    /// Whether it played to its end since last asked.
    pub(crate) fn take_ended(&self) -> bool {
        std::mem::take(&mut self.shared().ended)
    }

    pub(crate) fn failed(&self) -> Option<String> {
        self.shared().failed.clone()
    }
}

/// The player's thread: the asset, and a reader while frames are wanted.
struct Playback {
    path: PathBuf,
    commands: Receiver<Command>,
    shared: Arc<Mutex<Shared>>,
    clock: Arc<dyn LocalClock>,
    wake: mpsc::Sender<()>,
}

/// A reader decoding from a time on, and the media time it was started at
/// against the clock.
struct Reading {
    /// Kept alive while its output is read.
    _reader: Retained<AVAssetReader>,
    output: Retained<AVAssetReaderTrackOutput>,
    /// The frame read and not yet due.
    next: Option<(f64, Buffer)>,
    anchor: (f64, Instant),
}

impl Playback {
    fn run(mut self) {
        let opened = autoreleasepool(|_| Self::open(&self.path));
        let (asset, track, duration) = match opened {
            Ok(opened) => opened,
            Err(error) => return self.fail(error.to_string()),
        };
        self.update(|shared| shared.duration = Some(duration));
        let mut position = 0.0;
        let mut playing = false;
        let mut reading: Option<Reading> = None;
        loop {
            // Waits for a command while paused, or until the next frame
            // is due while playing.
            let wait = match (&mut reading, playing) {
                (Some(reading), true) => {
                    let step = autoreleasepool(|_| Self::step(reading, &self.clock));
                    match step {
                        Step::Show(media, buffer) => {
                            position = media;
                            self.show(buffer);
                            continue;
                        }
                        Step::Wait(left) => Some(left),
                        Step::Done => {
                            playing = false;
                            position = duration;
                            self.update(|shared| {
                                shared.ended = true;
                                shared.head = Head {
                                    media: duration,
                                    since: shared.head.since,
                                    playing: false,
                                };
                            });
                            None
                        }
                    }
                }
                (None, true) => {
                    match autoreleasepool(|_| Self::read(&asset, &track, position, &self.clock)) {
                        Ok(started) => {
                            let anchor = started.anchor;
                            reading = Some(started);
                            self.update(|shared| {
                                shared.head = Head {
                                    media: anchor.0,
                                    since: anchor.1,
                                    playing: true,
                                };
                            });
                            continue;
                        }
                        Err(error) => return self.fail(error.to_string()),
                    }
                }
                (_, false) => None,
            };
            let command = match wait {
                Some(left) => match self.commands.recv_timeout(left) {
                    Ok(command) => command,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => return,
                },
                None => match self.commands.recv() {
                    Ok(command) => command,
                    Err(_) => return,
                },
            };
            match command {
                Command::Play => {
                    if position >= duration - 0.05 {
                        position = 0.0;
                    }
                    playing = true;
                    reading = None;
                }
                Command::Pause => {
                    if let Some(reading) = &reading {
                        let (media, at) = reading.anchor;
                        position = (media
                            + self.clock.now().saturating_duration_since(at).as_secs_f64())
                        .min(duration);
                    }
                    playing = false;
                    reading = None;
                    self.update(|shared| {
                        shared.head = Head {
                            media: position,
                            since: shared.head.since,
                            playing: false,
                        };
                    });
                }
                Command::Seek(seconds) => {
                    position = seconds.clamp(0.0, duration);
                    reading = None;
                    if !playing {
                        // Shows the frame there, held.
                        let shown = autoreleasepool(|_| {
                            Self::read(&asset, &track, position, &self.clock).map(|mut reading| {
                                Self::sample(&reading.output)
                                    .map(|(_, buffer)| buffer)
                                    .or_else(|| reading.next.take().map(|(_, buffer)| buffer))
                            })
                        });
                        match shown {
                            Ok(Some(buffer)) => self.show(buffer),
                            Ok(None) => {}
                            Err(error) => return self.fail(error.to_string()),
                        }
                        self.update(|shared| {
                            shared.head = Head {
                                media: position,
                                since: shared.head.since,
                                playing: false,
                            };
                        });
                    }
                }
            }
        }
    }

    fn open(path: &Path) -> Result<(Retained<AVAsset>, Retained<AVAssetTrack>, f64), PlayError> {
        let path = NSString::from_str(&path.to_string_lossy());
        let url = NSURL::fileURLWithPath(&path);
        // SAFETY: plain AVFoundation calls on objects this thread owns.
        unsafe {
            let asset = AVAsset::assetWithURL(&url);
            let video = AVMediaTypeVideo.ok_or(PlayError::NoTrack)?;
            #[expect(deprecated, reason = "the synchronous load suits a thread of its own")]
            let tracks = asset.tracksWithMediaType(video);
            let track = tracks.firstObject().ok_or(PlayError::NoTrack)?;
            let duration = asset.duration().seconds();
            Ok((
                asset,
                track,
                if duration.is_finite() { duration } else { 0.0 },
            ))
        }
    }

    /// A reader from `seconds` on, anchored to now.
    fn read(
        asset: &AVAsset,
        track: &AVAssetTrack,
        seconds: f64,
        clock: &Arc<dyn LocalClock>,
    ) -> Result<Reading, PlayError> {
        let settings = Decoder::destination();
        // SAFETY: a CFDictionary of CFString keys is toll-free bridged to
        // an NSDictionary of NSString keys, and lives for these calls.
        let settings: &NSDictionary<NSString, AnyObject> =
            unsafe { &*settings.as_concrete_TypeRef().cast() };
        // SAFETY: plain AVFoundation calls on objects this thread owns.
        unsafe {
            let reader = AVAssetReader::assetReaderWithAsset_error(asset)
                .map_err(|error| PlayError::Reader(error.localizedDescription().to_string()))?;
            let output = AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(
                track,
                Some(settings),
            );
            output.setAlwaysCopiesSampleData(false);
            reader.setTimeRange(CMTimeRange {
                start: CMTime::with_seconds(seconds, 600),
                duration: kCMTimePositiveInfinity,
            });
            reader.addOutput(&output);
            if !reader.startReading() {
                let why = reader.error().map_or_else(
                    || "it won't start".into(),
                    |error| error.localizedDescription().to_string(),
                );
                return Err(PlayError::Reader(why));
            }
            Ok(Reading {
                _reader: reader,
                output,
                next: None,
                anchor: (seconds, clock.now()),
            })
        }
    }

    /// The next frame and its presentation time, if there is one.
    fn sample(output: &AVAssetReaderTrackOutput) -> Option<(f64, Buffer)> {
        // SAFETY: plain AVFoundation and CoreMedia calls; a reader track
        // output's image buffer is a CVPixelBuffer, retained by the get
        // rule past the sample's life.
        unsafe {
            loop {
                let sample = output.copyNextSampleBuffer()?;
                let Some(image) = sample.image_buffer() else {
                    continue;
                };
                let pixels = CVPixelBuffer::wrap_under_get_rule(
                    objc2_core_foundation::CFRetained::as_ptr(&image)
                        .as_ptr()
                        .cast::<std::ffi::c_void>() as CVPixelBufferRef,
                );
                let seconds = sample.presentation_time_stamp().seconds();
                return Some((
                    if seconds.is_finite() { seconds } else { 0.0 },
                    Buffer::decoded(pixels),
                ));
            }
        }
    }

    fn step(reading: &mut Reading, clock: &Arc<dyn LocalClock>) -> Step {
        let next = match reading.next.take() {
            Some(next) => next,
            None => match Self::sample(&reading.output) {
                Some(next) => next,
                // Read to its end (or failed: the picture just stops).
                None => return Step::Done,
            },
        };
        let (media, at) = reading.anchor;
        let due = at + Duration::from_secs_f64((next.0 - media).max(0.0));
        let now = clock.now();
        if due <= now {
            Step::Show(next.0, next.1)
        } else {
            let left = due.saturating_duration_since(now);
            reading.next = Some(next);
            Step::Wait(left)
        }
    }

    fn show(&mut self, buffer: Buffer) {
        self.update(|shared| shared.frame = Some(buffer));
    }

    fn fail(&mut self, why: String) {
        self.update(|shared| {
            shared.failed = Some(why);
            shared.head.playing = false;
        });
    }

    fn update(&mut self, change: impl FnOnce(&mut Shared)) {
        change(&mut self.shared.lock().unwrap_or_else(PoisonError::into_inner));
        // Full: the window hasn't looked yet, and will see this.
        let _ = self.wake.try_send(());
    }
}

/// What a playing reader does next.
enum Step {
    Show(f64, Buffer),
    Wait(Duration),
    Done,
}
