//! A recording's video in the browser: a `<video>` the page never shows
//! plays it, and each of its frames comes back as a picture for the window.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use futures::channel::mpsc;
use gpui::App;
use js_sys::Uint8Array;
use wasm_bindgen::prelude::Closure;

use super::frame::Frame;
use super::js;
use crate::LocalClock;

#[derive(Debug, Default)]
struct Shared {
    frame: Option<Frame>,
    /// Pictures replaced since the window last released them.
    stale: Vec<Frame>,
    failed: Option<String>,
}

/// A video playing. Dropping it stops it.
pub(crate) struct Player {
    video: js::VideoPlayer,
    shared: Rc<RefCell<Shared>>,
    _on_frame: Closure<dyn FnMut(u32, u32, Uint8Array)>,
    _on_ended: Closure<dyn FnMut()>,
    _on_error: Closure<dyn FnMut(String)>,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player").finish_non_exhaustive()
    }
}

impl Player {
    /// Opens the video at `url` and shows its frame at `seconds`; `play`
    /// starts it. The window is woken through `wake` as frames come.
    pub(crate) fn open(
        url: String,
        seconds: f64,
        play: bool,
        _clock: Arc<dyn LocalClock>,
        wake: mpsc::Sender<()>,
    ) -> Self {
        let shared = Rc::new(RefCell::new(Shared::default()));
        let woken = move |wake: &mut mpsc::Sender<()>| {
            // Full: the window hasn't looked yet, and will see this.
            let _ = wake.try_send(());
        };
        let on_frame = {
            let (shared, mut wake) = (Rc::clone(&shared), wake.clone());
            Closure::<dyn FnMut(u32, u32, Uint8Array)>::new(
                move |width: u32, height: u32, pixels: Uint8Array| {
                    if let Some(frame) = Frame::rgba(width, height, pixels.to_vec()) {
                        let mut shared = shared.borrow_mut();
                        if let Some(old) = shared.frame.replace(frame) {
                            shared.stale.push(old);
                        }
                        drop(shared);
                        woken(&mut wake);
                    }
                },
            )
        };
        let on_ended = {
            let mut wake = wake.clone();
            Closure::<dyn FnMut()>::new(move || woken(&mut wake))
        };
        let on_error = {
            let (shared, mut wake) = (Rc::clone(&shared), wake);
            Closure::<dyn FnMut(String)>::new(move |why: String| {
                shared.borrow_mut().failed = Some(why);
                woken(&mut wake);
            })
        };
        let video = js::VideoPlayer::new(&url, &on_frame, &on_ended, &on_error);
        video.seek(seconds);
        if play {
            video.play();
        }
        Self {
            video,
            shared,
            _on_frame: on_frame,
            _on_ended: on_ended,
            _on_error: on_error,
        }
    }

    pub(crate) fn play(&self) {
        self.video.play();
    }

    pub(crate) fn pause(&self) {
        self.video.pause();
    }

    pub(crate) fn seek(&self, seconds: f64) {
        self.video.seek(seconds);
    }

    /// Where the playhead is now, in seconds.
    pub(crate) fn position(&self) -> f64 {
        self.video.position()
    }

    pub(crate) fn playing(&self) -> bool {
        self.video.playing()
    }

    pub(crate) fn duration(&self) -> Option<f64> {
        let duration = self.video.duration();
        (duration >= 0.0).then_some(duration)
    }

    pub(crate) fn frame(&self) -> Option<Frame> {
        self.shared.borrow().frame.clone()
    }

    pub(crate) fn failed(&self) -> Option<String> {
        self.shared.borrow().failed.clone()
    }

    /// Frees the pictures replaced since last time.
    pub(crate) fn release_stale(&self, cx: &mut App) {
        let stale = std::mem::take(&mut self.shared.borrow_mut().stale);
        for frame in stale {
            frame.release(cx);
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.video.close_player();
    }
}
