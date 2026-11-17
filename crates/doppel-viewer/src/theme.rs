//! The window's colours, fonts and sizes: the browser viewer's design
//! tokens (`web/src/app.css`), dark and light, picked by the system's
//! appearance.
//!
//! Blueprint slate, dark and light: square and
//! hairline, Geist for people's words and Geist Mono for the machine's. The
//! chrome is drawn in `text` on the ground, with `slab` for what is
//! selected or primary, `live` for what is live, and `signal` for a person
//! in control or attention. Nothing is rounded and nothing casts a shadow.

use crate::ThemeChoice;
use gpui::{BoxShadow, Global, Hsla, WindowAppearance, point, px, rgba};

/// Every colour the window uses, by role.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Theme {
    /// Surfaces: the ground, and the panel a little above it.
    pub(crate) bg: Hsla,
    pub(crate) surface: Hsla,
    pub(crate) raised: Hsla,
    pub(crate) overlay: Hsla,
    pub(crate) hover: Hsla,
    pub(crate) pressed: Hsla,
    pub(crate) line: Hsla,
    pub(crate) line_strong: Hsla,

    /// The chrome's ink: text, and the rules between the window's parts.
    pub(crate) text: Hsla,
    /// Text that leads: names, times.
    pub(crate) strong: Hsla,
    pub(crate) muted: Hsla,
    pub(crate) faint: Hsla,

    /// What is selected or primary, and the ink on it.
    pub(crate) slab: Hsla,
    pub(crate) on_slab: Hsla,
    /// A person in control, or attention, and the ink on it.
    pub(crate) signal: Hsla,
    pub(crate) on_signal: Hsla,
    /// What is live: a running machine, the stream, what is selected.
    pub(crate) live: Hsla,

    pub(crate) accent: Hsla,
    pub(crate) accent_fill: Hsla,
    /// The ink on `accent_fill`.
    pub(crate) on_accent: Hsla,
    pub(crate) accent_soft: Hsla,
    pub(crate) success: Hsla,
    pub(crate) success_soft: Hsla,
    pub(crate) warning: Hsla,
    pub(crate) warning_soft: Hsla,
    pub(crate) danger: Hsla,
    pub(crate) danger_fill: Hsla,
    pub(crate) danger_soft: Hsla,

    /// The screen behind a machine's picture.
    pub(crate) screen: Hsla,
    /// Behind a dialog.
    pub(crate) backdrop: Hsla,
    pub(crate) white: Hsla,

    /// What an agent points out to the people watching.
    pub(crate) mark: Hsla,
    pub(crate) mark_soft: Hsla,
    pub(crate) mark_ink: Hsla,
    pub(crate) failed_ink: Hsla,
}

fn c(hex: u32) -> Hsla {
    rgba(hex).into()
}

impl Theme {
    pub(crate) fn dark() -> Self {
        let ink = 0x00c9_d2e0;
        let slab = 0x002a_3548;
        let signal = 0x00ff_7a50;
        let live = 0x004f_bf83;
        Self {
            bg: c(0x141a_24ff),
            surface: c(0x141a_24ff),
            raised: c(0x1a22_30ff),
            overlay: c(0x1a22_30ff),
            // Ink at 7% and 12%.
            hover: c(ink << 8 | 0x12),
            pressed: c(ink << 8 | 0x1f),
            line: c(0x2c37_47ff),
            line_strong: c(0x4653_6aff),
            text: c(ink << 8 | 0xff),
            strong: c(0xe3e9_f2ff),
            muted: c(0x8794_a8ff),
            faint: c(0x6673_8aff),
            slab: c(slab << 8 | 0xff),
            on_slab: c(0xe3e9_f2ff),
            signal: c(signal << 8 | 0xff),
            on_signal: c(0x2a0d_03ff),
            live: c(live << 8 | 0xff),
            accent: c(signal << 8 | 0xff),
            accent_fill: c(slab << 8 | 0xff),
            on_accent: c(0xe3e9_f2ff),
            accent_soft: c(signal << 8 | 0x29),
            success: c(live << 8 | 0xff),
            success_soft: c(live << 8 | 0x24),
            warning: c(0xe0ad_2eff),
            warning_soft: c(0xe0ad_2e24),
            danger: c(0xff6b_6bff),
            danger_fill: c(0xd645_45ff),
            danger_soft: c(0xff6b_6b24),
            screen: c(0x0b0f_16ff),
            backdrop: c(0x0000_008c),
            white: c(0xffff_ffff),
            mark: c(0xff5c_a8ff),
            mark_soft: c(0xff5c_a82e),
            mark_ink: c(0xc93a_7ef0),
            failed_ink: c(0xc93a_3aff),
        }
    }

    pub(crate) fn light() -> Self {
        let ink = 0x001f_2633;
        let ground = 0x00d8_dce3;
        let signal = 0x00ff_6a3d;
        let live = 0x002f_8a5b;
        Self {
            bg: c(ground << 8 | 0xff),
            surface: c(ground << 8 | 0xff),
            raised: c(0xcdd2_dbff),
            overlay: c(0xcdd2_dbff),
            hover: c(ink << 8 | 0x12),
            pressed: c(ink << 8 | 0x1f),
            line: c(0xb0b8_c7ff),
            line_strong: c(0x8794_a8ff),
            text: c(ink << 8 | 0xff),
            strong: c(0x0f14_1cff),
            muted: c(0x535e_6eff),
            faint: c(0x6b76_89ff),
            // What's selected sits on a light slab; the main action stays ink.
            slab: c(0xbcc3_ceff),
            on_slab: c(ink << 8 | 0xff),
            signal: c(signal << 8 | 0xff),
            on_signal: c(0x2a0d_03ff),
            live: c(live << 8 | 0xff),
            accent: c(signal << 8 | 0xff),
            accent_fill: c(ink << 8 | 0xff),
            on_accent: c(ground << 8 | 0xff),
            accent_soft: c(signal << 8 | 0x29),
            success: c(live << 8 | 0xff),
            success_soft: c(live << 8 | 0x24),
            warning: c(0x8a61_00ff),
            warning_soft: c(0xd4a0_172e),
            danger: c(0xb326_1eff),
            danger_fill: c(0xb326_1eff),
            danger_soft: c(0xb326_1e1f),
            screen: c(0xc3c9_d3ff),
            backdrop: c(0x0000_008c),
            white: c(0xffff_ffff),
            mark: c(0xff5c_a8ff),
            mark_soft: c(0xff5c_a82e),
            mark_ink: c(0xc93a_7ef0),
            failed_ink: c(0xc93a_3aff),
        }
    }

    /// The theme `choice` shows, with the system's `appearance`.
    pub(crate) fn chosen(choice: ThemeChoice, appearance: WindowAppearance) -> Self {
        match choice {
            ThemeChoice::System => Self::for_appearance(appearance),
            ThemeChoice::Light => Self::light(),
            ThemeChoice::Dark => Self::dark(),
        }
    }

    pub(crate) fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::dark(),
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::light(),
        }
    }

    /// A ring round something, as a CSS `box-shadow: 0 0 0 Npx` draws.
    pub(crate) fn ring(color: Hsla, width: f32) -> BoxShadow {
        BoxShadow {
            color,
            offset: point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: px(width),
            inset: false,
        }
    }
}

impl Global for Theme {}

/// A session's colours over a machine's screen: its pointer and the marks
/// its calls leave, a soft fill, and the ink its captions sit on (in white).
/// The same in light and dark: they are drawn over the guest's screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Hue {
    pub(crate) fill: Hsla,
    pub(crate) soft: Hsla,
    pub(crate) ink: Hsla,
}

/// The sessions' colours, as RGB: fill and ink. The first is the agent's
/// colour of one session alone. None is the signal orange: that means a
/// person in control, or attention. The browser viewer has the same.
const HUES: [(u32, u32); 4] = [
    (0x0022_e5ff, 0x000b_5d6b), // cyan
    (0x003d_8bff, 0x001f_4f9e), // blue
    (0x00b1_8cff, 0x005b_3aa8), // violet
    (0x00c3_d94e, 0x0056_6314), // lime
];

impl Hue {
    /// Session `session`'s colours: sessions count from 1, and a recording's
    /// one session (0) takes the first.
    pub(crate) fn of(session: u64) -> Self {
        let index = usize::try_from(session.saturating_sub(1) % HUES.len() as u64).unwrap_or(0);
        let (fill, ink) = HUES.get(index).copied().unwrap_or(HUES[0]);
        Self {
            fill: c(fill << 8 | 0xff),
            soft: c(fill << 8 | 0x38),
            ink: c(ink << 8 | 0xf0),
        }
    }
}

/// The fonts the window uses, as the browser viewer does: Geist for
/// people's words, Geist Mono for the machine's. Built into the binary
/// (OFL, `assets/fonts/LICENSE`) in four weights each.
pub(crate) const SANS: &str = "Geist";
pub(crate) const MONO: &str = "Geist Mono";

/// The fonts' files, for the text system to load before the window opens.
pub(crate) const FONTS: [&[u8]; 8] = [
    include_bytes!("../assets/fonts/Geist-400.ttf"),
    include_bytes!("../assets/fonts/Geist-500.ttf"),
    include_bytes!("../assets/fonts/Geist-600.ttf"),
    include_bytes!("../assets/fonts/Geist-700.ttf"),
    include_bytes!("../assets/fonts/GeistMono-400.ttf"),
    include_bytes!("../assets/fonts/GeistMono-500.ttf"),
    include_bytes!("../assets/fonts/GeistMono-600.ttf"),
    include_bytes!("../assets/fonts/GeistMono-700.ttf"),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG relative luminance of an RGB colour.
    fn luminance(rgb: u32) -> f64 {
        let channel = |shift: u32| {
            let v = f64::from((rgb >> shift) & 0xff) / 255.0;
            if v <= 0.039_28 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
    }

    fn contrast(a: u32, b: u32) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn captions_read_on_every_sessions_ink_and_pointers_stand_out() {
        for (fill, ink) in HUES {
            // White caption text on the ink, as text needs.
            assert!(contrast(ink, 0xff_ffff) >= 4.5, "{ink:06x}");
            // The pointer, outlined in white, against a dark screen; its
            // white outline carries it on a light one.
            assert!(contrast(fill, 0x00_0000) >= 3.0, "{fill:06x}");
        }
        assert_eq!(Hue::of(1), Hue::of(0));
        assert_eq!(Hue::of(1), Hue::of(5));
        assert_ne!(Hue::of(1), Hue::of(2));
    }
}
