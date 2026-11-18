//! The elements an agent saw, shown to a person: a tree of them beside the
//! screen, and their boxes over it, linked both ways. What is shown comes
//! from [`elements`]; this module only draws it and says what a person did.

use std::cell::Cell;
use std::collections::HashSet;
use std::f32::consts::FRAC_PI_2;
use std::rc::Rc;
use std::sync::Arc;

use doppel_desktop::ax::refs::Ref;
use doppel_desktop::ax::tree::{AxSnapshot, NodeIndex};
use doppel_desktop::geometry::Point;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Bounds, FontWeight, Hsla, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Pixels, Radians, ScrollHandle, SharedString, StatefulInteractiveElement, Styled,
    Transformation, Window, canvas, div, px, rgb,
};

use crate::elements::{self, Kind, Named, Row, Wrappers};
use crate::screen::Picture;
use crate::ui::icons::Icon;
use crate::ui::theme::{MONO, Theme};

/// What a person did in the tree or over the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pointing {
    /// The pointer is over an element, or over none.
    Hover(Option<NodeIndex>),
    /// The pointer left an element's row. It clears the highlight only if
    /// that row still has it: crossing to the next row, its entering may
    /// come before this row's leaving.
    Leave(NodeIndex),
    /// An element picked, or the pick dropped.
    Pick(Option<NodeIndex>),
    /// A branch folded or opened.
    Fold(NodeIndex),
    Wrappers(Wrappers),
    /// The person asked to look at the machine's elements themselves.
    Look,
    Close,
}

/// Whether the panel offers a person their own look: live, and not while
/// one is under way. A replay can't look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Looking {
    Ready,
    Busy,
}

/// Tells the panel's owner what a person did.
pub(crate) type Act = Rc<dyn Fn(Pointing, &mut Window, &mut App)>;

/// One panel's inspection: what is pointed at and picked, what is folded,
/// and where its boxes' layer is, to read the pointer over it.
#[derive(Debug)]
pub(crate) struct Inspect {
    pub(crate) wrappers: Wrappers,
    pub(crate) pointed: Option<NodeIndex>,
    pub(crate) picked: Option<NodeIndex>,
    pub(crate) folded: HashSet<NodeIndex>,
    /// The snapshot the folds and picks are about; a new one starts afresh.
    shown: Option<u64>,
    layer: Rc<Cell<Option<Bounds<Pixels>>>>,
    pub(crate) list: ScrollHandle,
}

impl Inspect {
    pub(crate) fn new() -> Self {
        Self {
            wrappers: Wrappers::Hide,
            pointed: None,
            picked: None,
            folded: HashSet::new(),
            shown: None,
            layer: Rc::new(Cell::new(None)),
            list: ScrollHandle::new(),
        }
    }

    /// Starts afresh when `snapshot` isn't the one shown before: other
    /// elements, so nothing pointed at or picked, and menus folded.
    pub(crate) fn follow(&mut self, snapshot: Option<&AxSnapshot>) {
        let id = snapshot.map(|snapshot| snapshot.id.get());
        if id == self.shown {
            return;
        }
        self.shown = id;
        self.pointed = None;
        self.picked = None;
        self.folded = snapshot.map(elements::initially_folded).unwrap_or_default();
    }

    /// Takes in what a person did; `false` when they closed the panel.
    pub(crate) fn apply(&mut self, pointing: Pointing, snapshot: Option<&AxSnapshot>) -> bool {
        match pointing {
            Pointing::Hover(index) => self.pointed = index,
            Pointing::Leave(index) => {
                if self.pointed == Some(index) {
                    self.pointed = None;
                }
            }
            Pointing::Pick(index) => {
                self.picked = index;
                if let (Some(index), Some(snapshot)) = (index, snapshot) {
                    self.folded = elements::reveal(snapshot, &self.folded, index);
                    let rows = elements::rows(snapshot, self.wrappers, &self.folded);
                    if let Some(at) = rows.iter().position(|row| row.index == index) {
                        self.list.scroll_to_item(at);
                    }
                }
            }
            Pointing::Fold(index) => {
                if !self.folded.remove(&index) {
                    self.folded.insert(index);
                }
            }
            Pointing::Wrappers(wrappers) => self.wrappers = wrappers,
            // The panel's owner takes the look.
            Pointing::Look => {}
            Pointing::Close => return false,
        }
        true
    }
}

/// A kind's colour: the same as the web console's, chosen to read over
/// any guest content.
fn colour(kind: Kind) -> Hsla {
    let hex = match kind {
        Kind::Control => 0x004c_8dff,
        Kind::Field => 0x002f_bf71,
        Kind::Text => 0x00b1_8cff,
        Kind::Menu => 0x00f0_b429,
        Kind::Window | Kind::Container | Kind::Other => 0x0090_97a2,
    };
    rgb(hex).into()
}

/// The elements' boxes over a screen whose picture is at `picture`: those
/// that can be acted on all the time, any other while pointed at or picked.
/// Pointing at the screen picks the deepest element there, unless a person
/// drives it (`picking` off).
pub(crate) fn boxes(
    snapshot: &Arc<AxSnapshot>,
    named: &Named,
    inspect: &Inspect,
    picture: Picture,
    stale: bool,
    picking: bool,
    act: &Act,
) -> impl IntoElement {
    let focus = [inspect.pointed, inspect.picked];
    let mut layer = div().absolute().inset_0().overflow_hidden().child(
        canvas(
            {
                let bounds = inspect.layer.clone();
                move |laid_out, _, _| bounds.set(Some(laid_out))
            },
            |_, (), _, _| {},
        )
        .absolute()
        .size_full(),
    );
    for (index, node) in snapshot.tree.iter() {
        let kind = Kind::of(&node.role);
        let focused = focus.contains(&Some(index));
        if inspect.wrappers == Wrappers::Hide && elements::wrapper(node)
            || !(kind.outlined() || focused)
        {
            continue;
        }
        let Some(frame) = elements::seen(snapshot, index) else {
            continue;
        };
        let (left, top) = picture.place(frame.left(), frame.top());
        let (right, bottom) =
            picture.place(frame.left() + frame.width(), frame.top() + frame.height());
        let ink = colour(kind);
        let mut shown = div()
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(right - left))
            .h(px(bottom - top))
            .border_color(ink)
            .opacity(if focused {
                1.0
            } else if stale {
                0.3
            } else {
                0.7
            });
        shown = if focused {
            shown.border_2().bg(ink.opacity(0.18)).child(
                div()
                    .absolute()
                    .left(px(-2.))
                    .map(|tag| {
                        if top < 24. {
                            tag.top(px(bottom - top + 3.))
                        } else {
                            tag.bottom(px(bottom - top + 3.))
                        }
                    })
                    .max_w(px(280.))
                    .px(px(6.))
                    .py(px(2.))
                    .bg(ink.blend(gpui::black().opacity(0.25)))
                    .text_size(px(11.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(gpui::white())
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(match named.get(index) {
                        Some(r) => format!("{r} · {}", elements::label(node)),
                        None => elements::label(node),
                    }),
            )
        } else {
            shown.border_1()
        };
        layer = layer.child(shown);
    }
    if picking {
        let under = move |position: gpui::Point<Pixels>,
                          bounds: &Rc<Cell<Option<Bounds<Pixels>>>>| {
            let bounds = bounds.get()?;
            let x = f32::from(position.x - bounds.origin.x);
            let y = f32::from(position.y - bounds.origin.y);
            let (width, height) = (picture.points.width(), picture.points.height());
            let fx = f64::from((x - picture.left) / picture.width);
            let fy = f64::from((y - picture.top) / picture.height);
            ((0.0..=1.0).contains(&fx) && (0.0..=1.0).contains(&fy))
                .then(|| Point::new(fx * width, fy * height).ok())
                .flatten()
        };
        let (moved, pressed) = (act.clone(), act.clone());
        let (snapshot_moved, snapshot_pressed) = (snapshot.clone(), snapshot.clone());
        let (bounds_moved, bounds_pressed) = (inspect.layer.clone(), inspect.layer.clone());
        let wrappers = inspect.wrappers;
        layer = layer
            .on_mouse_move(move |event, window, cx| {
                let at = under(event.position, &bounds_moved)
                    .and_then(|at| elements::element_at(&snapshot_moved, at, wrappers));
                moved(Pointing::Hover(at), window, cx);
            })
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                let at = under(event.position, &bounds_pressed)
                    .and_then(|at| elements::element_at(&snapshot_pressed, at, wrappers));
                pressed(Pointing::Pick(at), window, cx);
            });
    }
    layer
}

/// Why there's no snapshot to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Empty {
    /// No call has seen one yet.
    None,
    Loading,
    Failed(String),
}

/// The tree panel: the snapshot's elements as a tree, its age, and the
/// picked element's details; or why there's nothing to show. It is `width`
/// wide, or fills its row.
#[expect(clippy::too_many_arguments, reason = "one panel's whole state, drawn")]
pub(crate) fn tree(
    id: &str,
    snapshot: Result<&Arc<AxSnapshot>, Empty>,
    named: &Named,
    inspect: &Inspect,
    age: SharedString,
    stale: bool,
    looking: Option<Looking>,
    width: Option<f32>,
    theme: &Theme,
    act: &Act,
) -> impl IntoElement {
    let close = act.clone();
    let look = act.clone();
    let mut heading = div().flex().items_center().gap(px(8.)).min_w_0().child(
        div()
            .text_size(px(13.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.text)
            .truncate()
            .child(
                snapshot
                    .as_ref()
                    .map_or_else(|_| "Elements".to_owned(), |seen| seen.app.name.clone()),
            ),
    );
    if snapshot.is_ok() {
        heading = heading.child(
            div()
                .text_size(px(12.))
                .text_color(if stale { theme.warning } else { theme.muted })
                .whitespace_nowrap()
                .child(if stale {
                    SharedString::from(format!("{age} · acted since"))
                } else {
                    age
                }),
        );
    }
    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .pl(px(12.))
        .pr(px(6.))
        .py(px(6.))
        .child(heading)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(2.))
                .children(looking.map(|looking| {
                    let busy = looking == Looking::Busy;
                    div()
                        .id(SharedString::from(format!("{id}-look")))
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .h(px(26.))
                        .px(px(8.))
                        .text_size(px(12.))
                        .text_color(if busy { theme.faint } else { theme.muted })
                        .whitespace_nowrap()
                        .when(!busy, |button| {
                            button
                                .cursor_pointer()
                                .hover(|button| button.bg(theme.hover).text_color(theme.text))
                                .on_click(move |_, window, cx| look(Pointing::Look, window, cx))
                        })
                        .child(Icon::ScanEye.svg(13., if busy { theme.faint } else { theme.muted }))
                        .child(if busy { "Looking…" } else { "Look now" })
                }))
                .child(
                    div()
                        .id(SharedString::from(format!("{id}-close")))
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(26.))
                        .cursor_pointer()
                        .hover(|close| close.bg(theme.hover))
                        .child(Icon::X.svg(14., theme.muted))
                        .on_click(move |_, window, cx| close(Pointing::Close, window, cx)),
                ),
        );
    let panel = div()
        .flex()
        .flex_col()
        .map(|panel| match width {
            Some(width) => panel.flex_none().w(px(width)),
            None => panel.flex_1().min_w_0(),
        })
        .min_h_0()
        .border_1()
        .border_color(theme.line)
        .bg(theme.surface)
        .overflow_hidden()
        .child(header);
    let snapshot = match snapshot {
        Ok(snapshot) => snapshot,
        Err(empty) => {
            let words = match empty {
                Empty::None if looking.is_some() => {
                    "No snapshot yet. The tree shows what the agent sees each time it takes one, or what you see with Look now."
                        .to_owned()
                }
                Empty::None => {
                    "No snapshot yet. The tree shows what the agent sees each time it takes one."
                        .to_owned()
                }
                Empty::Loading => "Loading the snapshot…".to_owned(),
                Empty::Failed(why) => format!("The snapshot can't be shown: {why}"),
            };
            return panel.child(
                div()
                    .p(px(12.))
                    .text_size(px(12.))
                    .text_color(theme.muted)
                    .child(words),
            );
        }
    };
    let rows = elements::rows(snapshot, inspect.wrappers, &inspect.folded);
    let trail: HashSet<NodeIndex> = inspect
        .picked
        .map(|picked| elements::ancestors(snapshot, picked).into_iter().collect())
        .unwrap_or_default();

    let toggled = act.clone();
    let wrappers = inspect.wrappers;
    let options = div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .px(px(12.))
        .pb(px(6.))
        .text_size(px(12.))
        .text_color(theme.muted)
        .child(
            div()
                .id(SharedString::from(format!("{id}-wrappers")))
                .flex()
                .items_center()
                .gap(px(6.))
                .cursor_pointer()
                .child(
                    div()
                        .size(px(13.))
                        .border_1()
                        .border_color(theme.line_strong)
                        .map(|check| match wrappers {
                            Wrappers::Show => {
                                check.bg(theme.accent_fill).border_color(theme.accent_fill)
                            }
                            Wrappers::Hide => check,
                        }),
                )
                .child("Unnamed groups")
                .on_click(move |_, window, cx| {
                    let next = match wrappers {
                        Wrappers::Show => Wrappers::Hide,
                        Wrappers::Hide => Wrappers::Show,
                    };
                    toggled(Pointing::Wrappers(next), window, cx);
                }),
        )
        .child(
            div()
                .text_color(theme.faint)
                .whitespace_nowrap()
                .child(format!("{} of {}", rows.len(), snapshot.tree.len())),
        );

    let list = div()
        .id(SharedString::from(format!("{id}-rows")))
        .flex_1()
        .min_h_0()
        .border_t_1()
        .border_color(theme.line)
        .py(px(2.))
        .overflow_y_scroll()
        .track_scroll(&inspect.list)
        .children(rows.iter().map(|row| {
            row_element(
                id,
                snapshot,
                row,
                named.get(row.index),
                inspect,
                trail.contains(&row.index),
                theme,
                act,
            )
        }));

    let detail = inspect.picked.and_then(|picked| {
        let node = snapshot.tree.get(picked)?;
        let line = |term: &'static str, words: String| {
            div()
                .flex()
                .gap(px(10.))
                .child(
                    div()
                        .w(px(64.))
                        .flex_none()
                        .text_color(theme.faint)
                        .child(term),
                )
                .child(div().min_w_0().text_color(theme.text).child(words))
        };
        let frame = elements::frame(node).map_or_else(
            || "none".to_owned(),
            |frame| {
                format!(
                    "{:.0}, {:.0} · {:.0}×{:.0} pt",
                    frame.left(),
                    frame.top(),
                    frame.width(),
                    frame.height()
                )
            },
        );
        let mut detail = div()
            .flex()
            .flex_col()
            .gap(px(3.))
            .px(px(12.))
            .py(px(8.))
            .border_t_1()
            .border_color(theme.line)
            .text_size(px(12.))
            .child(line("Role", node.role.as_ax().to_owned()))
            .child(line(
                "Index",
                format!("{} of snapshot {}", picked.get(), snapshot.id.get()),
            ));
        match named.get(picked) {
            Some(r) => detail = detail.child(line("Ref", r.to_string())),
            None if named.known() => {
                detail = detail.child(line("Ref", "none: not shown to the agent".to_owned()));
            }
            None => {}
        }
        if let Some(identifier) = &node.identifier {
            detail = detail.child(line("Identifier", identifier.to_string()));
        }
        detail = detail.child(line("Frame", frame));
        if !node.actions.is_empty() {
            let actions: Vec<&str> = node
                .actions
                .iter()
                .map(|action| action.as_ax().trim_start_matches("AX"))
                .collect();
            detail = detail.child(line("Actions", actions.join(", ")));
        }
        Some(detail)
    });

    panel.child(options).child(list).children(detail)
}

/// How far each level is indented, and the gutter before the first.
const INDENT: f32 = 16.;
const GUTTER: f32 = 6.;
const ROW: f32 = 24.;

#[expect(clippy::too_many_arguments, reason = "one row's whole state, drawn")]
fn row_element(
    id: &str,
    snapshot: &AxSnapshot,
    row: &Row,
    named: Option<Ref>,
    inspect: &Inspect,
    on_trail: bool,
    theme: &Theme,
    act: &Act,
) -> impl IntoElement {
    let Some(node) = snapshot.tree.get(row.index) else {
        return div().id(SharedString::from(format!(
            "{id}-missing-{}",
            row.index.get()
        )));
    };
    let kind = Kind::of(&node.role);
    let ink = colour(kind);
    let branch_ink = theme.muted.opacity(0.4);
    let index = row.index;
    let mut element = div()
        .id(SharedString::from(format!("{id}-row-{}", index.get())))
        .relative()
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(ROW))
        .pl(px(GUTTER + row.depth as f32 * INDENT))
        .pr(px(8.))
        .text_size(px(12.))
        .whitespace_nowrap()
        .overflow_hidden()
        .map(|element| {
            if inspect.picked == Some(index) {
                element.bg(theme.accent_soft)
            } else if inspect.pointed == Some(index) {
                element.bg(theme.hover)
            } else {
                element
            }
        });
    // The lines that join an element to its parent, as a file browser draws them.
    for (column, carries) in row.lines.iter().enumerate() {
        if *carries {
            element = element.child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(GUTTER + column as f32 * INDENT + INDENT / 2.))
                    .w(px(1.))
                    .bg(branch_ink),
            );
        }
    }
    if row.depth > 0 {
        let x = GUTTER + (row.depth - 1) as f32 * INDENT + INDENT / 2.;
        element = element
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(x))
                    .w(px(1.))
                    .h(px(if row.last { ROW / 2. } else { ROW }))
                    .bg(branch_ink),
            )
            .child(
                div()
                    .absolute()
                    .top(px(ROW / 2.))
                    .left(px(x))
                    .w(px(INDENT / 2. - 2.))
                    .h(px(1.))
                    .bg(branch_ink),
            );
    }
    let marker: AnyElement =
        if row.branch {
            let folded = act.clone();
            div()
                .id(SharedString::from(format!("{id}-fold-{}", index.get())))
                .flex()
                .items_center()
                .justify_center()
                .size(px(14.))
                .hover(|chevron| chevron.bg(theme.pressed))
                .child(Icon::ChevronRight.svg(12., ink).with_transformation(
                    Transformation::rotate(Radians(if row.open { FRAC_PI_2 } else { 0. })),
                ))
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    folded(Pointing::Fold(index), window, cx);
                })
                .into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .justify_center()
                .size(px(14.))
                .child(div().size(px(7.)).map(|dot| match kind {
                    Kind::Container => dot.border_1().border_color(ink),
                    Kind::Window
                    | Kind::Control
                    | Kind::Field
                    | Kind::Text
                    | Kind::Menu
                    | Kind::Other => dot.bg(ink),
                }))
                .into_any_element()
        };
    element = element.child(marker);
    // The ref the agent's model knows it by, as its listing writes it.
    if let Some(r) = named {
        element = element.child(
            div()
                .flex_none()
                .font_family(MONO)
                .text_size(px(10.5))
                .text_color(theme.accent)
                .child(r.to_string()),
        );
    }
    element = element.child(
        div()
            .text_color(if on_trail { theme.text } else { theme.muted })
            .when(on_trail, |role| role.font_weight(FontWeight::MEDIUM))
            .child(node.role.label().to_owned()),
    );
    if let Some(name) = node.name() {
        element = element.child(
            div()
                .min_w_0()
                .truncate()
                .text_color(theme.text)
                .when(on_trail, |name| name.font_weight(FontWeight::MEDIUM))
                .child(elements::one_line(name)),
        );
    }
    if let Some(value) = elements::value(node) {
        element = element.child(
            div()
                .min_w_0()
                .truncate()
                .text_color(theme.faint)
                .child(format!("= {}", elements::one_line(&value))),
        );
    }
    if row.branch && !row.open {
        element = element.child(
            div()
                .px(px(5.))
                .border_1()
                .border_color(theme.line_strong)
                .text_size(px(10.))
                .text_color(theme.faint)
                .child(snapshot.tree.children(index).len().to_string()),
        );
    }
    let (hovered, picked) = (act.clone(), act.clone());
    let picked_now = inspect.picked;
    element
        .on_hover(move |inside, window, cx| {
            hovered(
                if *inside {
                    Pointing::Hover(Some(index))
                } else {
                    Pointing::Leave(index)
                },
                window,
                cx,
            );
        })
        .on_click(move |_, window, cx| {
            let pick = (picked_now != Some(index)).then_some(index);
            picked(Pointing::Pick(pick), window, cx);
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crossing_rows_keeps_the_new_rows_highlight_whatever_the_order() {
        let (parent, child) = (NodeIndex::new(1), NodeIndex::new(2));
        let mut inspect = Inspect::new();
        // The child's row is entered before the parent's is left.
        inspect.apply(Pointing::Hover(Some(parent)), None);
        inspect.apply(Pointing::Hover(Some(child)), None);
        inspect.apply(Pointing::Leave(parent), None);
        assert_eq!(inspect.pointed, Some(child));
        // Leaving the row that has it clears it.
        inspect.apply(Pointing::Leave(child), None);
        assert_eq!(inspect.pointed, None);
    }
}
