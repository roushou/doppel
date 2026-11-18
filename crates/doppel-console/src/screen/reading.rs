//! The Text layer: the lines a call read off the screen by text
//! recognition, boxed where they are, with what it looked for marked. Each
//! line's text shows when the pointer is over it. A chip in the corner says
//! whose read it is and how old, and offers a person their own read.

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, px, rgb,
};

use crate::screen::Picture;
use crate::screen::inspect::Looking;
use crate::timeline::TextRead;
use crate::ui::icons::Icon;
use crate::ui::theme::Theme;
use doppel_desktop::geometry::Rect;

/// What the layer draws: a read, or why there is none to draw.
pub(crate) enum Drawn<'a> {
    Read(&'a TextRead),
    Loading,
    Failed(&'a str),
    Nothing,
}

/// Runs a person's own read.
pub(crate) type ReadNow = Rc<dyn Fn(&mut Window, &mut App)>;

/// Lines are drawn in one colour, distinct from the elements' kinds; what
/// was looked for, in the accent.
const LINE: u32 = 0x00f0_b429;

#[expect(clippy::too_many_arguments, reason = "one layer's whole state, drawn")]
pub(crate) fn layer(
    id: &str,
    drawn: Drawn<'_>,
    picture: Picture,
    whose: SharedString,
    stale: bool,
    looking: Option<Looking>,
    theme: &Theme,
    read_now: &ReadNow,
) -> impl IntoElement {
    let mut layer = div().absolute().inset_0().overflow_hidden();
    let place = |frame: &Rect| {
        let (left, top) = picture.place(frame.left(), frame.top());
        let (right, bottom) =
            picture.place(frame.left() + frame.width(), frame.top() + frame.height());
        (left, top, (right - left).max(1.), (bottom - top).max(1.))
    };
    if let Drawn::Read(read) = &drawn {
        let ink = rgb(LINE);
        for (index, line) in read.lines.iter().enumerate() {
            let (left, top, width, height) = place(&line.frame);
            let group = SharedString::from(format!("{id}-line-{index}"));
            layer = layer.child(
                div()
                    .id(group.clone())
                    .group(group.clone())
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(width))
                    .h(px(height))
                    .border_1()
                    .border_color(ink)
                    .opacity(if stale { 0.35 } else { 0.75 })
                    .hover(|line| line.opacity(1.).bg(gpui::Hsla::from(ink).opacity(0.15)))
                    .child(
                        div()
                            .invisible()
                            .group_hover(group, |tag| tag.visible())
                            .absolute()
                            .left(px(-1.))
                            .map(|tag| {
                                if top < 22. {
                                    tag.top(px(height + 3.))
                                } else {
                                    tag.bottom(px(height + 3.))
                                }
                            })
                            .max_w(px(360.))
                            .px(px(6.))
                            .py(px(2.))
                            .bg(gpui::black().opacity(0.8))
                            .text_size(px(11.))
                            .text_color(gpui::white())
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(line.text.clone()),
                    ),
            );
        }
        for found in &read.found {
            let (left, top, width, height) = place(found);
            layer = layer.child(
                div()
                    .absolute()
                    .left(px(left - 2.))
                    .top(px(top - 2.))
                    .w(px(width + 4.))
                    .h(px(height + 4.))
                    .border_2()
                    .border_color(theme.accent)
                    .bg(theme.accent.opacity(0.18)),
            );
        }
    }
    let state: SharedString = match &drawn {
        Drawn::Read(read) => match &read.query {
            Some(query) if read.found.is_empty() => format!("{whose} · “{query}” not found").into(),
            Some(query) => format!("{whose} · “{query}” ×{}", read.found.len()).into(),
            None => whose,
        },
        Drawn::Loading => "Reading…".into(),
        Drawn::Failed(why) => format!("Can't read the screen: {why}").into(),
        Drawn::Nothing => "No text read yet".into(),
    };
    let state: SharedString = match &drawn {
        Drawn::Read(_) if stale => format!("{state} · acted since").into(),
        Drawn::Read(_) | Drawn::Loading | Drawn::Failed(_) | Drawn::Nothing => state,
    };
    let read_now = read_now.clone();
    let chip = div()
        .absolute()
        .top(px(picture.top + 8.))
        .right(px(8.))
        .flex()
        .items_center()
        .gap(px(6.))
        .pl(px(8.))
        .pr(px(4.))
        .py(px(3.))
        .bg(gpui::black().opacity(0.7))
        .text_size(px(11.))
        .text_color(gpui::white())
        .child(Icon::ScanText.svg(12., gpui::white().opacity(0.8)))
        .child(
            div()
                .whitespace_nowrap()
                .child(state)
                .when(stale, |words| words.text_color(theme.warning)),
        )
        .children(looking.map(|looking| {
            let busy = looking == Looking::Busy;
            div()
                .id(SharedString::from(format!("{id}-read-now")))
                .px(px(6.))
                .py(px(1.))
                .font_weight(FontWeight::MEDIUM)
                .bg(gpui::white().opacity(if busy { 0.08 } else { 0.16 }))
                .text_color(gpui::white().opacity(if busy { 0.5 } else { 1. }))
                .when(!busy, |button| {
                    button
                        .cursor_pointer()
                        .hover(|button| button.bg(gpui::white().opacity(0.26)))
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            read_now(window, cx);
                        })
                })
                .child(if busy { "Reading…" } else { "Read now" })
        }));
    layer.child(chip)
}
