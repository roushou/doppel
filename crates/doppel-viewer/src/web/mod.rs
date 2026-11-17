//! The browser's half of what differs between targets: the daemon reached
//! over the gateway's WebSocket, a screen's frames decoded by the browser
//! (WebCodecs) and a recording's video played by a `<video>`, each picture
//! handed to GPUI as an image; the time and the kept theme from the page.
//! `run` is what the page calls.

mod connection;
mod daemon;
pub(crate) mod feed;
mod frame;
mod js;
mod player;

use std::cell::RefCell;

use gpui::{App, ApplicationHandle};
use wasm_bindgen::prelude::wasm_bindgen;

pub use feed::Screen;
pub(crate) use frame::Frame;
pub(crate) use player::Player;

use crate::icons::Assets;
use crate::{LocalClock, LocalTime, Settings, SettingsError, ThemeChoice, Viewer};

thread_local! {
    /// The running app: dropping it would end it.
    static APPLICATION: RefCell<Option<ApplicationHandle>> = const { RefCell::new(None) };
}

/// Launches the app in the page, and opens the window with `open`.
pub(crate) fn launch(open: impl FnOnce(&mut App) -> bool + 'static) {
    gpui_platform::web_init();
    let handle = gpui_platform::single_threaded_web()
        .with_assets(Assets)
        .run_embedded(move |cx: &mut App| {
            // A page that can't open its window shows nothing either way.
            let _ = open(cx);
        });
    APPLICATION.with(|application| *application.borrow_mut() = Some(handle));
}

/// The browser draws in sRGB already.
pub(crate) fn tag_window(_: &mut gpui::Window) {}

/// Shows the viewer in the page, over the daemon behind the gateway that
/// served it, with the token the page was opened with.
#[wasm_bindgen]
pub fn run() {
    let token = js::take_token();
    js::share_token_with_videos(&token);
    Viewer::new(daemon::WebDaemon::new(js::socket_url(), token), PageClock)
        .keeping(PageSettings)
        .run();
}

/// The page's time.
struct PageClock;

impl LocalClock for PageClock {
    fn millis(&self) -> f64 {
        js::millis()
    }

    fn local(&self) -> LocalTime {
        local(js_sys::Date::now())
    }
}

/// `unix_ms` in the browser's time zone.
pub(crate) fn local(unix_ms: f64) -> LocalTime {
    let parts = js::local_parts(unix_ms);
    let part = |at: usize| parts.get(at).copied().unwrap_or(0);
    let small = |at: usize| u8::try_from(part(at)).unwrap_or(0);
    LocalTime {
        year: part(0),
        month: small(1),
        day: small(2),
        hour: small(3),
        minute: small(4),
        second: small(5),
    }
}

/// The theme, kept in the browser's storage for this gateway's pages.
struct PageSettings;

/// The browser wouldn't keep it (private browsing, or storage off).
#[derive(Debug, thiserror::Error)]
#[error("the browser won't keep the theme")]
struct NotKept;

impl Settings for PageSettings {
    fn theme(&self) -> ThemeChoice {
        js::kept_theme().parse().unwrap_or_default()
    }

    fn keep_theme(&self, choice: ThemeChoice) -> Result<(), SettingsError> {
        if js::keep_theme(choice.as_str()) {
            Ok(())
        } else {
            Err(SettingsError::new(NotKept))
        }
    }
}
