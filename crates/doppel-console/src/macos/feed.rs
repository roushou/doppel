//! A screen's feed on the desktop: its source read on a thread of its own,
//! which converts frames to pixel buffers and leaves only the latest in the
//! view's mailbox, and its input sent from another. Dropping the view ends
//! both: the reader stops at its next frame, and the input thread once its
//! queue is empty, which closes the screen's connection and hands control
//! back.

use futures::StreamExt as _;
use futures::channel::mpsc;
use futures::executor::block_on;

use super::decode::Decoder;
use super::pixels::Pixels as PixelPool;
use super::ports::{
    EncodedSource, FrameSource, Frames, InputSink, Next, NextEncoded, Screen, Takeover,
};
use crate::resync::{Resync, Step};
use crate::screen::{Control, Feed, Mailbox, PumpReplies, Pumped, Update, superseded};
use crate::steering::{SteerEvent, SteerRequest};

/// Starts reading `screen`'s source into `mailbox`, and, when it takes
/// input, sending what the view pumps.
pub(crate) fn start(screen: Screen, mailbox: Mailbox, replies: PumpReplies) -> Feed {
    Reader {
        source: screen.source,
        mailbox,
    }
    .spawn();
    let takes_control = screen.takeover.is_some();
    let input = (screen.input.is_some() || takes_control).then(|| {
        let (sender, inputs) = mpsc::unbounded();
        InputPump {
            sink: screen.input,
            takeover: screen.takeover,
            inputs,
            replies,
        }
        .spawn();
        sender
    });
    Feed {
        points: screen.points,
        control: match input {
            None => Control::ViewOnly,
            Some(sender) if takes_control => Control::Steered(sender),
            Some(sender) => Control::Direct(sender),
        },
    }
}

/// Reads one source until it ends, fails, or its view goes away.
struct Reader {
    source: Frames,
    mailbox: Mailbox,
}

impl Reader {
    fn spawn(self) {
        std::thread::Builder::new()
            .name("doppel-console-reader".into())
            .spawn(move || self.run())
            .map(drop)
            .unwrap_or_else(|_| {
                // No thread for this screen: its view keeps waiting.
            });
    }

    fn run(self) {
        match self.source {
            Frames::Raw(source) => Self::read_raw(source, self.mailbox),
            Frames::Encoded(source) => Self::read_encoded(source, self.mailbox),
        }
    }

    fn read_raw(mut source: Box<dyn FrameSource>, mut mailbox: Mailbox) {
        // Made here, on the reader's thread: the pool stays on it.
        let mut pool: Option<PixelPool> = None;
        while !mailbox.abandoned() {
            let update = match source.next_frame() {
                Ok(Next::Frame(frame)) => {
                    let size = (frame.width(), frame.height());
                    let pixels = match pool.take() {
                        Some(pixels) if pixels.size() == size => Ok(pixels),
                        Some(_) | None => PixelPool::new(size.0, size.1),
                    };
                    match pixels.and_then(|pixels| {
                        let buffer = pixels.fill(frame);
                        pool = Some(pixels);
                        buffer
                    }) {
                        Ok(buffer) => Update::Frame(buffer),
                        Err(error) => Update::Failed(error.to_string()),
                    }
                }
                Ok(Next::Unchanged) => continue,
                Ok(Next::Ended) => Update::Ended,
                Err(error) => Update::Failed(error.to_string()),
            };
            let last = !matches!(update, Update::Frame(_));
            mailbox.put(update);
            if last {
                return;
            }
        }
    }

    /// Decodes frames from a key frame on. A frame that fails to decode
    /// isn't shown, and the screen holds its last frame until the next key
    /// frame; one that can't get a decoder ends the screen with why.
    fn read_encoded(mut source: Box<dyn EncodedSource>, mut mailbox: Mailbox) {
        // Made here, on the reader's thread: the decoder stays on it.
        let mut decoder: Option<Decoder> = None;
        let mut resync = Resync::START;
        while !mailbox.abandoned() {
            let frame = match source.next_frame() {
                Ok(NextEncoded::Frame(frame)) => frame,
                Ok(NextEncoded::Unchanged) => continue,
                Ok(NextEncoded::Ended) => return mailbox.put(Update::Ended),
                Err(error) => return mailbox.put(Update::Failed(error.to_string())),
            };
            let (next, step) = resync.next(&frame);
            resync = next;
            match step {
                Step::Restart(parameter_sets) => {
                    if !decoder
                        .as_ref()
                        .is_some_and(|decoder| decoder.decodes(parameter_sets))
                    {
                        // The old session goes before the new one is made.
                        drop(decoder.take());
                        match Decoder::new(parameter_sets) {
                            Ok(made) => decoder = Some(made),
                            Err(error) => return mailbox.put(Update::Failed(error.to_string())),
                        }
                    }
                }
                Step::Decode => {}
                Step::Skip => continue,
            }
            let Some(decoder) = decoder.as_mut() else {
                resync = resync.failed();
                continue;
            };
            match decoder.decode(&frame.data) {
                Ok(Some(buffer)) => mailbox.put(Update::Frame(buffer)),
                Ok(None) => {}
                Err(_) => resync = resync.failed(),
            }
        }
    }
}

/// Sends one screen's input, and its requests for control, from a thread of
/// its own and in order, so a slow guest never stalls the window. Moves that
/// queue up behind a slow send collapse to the last one.
struct InputPump {
    sink: Option<Box<dyn InputSink>>,
    takeover: Option<Box<dyn Takeover>>,
    inputs: mpsc::UnboundedReceiver<Pumped>,
    replies: PumpReplies,
}

impl InputPump {
    fn spawn(self) {
        // No thread: the screen stays view-only (its sender's sends fail).
        let _ = std::thread::Builder::new()
            .name("doppel-console-input".into())
            .spawn(move || self.run());
    }

    fn run(mut self) {
        while let Some(first) = block_on(self.inputs.next()) {
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
                        if superseded(input, next) {
                            continue;
                        }
                        let Some(sink) = self.sink.as_mut() else {
                            continue;
                        };
                        let outcome = sink.send(input.clone());
                        self.replies
                            .failed(outcome.err().map(|error| format!("input: {error}")));
                    }
                    Pumped::Steer(request) => self.steer(*request),
                }
            }
        }
    }

    fn steer(&mut self, request: SteerRequest) {
        let outcome = match (self.takeover.as_mut(), request) {
            (Some(takeover), SteerRequest::Take) => takeover.take(),
            (Some(takeover), SteerRequest::HandBack) => takeover.hand_back(),
            // Views only steer screens that have a takeover.
            (None, SteerRequest::Take | SteerRequest::HandBack) => Ok(()),
        };
        self.replies.answered(match outcome {
            Ok(()) => SteerEvent::Answered,
            Err(error) => SteerEvent::Refused(error.to_string()),
        });
    }
}
