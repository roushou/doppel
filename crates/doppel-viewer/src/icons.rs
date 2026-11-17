//! The icons the window draws: Lucide's (ISC, `assets/icons/LICENSE`), the
//! same the browser viewer uses, built into the binary.

use std::borrow::Cow;

use gpui::{AssetSource, Hsla, SharedString, Svg, svg};

macro_rules! icons {
    ($($variant:ident => $file:literal,)*) => {
        /// One of the window's icons.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(crate) enum Icon {
            $($variant,)*
        }

        impl Icon {
            const fn path(self) -> &'static str {
                match self {
                    $(Self::$variant => concat!("icons/", $file, ".svg"),)*
                }
            }
        }

        /// The icons, by the path GPUI asks for.
        #[derive(Debug)]
        pub(crate) struct Assets;

        impl AssetSource for Assets {
            fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
                Ok(match path {
                    $(concat!("icons/", $file, ".svg") => Some(Cow::Borrowed(
                        include_bytes!(concat!("../assets/icons/", $file, ".svg")).as_slice(),
                    )),)*
                    _ => None,
                })
            }

            fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
                Ok([$(concat!("icons/", $file, ".svg"),)*]
                    .into_iter()
                    .filter(|known| known.starts_with(path))
                    .map(SharedString::from)
                    .collect())
            }
        }
    };
}

icons! {
    ArrowDownUp => "arrow-down-up",
    Bookmark => "bookmark",
    Check => "check",
    ChevronRight => "chevron-right",
    CircleStop => "circle-stop",
    Copy => "copy",
    Ellipsis => "ellipsis",
    Eye => "eye",
    Film => "film",
    Flag => "flag",
    EyeOff => "eye-off",
    GitFork => "git-fork",
    Hand => "hand",
    Hourglass => "hourglass",
    Keyboard => "keyboard",
    MapPin => "map-pin",
    ListTree => "list-tree",
    LoaderCircle => "loader-circle",
    Columns2 => "columns-2",
    Monitor => "monitor",
    Moon => "moon",
    MousePointer2 => "mouse-pointer-2",
    Pause => "pause",
    Play => "play",
    Plus => "plus",
    Pointer => "pointer",
    Power => "power",
    RotateCcw => "rotate-ccw",
    Rewind => "rewind",
    ScanEye => "scan-eye",
    ScanText => "scan-text",
    Sun => "sun",
    Terminal => "terminal",
    TextCursorInput => "text-cursor-input",
    TrashTwo => "trash-2",
    TriangleAlert => "triangle-alert",
    UndoTwo => "undo-2",
    X => "x",
}

impl Icon {
    /// The icon as an element, `size` points square, drawn in `color`: an
    /// SVG takes its colour from its own style, never its parent's.
    pub(crate) fn svg(self, size: f32, color: Hsla) -> Svg {
        use gpui::{Styled, px};
        svg()
            .path(self.path())
            .size(px(size))
            .flex_none()
            .text_color(color)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_built_in() {
        for icon in [
            Icon::Pointer,
            Icon::X,
            Icon::TrashTwo,
            Icon::TriangleAlert,
            Icon::ListTree,
            Icon::ChevronRight,
            Icon::ScanText,
            Icon::Flag,
            Icon::MapPin,
            Icon::Rewind,
            Icon::Check,
            Icon::Columns2,
            Icon::Sun,
            Icon::Moon,
        ] {
            assert!(matches!(Assets.load(icon.path()), Ok(Some(_))), "{icon:?}");
        }
        assert!(matches!(Assets.load("icons/none.svg"), Ok(None)));
        assert_eq!(Assets.list("icons/").map(|all| all.len()).ok(), Some(38));
    }
}
