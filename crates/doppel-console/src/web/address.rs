//! The page's address bar, where the window's place shows: the History
//! API, through `glue.js`.

use std::pin::Pin;
use std::task::{Context, Poll};

use futures::Stream;
use futures::channel::mpsc;
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::Closure;

use super::js;
use crate::route::Entry;

/// The path the page was opened at, or moved to.
pub(crate) fn current() -> Option<String> {
    Some(js::address())
}

/// Shows `path` in the address bar.
pub(crate) fn show(path: &str, entry: Entry) {
    js::show_address(
        path,
        match entry {
            Entry::Replace => true,
            Entry::Push => false,
        },
    );
}

/// The paths Back and Forward move to, until it's dropped.
pub(crate) fn changes() -> Changes {
    let (sender, arrived) = mpsc::unbounded();
    let on_change = Closure::<dyn FnMut(String)>::new(move |path: String| {
        let _ = sender.unbounded_send(path);
    });
    let listener = js::watch_address(&on_change);
    Changes {
        arrived,
        listener,
        _on_change: on_change,
    }
}

/// Back and Forward, as paths.
pub(crate) struct Changes {
    arrived: mpsc::UnboundedReceiver<String>,
    listener: JsValue,
    _on_change: Closure<dyn FnMut(String)>,
}

impl Stream for Changes {
    type Item = String;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<String>> {
        Pin::new(&mut self.arrived).poll_next(cx)
    }
}

impl Drop for Changes {
    fn drop(&mut self) {
        js::unwatch_address(&self.listener);
    }
}
