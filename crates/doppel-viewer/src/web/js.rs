//! The browser APIs the viewer reaches, as `glue.js` exports them.

use js_sys::Uint8Array;
use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/src/web/glue.js")]
extern "C" {
    /// A screen's H.264 decoder (WebCodecs).
    pub(crate) type ScreenDecoder;

    #[wasm_bindgen(constructor)]
    pub(crate) fn new(
        on_frame: &Closure<dyn FnMut(u32, u32, Uint8Array)>,
        on_error: &Closure<dyn FnMut(String)>,
    ) -> ScreenDecoder;

    #[wasm_bindgen(method)]
    pub(crate) fn configure(this: &ScreenDecoder, codec: &str, description: &[u8]) -> bool;

    #[wasm_bindgen(method)]
    pub(crate) fn decode(this: &ScreenDecoder, data: &[u8], key: bool, timestamp: f64) -> bool;

    #[wasm_bindgen(method, js_name = close)]
    pub(crate) fn close_decoder(this: &ScreenDecoder);

    /// A recording's video, played by a `<video>` the page never shows.
    pub(crate) type VideoPlayer;

    #[wasm_bindgen(constructor)]
    pub(crate) fn new(
        url: &str,
        on_frame: &Closure<dyn FnMut(u32, u32, Uint8Array)>,
        on_ended: &Closure<dyn FnMut()>,
        on_error: &Closure<dyn FnMut(String)>,
    ) -> VideoPlayer;

    #[wasm_bindgen(method)]
    pub(crate) fn play(this: &VideoPlayer);

    #[wasm_bindgen(method)]
    pub(crate) fn pause(this: &VideoPlayer);

    #[wasm_bindgen(method)]
    pub(crate) fn seek(this: &VideoPlayer, seconds: f64);

    #[wasm_bindgen(method)]
    pub(crate) fn position(this: &VideoPlayer) -> f64;

    #[wasm_bindgen(method)]
    pub(crate) fn playing(this: &VideoPlayer) -> bool;

    #[wasm_bindgen(method)]
    pub(crate) fn duration(this: &VideoPlayer) -> f64;

    #[wasm_bindgen(method, js_name = close)]
    pub(crate) fn close_player(this: &VideoPlayer);

    #[wasm_bindgen(js_name = takeToken)]
    pub(crate) fn take_token() -> String;

    #[wasm_bindgen(js_name = shareTokenWithVideos)]
    pub(crate) fn share_token_with_videos(token: &str);

    #[wasm_bindgen(js_name = socketUrl)]
    pub(crate) fn socket_url() -> String;

    pub(crate) fn millis() -> f64;

    #[wasm_bindgen(js_name = localParts)]
    pub(crate) fn local_parts(unix_ms: f64) -> Vec<i32>;

    #[wasm_bindgen(js_name = keptTheme)]
    pub(crate) fn kept_theme() -> String;

    #[wasm_bindgen(js_name = keepTheme)]
    pub(crate) fn keep_theme(theme: &str) -> bool;
}
