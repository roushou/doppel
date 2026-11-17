//! A machine's film in the browser viewer, as WebAssembly: the shapes an
//! export burns in (`doppel_scene::film`), with the captions a viewer adds
//! beside each agent's pointer, so the browser, the desktop viewer and an
//! exported film draw the same.
//!
//! A [`Film`] is one machine's timeline. Live, it takes in the daemon's
//! activity events as they come; in replay, a recording's trace, and each
//! snapshot the page fetches for it. Each frame it answers a
//! `doppel.film.v1.Frame`, which the page draws over the screen. Bytes in,
//! bytes out: the page speaks protobuf, as it does with the daemon.
//! `exports` is the WebAssembly face of it.

use std::sync::Arc;

use doppel_machine::ax::tree::AxSnapshot;
use doppel_machine::geometry::Size;
use doppel_machine::machine::MachineName;
use doppel_protocol::convert::ParseError;
use doppel_protocol::pb::agent::v1 as agent;
use doppel_protocol::pb::daemon::v1 as daemon;
use doppel_protocol::pb::film::v1 as film;
use doppel_protocol::pb::vmm::v1 as vmm;
use doppel_scene::film::{Caption, Picture};
use doppel_scene::recordings::Recording;
use doppel_scene::timeline::{ActivityEvent, Moment, Seen, from_trace, record};
use prost::Message as _;

mod exports;

/// Why the film couldn't take something in, or draw.
#[derive(Debug, thiserror::Error)]
pub enum FilmError {
    #[error("unreadable protobuf: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("unreadable: {0}")]
    Parse(#[from] ParseError),
    #[error("{0:?} isn't a machine's name")]
    NotAMachine(String),
    #[error("a display without area: {width} × {height}")]
    Display { width: f64, height: f64 },
}

/// One machine's calls, on the page's clock live or the session's in
/// replay, and what is drawn over its screen at any instant.
#[derive(Debug, Default)]
pub struct Film {
    moments: Vec<Moment>,
}

impl Film {
    /// A live machine's film, before anything has happened on it.
    #[must_use]
    pub fn live() -> Self {
        Self::default()
    }

    /// The film of `machine`'s calls in a recording, from its
    /// `doppel.daemon.v1.Recording` bytes.
    ///
    /// # Errors
    /// The bytes aren't a recording, or `machine` isn't a name.
    pub fn replay(recording: &[u8], machine: &str) -> Result<Self, FilmError> {
        let machine: MachineName = machine
            .parse()
            .map_err(|_| FilmError::NotAMachine(machine.to_owned()))?;
        let recording = daemon::Recording::decode(recording)?;
        let recording = Recording::of(recording.try_into()?);
        Ok(Self {
            moments: from_trace(&recording, &machine),
        })
    }

    /// Takes in a `doppel.daemon.v1.Activity` the daemon sent, at `now` on
    /// the page's clock. The page sends a machine's film only its events.
    ///
    /// # Errors
    /// The bytes aren't an event this build can read; the film is as it was.
    pub fn told(&mut self, activity: &[u8], now: f64) -> Result<(), FilmError> {
        let activity = daemon::Activity::decode(activity)?;
        record(
            &mut self.moments,
            ActivityEvent::of(activity.try_into()?),
            now,
        );
        Ok(())
    }

    /// Puts the snapshot a recorded call saw in place, from its trace line
    /// and its `doppel.agent.v1.AxSnapshot` bytes, so the film shows what
    /// the agent took in.
    ///
    /// # Errors
    /// The bytes aren't a snapshot this build can read.
    pub fn seen(&mut self, line: u32, snapshot: &[u8]) -> Result<(), FilmError> {
        let snapshot = Arc::new(AxSnapshot::try_from(agent::AxSnapshot::decode(snapshot)?)?);
        for moment in &mut self.moments {
            if moment.seen == Some(Seen::Recorded(line)) {
                moment.seen = Some(Seen::Live(Arc::clone(&snapshot)));
            }
        }
        Ok(())
    }

    /// What goes over a display of `width` × `height` points at `now`.
    ///
    /// # Errors
    /// The display has no area.
    pub fn frame(&self, width: f64, height: f64, now: f64) -> Result<film::Frame, FilmError> {
        let display = Size::new(width, height)
            .ok()
            .filter(|size| size.width() > 0.0 && size.height() > 0.0)
            .ok_or(FilmError::Display { width, height })?;
        Ok(film::Frame {
            picture: Some(vmm::Picture::from(&Picture::at(
                &self.moments,
                display,
                now,
            ))),
            captions: Caption::at(&self.moments, now)
                .iter()
                .map(Into::into)
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use doppel_protocol::pb::common::v1 as common;

    use super::*;

    fn click(request: u64, at: (f64, f64), why: &str) -> Vec<u8> {
        daemon::Activity {
            session: 1,
            request,
            machine: "live".into(),
            client: "claude".into(),
            caption: "click".into(),
            phase: Some(daemon::activity::Phase::Started(daemon::CallStarted {
                why: why.into(),
                gesture: daemon::Gesture::Click as i32,
                target: Some(daemon::call_started::Target::Point(common::Point {
                    x: at.0,
                    y: at.1,
                })),
                mark: None,
            })),
        }
        .encode_to_vec()
    }

    fn pointer(frame: &film::Frame) -> Option<vmm::Spot> {
        frame
            .picture
            .as_ref()?
            .shapes
            .iter()
            .find_map(|shape| match &shape.kind {
                Some(vmm::shape::Kind::Pointer(pointer)) => pointer.tip,
                _ => None,
            })
    }

    #[test]
    fn a_live_call_draws_the_pointer_and_its_caption() {
        let mut film = Film::live();
        film.told(&click(1, (200.0, 100.0), "open the menu"), 1000.0)
            .unwrap();
        let frame = film.frame(1280.0, 800.0, 2000.0).unwrap();
        assert_eq!(pointer(&frame), Some(vmm::Spot { x: 200.0, y: 100.0 }));
        let [caption] = frame.captions.try_into().unwrap();
        assert_eq!(caption.why.as_deref(), Some("open the menu"));
        assert_eq!(caption.tip, Some(vmm::Spot { x: 200.0, y: 100.0 }));
    }

    #[test]
    fn what_cant_be_read_is_refused_and_leaves_the_film_as_it_was() {
        let mut film = Film::live();
        assert!(matches!(
            film.told(&[0xff, 0xff], 0.0),
            Err(FilmError::Decode(_))
        ));
        let no_phase = daemon::Activity {
            machine: "live".into(),
            ..daemon::Activity::default()
        };
        assert!(matches!(
            film.told(&no_phase.encode_to_vec(), 0.0),
            Err(FilmError::Parse(_))
        ));
        assert!(film.moments.is_empty());
        assert!(matches!(
            film.frame(0.0, 800.0, 0.0),
            Err(FilmError::Display { .. })
        ));
    }

    #[test]
    fn a_replay_plays_its_machines_calls_from_the_trace() {
        let call = |machine: &str, start_ms| daemon::TraceLine {
            line: Some(daemon::trace_line::Line::Call(daemon::TraceCall {
                start_ms,
                end_ms: start_ms + 500,
                machine: Some(machine.into()),
                call: "click".into(),
                outcome: daemon::TraceOutcome::Done as i32,
                by: daemon::TraceActor::Session as i32,
                acted: Some(daemon::CallStarted {
                    why: String::new(),
                    gesture: daemon::Gesture::Click as i32,
                    target: Some(daemon::call_started::Target::Point(common::Point {
                        x: 50.0,
                        y: 60.0,
                    })),
                    mark: None,
                }),
                ..daemon::TraceCall::default()
            })),
        };
        let recording = daemon::Recording {
            id: "2026-11-14T06-15-42Z-s1".into(),
            videos: Vec::new(),
            trace: vec![call("live", 100), call("other", 200)],
        }
        .encode_to_vec();
        let film = Film::replay(&recording, "live").unwrap();
        assert_eq!(film.moments.len(), 1);
        let frame = film.frame(1280.0, 800.0, 300.0).unwrap();
        assert_eq!(pointer(&frame), Some(vmm::Spot { x: 50.0, y: 60.0 }));
        assert!(matches!(
            Film::replay(&recording, "Not A Name"),
            Err(FilmError::NotAMachine(_))
        ));
    }
}
