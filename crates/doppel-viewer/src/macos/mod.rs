//! The desktop's half of what differs between targets: frames decoded with
//! VideoToolbox into CoreVideo pixel buffers GPUI draws as surfaces, a
//! recording's video read with AVFoundation, the screen's ports and the
//! threads that read and feed them, and the window tagged sRGB.

mod decode;
pub(crate) mod feed;
mod pixels;
mod player;
pub(crate) mod ports;
mod srgb;

pub(crate) use pixels::Buffer as Frame;
pub(crate) use player::Player;
pub(crate) use srgb::tag as tag_window;
