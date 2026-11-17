//! Small pieces the window is built of: buttons, labels, dots, the logo
//! and the spinner.
//! Chrome labels are small, monospaced and upper case; GPUI has no
//! text-transform, so the strings are upper-cased where they are built.

use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Animation, AnimationExt, Div, ElementId, FontWeight, Hsla, InteractiveElement, IntoElement,
    ParentElement, SharedString, Stateful, StatefulInteractiveElement, Styled, Transformation, div,
    percentage, px,
};

use crate::icons::Icon;
use crate::theme::{MONO, Theme};

/// The group a button's icon follows on hover.
const BUTTON: &str = "doppel-button";

/// How a button looks: quiet by default; `Primary` for the one main action,
/// `Danger` for what can't be undone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Look {
    Plain,
    Primary,
    Danger,
    /// No fill or border until hovered.
    Ghost,
    /// A toggle that is on.
    On,
}

/// Whether a button can be pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Enabled {
    Yes,
    No,
}

impl Enabled {
    pub(crate) const fn when(enabled: bool) -> Self {
        if enabled { Self::Yes } else { Self::No }
    }

    pub(crate) const fn yes(self) -> bool {
        match self {
            Self::Yes => true,
            Self::No => false,
        }
    }
}

/// A button: an icon, a label, or both. Add its click with
/// [`Button::on_press`], which does nothing while it's disabled.
pub(crate) struct Button {
    element: Stateful<Div>,
    enabled: Enabled,
}

impl std::fmt::Debug for Button {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Button")
            .field("enabled", &self.enabled)
            .finish()
    }
}

impl Button {
    pub(crate) fn new(
        id: impl Into<ElementId>,
        theme: &Theme,
        look: Look,
        icon: Option<Icon>,
        label: Option<SharedString>,
    ) -> Self {
        let (fill, border, ink, hover_fill, hover_ink) = match look {
            Look::Plain => (
                theme.raised,
                theme.line_strong,
                theme.text,
                theme.overlay,
                theme.text,
            ),
            Look::Primary => (
                theme.accent_fill,
                theme.accent_fill,
                theme.on_accent,
                theme.accent_fill.blend(theme.bg.opacity(0.15)),
                theme.on_accent,
            ),
            Look::Danger => (
                theme.danger_fill,
                theme.danger_fill,
                theme.white,
                theme.danger_fill.blend(theme.white.opacity(0.12)),
                theme.white,
            ),
            Look::Ghost => (
                gpui::transparent_black(),
                gpui::transparent_black(),
                theme.muted,
                theme.hover,
                theme.text,
            ),
            Look::On => (
                theme.slab,
                gpui::transparent_black(),
                theme.on_slab,
                theme.slab,
                theme.on_slab,
            ),
        };
        let only_icon = label.is_none();
        let element = div()
            .id(id)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .h(px(30.))
            .border_1()
            .border_color(border)
            .bg(fill)
            .text_color(ink)
            .font_family(MONO)
            .text_size(px(11.))
            .font_weight(FontWeight::MEDIUM)
            .whitespace_nowrap()
            .cursor_pointer()
            .hover(move |style| style.bg(hover_fill).text_color(hover_ink))
            .map(|element| {
                if only_icon {
                    element.w(px(30.))
                } else {
                    element.px(px(11.))
                }
            })
            .group(BUTTON)
            .children(icon.map(|icon| {
                icon.svg(if only_icon { 15. } else { 14. }, ink)
                    .group_hover(BUTTON, move |icon| icon.text_color(hover_ink))
            }))
            .children(label.map(|label| upper(&label)));
        Self {
            element,
            enabled: Enabled::Yes,
        }
    }

    /// Greyed out and unpressable when `enabled` says so.
    pub(crate) fn enabled(mut self, enabled: Enabled) -> Self {
        self.enabled = enabled;
        if !enabled.yes() {
            self.element = self.element.opacity(0.45).cursor_default();
        }
        self
    }

    pub(crate) fn on_press(
        self,
        listener: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
    ) -> Stateful<Div> {
        if self.enabled.yes() {
            self.element.on_click(listener)
        } else {
            self.element
        }
    }
}

/// A spinning loader icon.
pub(crate) fn spinner(id: impl Into<ElementId>, size: f32, color: Hsla) -> impl IntoElement {
    Icon::LoaderCircle.svg(size, color).with_animation(
        id,
        Animation::new(Duration::from_millis(900)).repeat(),
        |svg, delta| svg.with_transformation(Transformation::rotate(percentage(delta))),
    )
}

/// Something that fades in and out, as the browser viewer's `pulse` does.
pub(crate) fn pulse<E: IntoElement + Styled + 'static>(
    id: impl Into<ElementId>,
    period: Duration,
    element: E,
) -> impl IntoElement {
    element.with_animation(id, Animation::new(period).repeat(), |element, delta| {
        // 1 → 0.35 → 1.
        let depth = 1.0 - (delta * 2.0 - 1.0).abs();
        element.opacity(1.0 - 0.65 * depth)
    })
}

/// A chrome label as the browser viewer's `.label-mono` draws it: small,
/// monospaced, upper case.
pub(crate) fn upper(text: &str) -> SharedString {
    text.to_uppercase().into()
}

/// A small square dot.
pub(crate) fn dot(size: f32, fill: Hsla) -> Div {
    div().flex_none().size(px(size)).bg(fill)
}

/// doppel's mark on its slab: two squares, a machine and its double, one
/// outlined and one filled with the signal colour.
pub(crate) fn logo(theme: &Theme, side: f32) -> Div {
    div()
        .flex_none()
        .size(px(side))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .relative()
                .size(px(20.))
                .child(
                    div()
                        .absolute()
                        .left(px(2.))
                        .top(px(2.))
                        .size(px(11.))
                        .border(px(1.5))
                        .border_color(theme.strong),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(7.))
                        .top(px(7.))
                        .size(px(11.))
                        .bg(theme.live),
                ),
        )
}
