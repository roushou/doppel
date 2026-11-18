//! A screen's feed in the browser: the daemon's H.264 frames, decoded by
//! the browser (WebCodecs), with only the latest picture left in the view's
//! mailbox; and the person's input sent in order over the same connection.
//! Both run as tasks on the page's one thread. Dropping the view ends both:
//! the reader stops at its next frame, which cancels the stream, and the
//! input task hands control back if the person still had it.

use std::cell::Cell;
use std::rc::Rc;

use doppel_desktop::geometry::Size;
use doppel_machine::machine::MachineName;
use doppel_protocol::pb::daemon::v1 as daemon;
use doppel_scene::encoded::{DecoderConfig, EncodedFrame};
use futures::StreamExt as _;
use futures::channel::mpsc;
use js_sys::Uint8Array;
use wasm_bindgen::prelude::Closure;
use wasm_bindgen_futures::spawn_local;

use super::connection::{Connection, Events};
use super::frame::Frame;
use super::js;
use crate::resync::{Resync, Step};
use crate::screen::{Feed, Mailbox, PumpReplies, Pumped, Update, superseded};
use crate::steering::{SteerEvent, SteerRequest};

/// A running machine's screen, as the daemon streams it.
pub struct Screen {
    pub(crate) connection: Rc<Connection>,
    pub(crate) name: MachineName,
    pub(crate) frames: Events,
    /// The display's size in points: without it, the screen is view-only.
    pub(crate) points: Option<Size>,
}

impl std::fmt::Debug for Screen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Screen")
            .field("name", &self.name)
            .field("points", &self.points)
            .finish_non_exhaustive()
    }
}

/// Starts reading `screen` into `mailbox`, and sending what the view pumps.
pub(crate) fn start(screen: Screen, mailbox: Mailbox, replies: PumpReplies) -> Feed {
    let Screen {
        connection,
        name,
        frames,
        points,
    } = screen;
    spawn_local(read(frames, mailbox));
    let (sender, inputs) = mpsc::unbounded();
    spawn_local(
        InputPump {
            connection,
            name,
            display: points,
            holding: false,
            inputs,
            replies,
        }
        .run(),
    );
    Feed {
        points,
        takes_control: true,
        input: Some(sender),
    }
}

/// The browser's decoder, and the parameter sets it's configured for.
struct Decoder {
    decoder: js::ScreenDecoder,
    configured: Option<Vec<Vec<u8>>>,
    /// Set by the decoder when it fails: it needs configuring again.
    broken: Rc<Cell<bool>>,
    _on_frame: Closure<dyn FnMut(u32, u32, Uint8Array)>,
    _on_error: Closure<dyn FnMut(String)>,
}

impl Decoder {
    fn new(mailbox: &Mailbox) -> Self {
        let broken = Rc::new(Cell::new(false));
        let on_frame = {
            let mut mailbox = mailbox.clone();
            Closure::<dyn FnMut(u32, u32, Uint8Array)>::new(
                move |width: u32, height: u32, pixels: Uint8Array| {
                    if let Some(frame) = Frame::rgba(width, height, pixels.to_vec()) {
                        mailbox.put(Update::Frame(frame));
                    }
                },
            )
        };
        let on_error = {
            let broken = Rc::clone(&broken);
            Closure::<dyn FnMut(String)>::new(move |_: String| broken.set(true))
        };
        Self {
            decoder: js::ScreenDecoder::new(&on_frame, &on_error),
            configured: None,
            broken,
            _on_frame: on_frame,
            _on_error: on_error,
        }
    }

    /// Ready for a stream with these parameter sets; false if the browser
    /// can't decode it.
    fn restart(&mut self, parameter_sets: &[Vec<u8>]) -> bool {
        if !self.broken.get() && self.configured.as_deref() == Some(parameter_sets) {
            return true;
        }
        self.broken.set(false);
        self.configured = None;
        let Some(config) = DecoderConfig::of(parameter_sets) else {
            return false;
        };
        if !self.decoder.configure(&config.codec, &config.description) {
            return false;
        }
        self.configured = Some(parameter_sets.to_vec());
        true
    }

    /// Hands a frame to the decoder; false when it couldn't take it.
    fn decode(&self, frame: &EncodedFrame, key: bool) -> bool {
        !self.broken.get() && self.decoder.decode(&frame.data, key, frame.sequence as f64)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.decoder.close_decoder();
    }
}

/// Decodes frames from a key frame on. A frame that can't be decoded isn't
/// shown, and the screen holds its last picture until the next key frame.
async fn read(mut frames: Events, mut mailbox: Mailbox) {
    let mut decoder = Decoder::new(&mailbox);
    let mut resync = Resync::START;
    while !mailbox.abandoned() {
        let Some(event) = frames.next().await else {
            return mailbox.put(Update::Ended);
        };
        let frame = match event {
            daemon::daemon_event::Kind::EncodedFrame(frame) => {
                match EncodedFrame::try_from(frame) {
                    Ok(frame) => frame,
                    Err(error) => return mailbox.put(Update::Failed(error.to_string())),
                }
            }
            // Not a screen's: passed over.
            daemon::daemon_event::Kind::DisplayFrame(_)
            | daemon::daemon_event::Kind::Activity(_) => continue,
        };
        if decoder.broken.get() {
            resync = resync.failed();
        }
        let (next, step) = resync.next(&frame);
        resync = next;
        let key = match step {
            Step::Restart(parameter_sets) => {
                if !decoder.restart(parameter_sets) {
                    return mailbox.put(Update::Failed(
                        "this browser can't decode the machine's screen".into(),
                    ));
                }
                true
            }
            Step::Decode => false,
            Step::Skip => continue,
        };
        if !decoder.decode(&frame, key) {
            resync = resync.failed();
        }
    }
}

/// Sends one screen's input, and its requests for control, in order. Moves
/// that queue up behind a slow send collapse to the last one.
struct InputPump {
    connection: Rc<Connection>,
    name: MachineName,
    /// The display's size in points: what input is placed on.
    display: Option<Size>,
    /// Whether the person has control, as the daemon last said.
    holding: bool,
    inputs: mpsc::UnboundedReceiver<Pumped>,
    replies: PumpReplies,
}

impl InputPump {
    async fn run(mut self) {
        while let Some(first) = self.inputs.next().await {
            let mut batch = vec![first];
            while let Ok(more) = self.inputs.try_recv() {
                batch.push(more);
            }
            for (index, pumped) in batch.iter().enumerate() {
                match pumped {
                    Pumped::Input(input) => {
                        let next = match batch.get(index + 1) {
                            Some(Pumped::Input(next)) => Some(next),
                            Some(Pumped::Steer(_)) | None => None,
                        };
                        if !superseded(input, next) {
                            let failure = self.send(input.clone()).await.err();
                            self.replies
                                .failed(failure.map(|why| format!("input: {why}")));
                        }
                    }
                    Pumped::Steer(request) => self.steer(*request).await,
                }
            }
        }
        // The view is gone: the connection stays, so control is handed
        // back here rather than by closing it.
        if self.holding {
            let _ = self
                .connection
                .call(daemon::HandBack {
                    name: self.name.to_string(),
                })
                .await;
        }
    }

    async fn send(&self, input: crate::GuestInput) -> Result<(), String> {
        let Some(display) = self.display else {
            return Ok(());
        };
        let input = input.on(display).map_err(|error| error.to_string())?;
        self.connection
            .call(daemon::SendHostInput {
                name: self.name.to_string(),
                input: Some((&input).into()),
            })
            .await
            .map(drop)
            .map_err(|error| error.to_string())
    }

    async fn steer(&mut self, request: SteerRequest) {
        let name = self.name.to_string();
        let outcome = match request {
            SteerRequest::Take => self
                .connection
                .call(daemon::TakeControl { name })
                .await
                .map(|_| self.holding = true),
            SteerRequest::HandBack => self
                .connection
                .call(daemon::HandBack { name })
                .await
                .map(|_| self.holding = false),
        };
        self.replies.answered(match outcome {
            Ok(()) => SteerEvent::Answered,
            Err(error) => SteerEvent::Refused(error.to_string()),
        });
    }
}
