//! How a place is shown in the address bar (`platform::address`).

/// Whether showing a place adds to the browser's history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Entry {
    /// In place of the current entry: where the page opened, settled.
    Replace,
    /// A new entry, which Back returns from.
    Push,
}
