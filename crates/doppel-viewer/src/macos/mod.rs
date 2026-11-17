//! The desktop's half of what differs between targets: frames decoded with
//! VideoToolbox into CoreVideo pixel buffers GPUI draws as surfaces, a
//! recording's video read with AVFoundation, the screen's ports and the
//! threads that read and feed them, and the window tagged sRGB.

pub(crate) mod address;
mod decode;
pub(crate) mod feed;
mod pixels;
mod player;
pub(crate) mod ports;
mod srgb;

pub(crate) use pixels::Buffer as Frame;
pub(crate) use player::Player;
pub(crate) use srgb::tag as tag_window;

use gpui::App;

use crate::icons::Assets;

/// Launches the app, and opens the window with `open`: the app quits when
/// it's closed, or when it can't be opened.
pub(crate) fn launch(open: impl FnOnce(&mut App) -> bool + 'static) {
    gpui_platform::application()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            if !open(cx) {
                cx.quit();
                return;
            }
            cx.on_window_closed(|cx, _| cx.quit()).detach();
            cx.activate(true);
        });
}
