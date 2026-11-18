//! A desktop window has no address bar: its place isn't shown anywhere,
//! and nothing moves it but the window itself.

use futures::stream::{self, Empty};

use crate::address::Entry;

/// None: the window opens where `doppel view` says.
pub(crate) const fn current() -> Option<String> {
    None
}

pub(crate) const fn show(_path: &str, _entry: Entry) {}

/// Nothing: there's no Back.
pub(crate) fn changes() -> Empty<String> {
    stream::empty()
}
