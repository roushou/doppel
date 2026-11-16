//! `doppel view`: the desktop window, over the daemon. This is the one place
//! that knows both sides: the daemon's display connections, doppel-host's
//! shared frames and the client's host input on one, the viewer's frame
//! sources and input sink on the other; `port` maps the rest of the
//! viewer's `Daemon` port.

use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use doppel_client::{Client, ClientError, Display, DisplayLayout, EncodedDisplay};
use doppel_host::display::{Announcement, DisplayError, Nv12Frame, SharedFrames};
use doppel_machine::geometry::{GeometryError, Point, Size};
use doppel_machine::input::{
    Button, ClickCount, Input, Key, KeyChord, Modifier, NamedKey, Pointer, PointerPhase,
};
use doppel_machine::machine::MachineName;
use doppel_protocol::display::{RingLayout, SlotRead};
use doppel_viewer::{
    EncodedSource, FrameRef, FrameSource, GuestInput, InputSink, Next, NextEncoded, Position,
    Screen, SourceError, Takeover, Viewer,
};

use doppel_host::capabilities::SystemClock;

use crate::daemon_at::within;

#[cfg(test)]
mod fixtures;
mod port;
mod recordings;

pub(crate) use recordings::Videos;

/// Why a machine's display can't be shown.
#[derive(Debug, thiserror::Error)]
enum ViewError {
    #[error(transparent)]
    Daemon(#[from] ClientError),
    #[error(transparent)]
    Display(#[from] DisplayError),
}

/// A machine's display, announced by the daemon and read from the memory
/// its VMM shares, as a viewer source.
struct DisplaySource {
    display: Display<SystemClock>,
    frames: SharedFrames,
    frame: Nv12Frame,
}

impl DisplaySource {
    /// How long a source waits for a frame before saying nothing changed.
    const WAIT: Duration = Duration::from_secs(1);

    fn open(socket: &Path, name: &MachineName) -> Result<Self, ViewError> {
        let (display, layout, memory) =
            Display::open(socket, SystemClock, name, within(Self::WAIT * 10))?;
        Ok(Self {
            display,
            frames: Self::map(layout, &memory)?,
            frame: Nv12Frame::default(),
        })
    }

    fn map(layout: DisplayLayout, memory: &OwnedFd) -> Result<SharedFrames, DisplayError> {
        let ring = RingLayout {
            width_pixels: layout.width_pixels,
            height_pixels: layout.height_pixels,
            slot_count: layout.slot_count,
            slot_stride: layout.slot_stride,
            luma_offset: layout.luma_offset,
            luma_bytes_per_row: layout.luma_bytes_per_row,
            chroma_offset: layout.chroma_offset,
            chroma_bytes_per_row: layout.chroma_bytes_per_row,
        };
        SharedFrames::map(ring, layout.points, memory)
    }
}

impl FrameSource for DisplaySource {
    fn next_frame(&mut self) -> Result<Next<'_>, SourceError> {
        loop {
            let announced = match self.display.next_frame(within(Self::WAIT)) {
                Ok(announced) => Announcement {
                    slot: announced.slot,
                    sequence: announced.sequence,
                },
                Err(ClientError::Timeout) => return Ok(Next::Unchanged),
                // The daemon ends the connection when the VM stops.
                Err(ClientError::Closed) => return Ok(Next::Ended),
                Err(error) => return Err(SourceError::new(error)),
            };
            match self.frames.read(announced, &mut self.frame) {
                Ok(SlotRead::Whole) => break,
                // Reused while read: a newer frame is coming.
                Ok(SlotRead::Torn) => {}
                Err(error) => return Err(SourceError::new(error)),
            }
        }
        FrameRef::new(
            self.frame.width(),
            self.frame.height(),
            self.frame.luma(),
            self.frame.chroma(),
        )
        .map(Next::Frame)
        .map_err(SourceError::new)
    }
}

/// A machine's display as the daemon streams it, H.264-encoded by its VMM,
/// as a viewer source: the viewer decodes it.
struct StreamSource {
    display: EncodedDisplay<SystemClock>,
}

impl StreamSource {
    fn open(socket: &Path, name: &MachineName) -> Result<(Self, Option<Size>), ViewError> {
        let (display, size) =
            EncodedDisplay::open(socket, SystemClock, name, within(DisplaySource::WAIT * 10))?;
        Ok((Self { display }, size.points))
    }

    fn frame(frame: doppel_client::EncodedFrame) -> doppel_viewer::EncodedFrame {
        doppel_viewer::EncodedFrame {
            sequence: frame.sequence,
            kind: match frame.kind {
                doppel_client::FrameKind::Key { parameter_sets } => {
                    doppel_viewer::FrameKind::Key { parameter_sets }
                }
                doppel_client::FrameKind::Delta => doppel_viewer::FrameKind::Delta,
            },
            data: frame.data,
        }
    }
}

impl EncodedSource for StreamSource {
    fn next_frame(&mut self) -> Result<NextEncoded, SourceError> {
        match self.display.next_frame(within(DisplaySource::WAIT)) {
            Ok(frame) => Ok(NextEncoded::Frame(Self::frame(frame))),
            Err(ClientError::Timeout) => Ok(NextEncoded::Unchanged),
            // The daemon ends the connection when the VM stops.
            Err(ClientError::Closed) => Ok(NextEncoded::Ended),
            Err(error) => Err(SourceError::new(error)),
        }
    }
}

/// A machine's host input, the VMM's virtual keyboard and pointer reached
/// through the daemon, as a viewer sink. Works before login and without the
/// guest agent, and doesn't wait for the machine's queue; the daemon takes
/// it only once this connection has control.
struct HostInput {
    client: Arc<Client<SystemClock>>,
    name: MachineName,
    display: GuestDisplay,
}

impl HostInput {
    /// How long one input may take to be delivered.
    const WAIT: Duration = Duration::from_secs(2);
}

impl InputSink for HostInput {
    fn send(&mut self, input: GuestInput) -> Result<(), SourceError> {
        let input = self.display.input(input).map_err(SourceError::new)?;
        self.client
            .host_input(&self.name, &input, within(Self::WAIT))
            .map_err(SourceError::new)
    }
}

/// Taking a machine's control from its agents, on the connection that
/// sends the screen's input: only it may, and closing it hands control back.
struct MachineControl {
    client: Arc<Client<SystemClock>>,
    name: MachineName,
}

impl MachineControl {
    const WAIT: Duration = Duration::from_secs(5);
}

impl Takeover for MachineControl {
    fn take(&mut self) -> Result<(), SourceError> {
        self.client
            .take_control(&self.name, within(Self::WAIT))
            .map_err(SourceError::new)
    }

    fn hand_back(&mut self) -> Result<(), SourceError> {
        self.client
            .hand_back(&self.name, within(Self::WAIT))
            .map_err(SourceError::new)
    }
}

/// The guest display, as big as it is in points: what the viewer's
/// fractions of the screen scale to.
#[derive(Debug, Clone, Copy, PartialEq)]
struct GuestDisplay(Size);

impl GuestDisplay {
    /// The VMM takes points inside the display: the far edges are its last
    /// point, not one past it.
    fn point(self, at: Position) -> Result<Point, GeometryError> {
        let (width, height) = (self.0.width(), self.0.height());
        Point::new(
            (at.x() * width).min(width - 1.0).max(0.0),
            (at.y() * height).min(height - 1.0).max(0.0),
        )
    }

    fn button(button: doppel_viewer::Button) -> Button {
        match button {
            doppel_viewer::Button::Left => Button::Left,
            doppel_viewer::Button::Right => Button::Right,
        }
    }

    fn clicks(clicks: u32) -> ClickCount {
        // Clicking on past a triple still selects as a triple.
        ClickCount::try_from(clicks.clamp(1, 3)).unwrap_or(ClickCount::Single)
    }

    fn modifiers(modifiers: Vec<doppel_viewer::Modifier>) -> Vec<Modifier> {
        modifiers
            .into_iter()
            .map(|modifier| match modifier {
                doppel_viewer::Modifier::Command => Modifier::Command,
                doppel_viewer::Modifier::Shift => Modifier::Shift,
                doppel_viewer::Modifier::Option => Modifier::Option,
                doppel_viewer::Modifier::Control => Modifier::Control,
            })
            .collect()
    }

    fn key(key: doppel_viewer::Key) -> Key {
        use doppel_viewer::NamedKey as Viewer;
        match key {
            doppel_viewer::Key::Character(c) => Key::Character(c),
            doppel_viewer::Key::Named(named) => Key::Named(match named {
                Viewer::Return => NamedKey::Return,
                Viewer::Tab => NamedKey::Tab,
                Viewer::Space => NamedKey::Space,
                Viewer::Delete => NamedKey::Delete,
                Viewer::Escape => NamedKey::Escape,
                Viewer::Left => NamedKey::Left,
                Viewer::Right => NamedKey::Right,
                Viewer::Up => NamedKey::Up,
                Viewer::Down => NamedKey::Down,
                Viewer::Home => NamedKey::Home,
                Viewer::End => NamedKey::End,
                Viewer::PageUp => NamedKey::PageUp,
                Viewer::PageDown => NamedKey::PageDown,
                Viewer::F(n) => NamedKey::F(n),
            }),
        }
    }

    fn input(self, input: GuestInput) -> Result<Input, GeometryError> {
        Ok(match input {
            GuestInput::Pointer {
                phase,
                at,
                modifiers,
            } => Input::Pointer(Pointer {
                phase: match phase {
                    doppel_viewer::PointerPhase::Press { button, clicks } => {
                        PointerPhase::Press(Self::button(button), Self::clicks(clicks))
                    }
                    doppel_viewer::PointerPhase::Release { button, clicks } => {
                        PointerPhase::Release(Self::button(button), Self::clicks(clicks))
                    }
                    doppel_viewer::PointerPhase::Move { held } => {
                        PointerPhase::Move(held.map(Self::button))
                    }
                },
                at: self.point(at)?,
                modifiers: Self::modifiers(modifiers),
            }),
            GuestInput::Scroll { at, dx, dy } => Input::Scroll {
                at: self.point(at)?,
                dx: dx * self.0.width(),
                dy: dy * self.0.height(),
            },
            GuestInput::Key { key, modifiers } => Input::Key(KeyChord {
                modifiers: Self::modifiers(modifiers),
                key: Self::key(key),
            }),
        })
    }
}

/// How a viewer gets a display's frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Frames {
    /// From the memory the VMM shares: on the VMM's own Mac only.
    Shared,
    /// Encoded by the VMM and decoded here: across a network too.
    Encoded,
}

impl Frames {
    /// `name`'s screen, taking input once a person takes control: frames,
    /// input and control each on a connection of their own, so closing
    /// the screen hands control back.
    fn screen(self, socket: &Path, name: &MachineName) -> Result<Screen, ViewError> {
        let (screen, points) = match self {
            Self::Shared => {
                let source = DisplaySource::open(socket, name)?;
                let points = source.frames.points();
                (Screen::new(source), points)
            }
            Self::Encoded => {
                let (source, points) = StreamSource::open(socket, name)?;
                (Screen::encoded(source), points)
            }
        };
        let client = Arc::new(Client::connect(
            socket,
            SystemClock,
            within(DisplaySource::WAIT * 10),
        )?);
        let screen = screen.with_takeover(MachineControl {
            client: Arc::clone(&client),
            name: name.clone(),
        });
        Ok(match points {
            Some(size) => screen.with_points(size).with_input(HostInput {
                client,
                name: name.clone(),
                display: GuestDisplay(size),
            }),
            // Without the display's size a person's input can't be placed
            // on it, so the screen is view-only.
            None => screen,
        })
    }
}

/// The machines to show first: the named ones, or the first running one.
#[derive(Debug)]
pub(crate) struct View {
    names: Vec<MachineName>,
    frames: Frames,
    videos: Videos,
    settings: port::KeptSettings,
}

impl View {
    /// The viewer keeps what the person chooses (the theme) in `home`.
    pub(crate) fn new(
        names: Vec<MachineName>,
        frames: Frames,
        videos: Videos,
        home: &Path,
    ) -> Self {
        Self {
            names,
            frames,
            videos,
            settings: port::KeptSettings::in_home(home),
        }
    }

    /// Shows the machines until the window closes. Everything goes through
    /// the daemon listening on `socket`, which `client` is connected to.
    pub(crate) fn run(self, client: Client<SystemClock>, socket: &Path) -> Result<()> {
        let daemon = port::DaemonPort::new(socket.to_path_buf(), self.frames, self.videos, client);
        Viewer::new(daemon, port::ViewerClock)
            .keeping(self.settings)
            .showing(self.names)
            .run();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display() -> GuestDisplay {
        GuestDisplay(Size::new(1280.0, 800.0).unwrap())
    }

    fn at(x: f64, y: f64) -> Position {
        Position::new(x, y).unwrap()
    }

    #[test]
    fn encoded_frames_keep_their_kind_and_bytes() {
        let key = StreamSource::frame(doppel_client::EncodedFrame {
            sequence: 7,
            kind: doppel_client::FrameKind::Key {
                parameter_sets: vec![vec![0x67], vec![0x68]],
            },
            data: vec![0, 0, 0, 1, 0x65],
        });
        assert_eq!(
            key,
            doppel_viewer::EncodedFrame {
                sequence: 7,
                kind: doppel_viewer::FrameKind::Key {
                    parameter_sets: vec![vec![0x67], vec![0x68]],
                },
                data: vec![0, 0, 0, 1, 0x65],
            }
        );
        let delta = StreamSource::frame(doppel_client::EncodedFrame {
            sequence: 8,
            kind: doppel_client::FrameKind::Delta,
            data: vec![1],
        });
        assert_eq!(delta.kind, doppel_viewer::FrameKind::Delta);
        assert_eq!((delta.sequence, delta.data), (8, vec![1]));
    }

    #[test]
    fn fractions_scale_to_points_inside_the_display() {
        assert_eq!(display().point(at(0.5, 0.25)), Point::new(640.0, 200.0));
        // The far edges are the display's last point, which the VMM accepts.
        assert_eq!(display().point(at(1.0, 1.0)), Point::new(1279.0, 799.0));
    }

    #[test]
    fn viewer_input_becomes_host_input() {
        let press = display()
            .input(GuestInput::Pointer {
                phase: doppel_viewer::PointerPhase::Press {
                    button: doppel_viewer::Button::Right,
                    clicks: 5,
                },
                at: at(0.0, 0.0),
                modifiers: vec![doppel_viewer::Modifier::Shift],
            })
            .unwrap();
        assert_eq!(
            press,
            Input::Pointer(Pointer {
                phase: PointerPhase::Press(Button::Right, ClickCount::Triple),
                at: Point::new(0.0, 0.0).unwrap(),
                modifiers: vec![Modifier::Shift],
            })
        );
        let scroll = display()
            .input(GuestInput::Scroll {
                at: at(0.5, 0.5),
                dx: 0.0,
                dy: -0.1,
            })
            .unwrap();
        assert_eq!(
            scroll,
            Input::Scroll {
                at: Point::new(640.0, 400.0).unwrap(),
                dx: 0.0,
                dy: -80.0,
            }
        );
        let key = display()
            .input(GuestInput::Key {
                key: doppel_viewer::Key::Named(doppel_viewer::NamedKey::F(5)),
                modifiers: Vec::new(),
            })
            .unwrap();
        assert_eq!(
            key,
            Input::Key(KeyChord {
                modifiers: Vec::new(),
                key: Key::Named(NamedKey::F(5)),
            })
        );
    }
}
