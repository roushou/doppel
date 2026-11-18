//! Where the window is, as an address: what the browser's address bar
//! shows, so a link opens the same place and Back returns to the last
//! one. Parsed once from the path; everything past it is a [`Route`].
//! Pure.

use doppel_machine::machine::MachineName;

use crate::RecordingId;

/// A place in the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Route {
    /// The machines, with whatever the stage shows: `/`.
    Machines,
    /// One machine on the stage: `/machines/<name>`.
    Machine(MachineName),
    /// The recordings: `/replay`.
    Recordings,
    /// One recording, replayed: `/replay/<id>`.
    Replay(RecordingId),
}

impl Route {
    /// The place `path` names; `None` for one that isn't a place, or names
    /// something that can't be (a malformed machine name or recording).
    pub(crate) fn parse(path: &str) -> Option<Self> {
        let path = path.strip_suffix('/').unwrap_or(path);
        let mut parts = path.split('/').skip(1);
        match (parts.next(), parts.next(), parts.next()) {
            (None | Some(""), None, None) | (Some("machines"), None, None) => Some(Self::Machines),
            (Some("machines"), Some(name), None) => name.parse().ok().map(Self::Machine),
            (Some("replay"), None, None) => Some(Self::Recordings),
            (Some("replay"), Some(id), None) => recording(id).map(Self::Replay),
            (Some(_) | None, _, _) => None,
        }
    }

    /// Its path, which [`Route::parse`] reads back.
    pub(crate) fn path(&self) -> String {
        match self {
            Self::Machines => "/".to_owned(),
            Self::Machine(name) => format!("/machines/{name}"),
            Self::Recordings => "/replay".to_owned(),
            Self::Replay(id) => format!("/replay/{}", id.as_str()),
        }
    }
}

/// A recording's id as a path names it: one component of letters, digits,
/// `-`, `_` and `.`, not starting with a dot, which is what the daemon
/// names them.
fn recording(raw: &str) -> Option<RecordingId> {
    let fits = !raw.is_empty()
        && !raw.starts_with('.')
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    fits.then(|| RecordingId::new(raw.to_owned()))
}

// How a place is shown in the address bar (`platform::address`).

/// Whether showing a place adds to the browser's history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Entry {
    /// In place of the current entry: where the page opened, settled.
    Replace,
    /// A new entry, which Back returns from.
    Push,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_place_reads_back_from_its_path() {
        let live: MachineName = "live".parse().unwrap();
        for route in [
            Route::Machines,
            Route::Machine(live),
            Route::Recordings,
            Route::Replay(RecordingId::new("2026-11-17T01-55-02Z-s1".into())),
        ] {
            assert_eq!(
                Route::parse(&route.path()),
                Some(route.clone()),
                "{route:?}"
            );
        }
    }

    #[test]
    fn paths_that_name_no_place_are_refused() {
        assert_eq!(Route::parse(""), Some(Route::Machines));
        assert_eq!(Route::parse("/machines/"), Some(Route::Machines));
        assert_eq!(Route::parse("/replay/"), Some(Route::Recordings));
        for path in [
            "/elsewhere",
            "/machines/live/screen",
            "/machines/Not A Name",
            "/replay/.hidden",
            "/replay/a%2Fb",
            "/replay/a/b",
        ] {
            assert_eq!(Route::parse(path), None, "{path}");
        }
    }
}
