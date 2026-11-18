//! An accessibility snapshot as a person reads it beside the screen: a tree
//! of elements, each with its kind, its name and where it is on the
//! display. Pure: the core's checked tree in; rows, kinds and the element
//! under a point out.

use std::collections::{HashMap, HashSet};

use doppel_desktop::ax::refs::{Ref, SnapshotRefs};
use doppel_desktop::ax::tree::{AxNode, AxSnapshot, AxValue, NodeIndex, Role};
use doppel_desktop::geometry::{Point, Rect};

/// The refs the agent's model was given for a snapshot's elements, by the
/// node each names. Empty when the agent named none, or named another
/// snapshot's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Named {
    refs: HashMap<NodeIndex, Ref>,
    /// The agent named this snapshot's elements, even if it named none.
    known: bool,
}

impl Named {
    pub fn of(snapshot: &AxSnapshot, refs: Option<&SnapshotRefs>) -> Self {
        match refs.filter(|refs| refs.snapshot() == snapshot.id) {
            Some(refs) => Self {
                refs: refs.refs().collect(),
                known: true,
            },
            None => Self::default(),
        }
    }

    pub fn get(&self, node: NodeIndex) -> Option<Ref> {
        self.refs.get(&node).copied()
    }

    /// Whether the agent named this snapshot's elements: an element
    /// without a ref then wasn't shown to its model.
    pub const fn known(&self) -> bool {
        self.known
    }
}

/// What an element is, for its colour and for which ones are outlined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Window,
    Control,
    Field,
    Text,
    Container,
    Menu,
    /// A role this build doesn't know.
    Other,
}

impl Kind {
    pub fn of(role: &Role) -> Self {
        match role {
            Role::Application | Role::Window | Role::Sheet | Role::Popover => Self::Window,
            Role::Button
            | Role::CheckBox
            | Role::RadioButton
            | Role::PopUpButton
            | Role::MenuButton
            | Role::Link
            | Role::Slider
            | Role::Incrementor
            | Role::DisclosureTriangle
            | Role::ColorWell => Self::Control,
            Role::ComboBox | Role::TextField | Role::TextArea => Self::Field,
            Role::StaticText
            | Role::Image
            | Role::Heading
            | Role::ProgressIndicator
            | Role::BusyIndicator
            | Role::ValueIndicator
            | Role::LevelIndicator => Self::Text,
            Role::Toolbar
            | Role::Group
            | Role::ScrollArea
            | Role::SplitGroup
            | Role::Splitter
            | Role::ScrollBar
            | Role::LayoutArea
            | Role::LayoutItem
            | Role::TabGroup
            | Role::WebArea
            | Role::List
            | Role::RadioGroup
            | Role::Table
            | Role::Outline
            | Role::Browser
            | Role::Row
            | Role::Column
            | Role::Cell
            | Role::Ruler
            | Role::RulerMarker => Self::Container,
            Role::MenuBar | Role::MenuBarItem | Role::Menu | Role::MenuItem => Self::Menu,
            Role::Unknown(_) => Self::Other,
        }
    }

    /// Outlined over the screen all the time: what an agent can act on.
    /// The rest show while pointed at or picked.
    pub const fn outlined(self) -> bool {
        match self {
            Self::Control | Self::Field | Self::Menu => true,
            Self::Window | Self::Text | Self::Container | Self::Other => false,
        }
    }
}

/// Whether unnamed containers are shown, or their children in their place,
/// as in the agent's listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrappers {
    Show,
    Hide,
}

/// An unnamed container: the agent's listing shows its children in its place.
pub fn wrapper(node: &AxNode) -> bool {
    Kind::of(&node.role) == Kind::Container && node.name().is_none()
}

/// What a person would call an element: its role, and its name if it has one.
pub fn label(node: &AxNode) -> String {
    match node.name() {
        Some(name) => format!("{} “{name}”", node.role.label()),
        None => node.role.label().to_owned(),
    }
}

/// Its value as text, when it has one that isn't its name.
pub fn value(node: &AxNode) -> Option<String> {
    let text = match node.value.as_ref()? {
        AxValue::Text(text) => text.clone(),
        AxValue::Number(number) => number.to_string(),
        AxValue::Boolean(true) => "on".to_owned(),
        AxValue::Boolean(false) => "off".to_owned(),
    };
    (!text.trim().is_empty() && node.name() != Some(text.trim())).then_some(text)
}

/// The most of a name or value a tree row shows.
const ONE_LINE_MAX: usize = 120;

/// `text` as one line for a tree row: its runs of spaces and line breaks
/// each one space, cut with "…" past [`ONE_LINE_MAX`] characters. A text
/// area's value is its whole contents, many lines of it.
pub fn one_line(text: &str) -> String {
    // Words only until past the cut: a text area may hold pages.
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
        if line.chars().count() > ONE_LINE_MAX {
            let mut cut: String = line.chars().take(ONE_LINE_MAX).collect();
            cut.push('…');
            return cut;
        }
    }
    line
}

/// Where an element is, in display points; `None` when it has no area.
pub fn frame(node: &AxNode) -> Option<Rect> {
    node.frame.filter(|frame| frame.has_area())
}

/// A row of the tree panel: an element, where it sits in the tree shown,
/// and how to draw its branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub index: NodeIndex,
    pub depth: usize,
    /// Its parent in the tree shown; `None` at the top.
    pub parent: Option<NodeIndex>,
    /// It has children shown under it, so it can fold.
    pub branch: bool,
    pub open: bool,
    /// The last of its siblings: its branch line stops here.
    pub last: bool,
    /// The lines passing beside it, one per column from the left, under
    /// each ancestor above its parent: whether that ancestor's line carries
    /// on past this row. Its own link to its parent is the elbow at column
    /// `depth - 1`.
    pub lines: Vec<bool>,
}

/// An element's children as the tree shows them: with wrappers hidden, a
/// wrapper's children in its place.
fn shown_children(snapshot: &AxSnapshot, index: NodeIndex, wrappers: Wrappers) -> Vec<NodeIndex> {
    snapshot
        .tree
        .children(index)
        .iter()
        .flat_map(|&child| {
            let lifted =
                wrappers == Wrappers::Hide && snapshot.tree.get(child).is_some_and(wrapper);
            if lifted {
                shown_children(snapshot, child, wrappers)
            } else {
                vec![child]
            }
        })
        .collect()
}

/// The rows the tree panel shows, in order. A folded element's
/// descendants aren't shown.
pub fn rows(snapshot: &AxSnapshot, wrappers: Wrappers, folded: &HashSet<NodeIndex>) -> Vec<Row> {
    fn visit(
        snapshot: &AxSnapshot,
        wrappers: Wrappers,
        folded: &HashSet<NodeIndex>,
        at: (NodeIndex, Option<NodeIndex>, usize, bool),
        lines: Vec<bool>,
        out: &mut Vec<Row>,
    ) {
        let (index, parent, depth, last) = at;
        let children = shown_children(snapshot, index, wrappers);
        let open = !folded.contains(&index);
        // A top element has no line of its own to pass on.
        let below = if depth == 0 {
            Vec::new()
        } else {
            let mut below = lines.clone();
            below.push(!last);
            below
        };
        out.push(Row {
            index,
            depth,
            parent,
            branch: !children.is_empty(),
            open,
            last,
            lines,
        });
        if open {
            let count = children.len();
            for (at, child) in children.into_iter().enumerate() {
                visit(
                    snapshot,
                    wrappers,
                    folded,
                    (child, Some(index), depth + 1, at + 1 == count),
                    below.clone(),
                    out,
                );
            }
        }
    }
    let root = NodeIndex::ROOT;
    let top = match snapshot.tree.get(root) {
        Some(node) if wrappers == Wrappers::Hide && wrapper(node) => {
            shown_children(snapshot, root, wrappers)
        }
        Some(_) => vec![root],
        None => Vec::new(),
    };
    let mut out = Vec::new();
    let count = top.len();
    for (at, index) in top.into_iter().enumerate() {
        visit(
            snapshot,
            wrappers,
            folded,
            (index, None, 0, at + 1 == count),
            Vec::new(),
            &mut out,
        );
    }
    out
}

/// An element's ancestors, nearest first.
pub fn ancestors(snapshot: &AxSnapshot, index: NodeIndex) -> Vec<NodeIndex> {
    std::iter::successors(snapshot.tree.parent(index), |&at| snapshot.tree.parent(at)).collect()
}

/// The folds with `index`'s ancestors opened, so it shows.
pub fn reveal(
    snapshot: &AxSnapshot,
    folded: &HashSet<NodeIndex>,
    index: NodeIndex,
) -> HashSet<NodeIndex> {
    let mut opened = folded.clone();
    for ancestor in ancestors(snapshot, index) {
        opened.remove(&ancestor);
    }
    opened
}

/// The elements folded at first: menus, which are long and rarely what the
/// agent looked at.
pub fn initially_folded(snapshot: &AxSnapshot) -> HashSet<NodeIndex> {
    snapshot
        .tree
        .iter()
        .filter(|(index, node)| {
            Kind::of(&node.role) == Kind::Menu && !snapshot.tree.children(*index).is_empty()
        })
        .map(|(index, _)| index)
        .collect()
}

/// The part of an element's frame inside every enclosing element's: what's
/// on screen of it. A text area's frame is its whole document, scrolled
/// mostly out of its window. `None` when it has no frame, or none of it
/// is seen.
pub fn seen(snapshot: &AxSnapshot, index: NodeIndex) -> Option<Rect> {
    let own = frame(snapshot.tree.get(index)?)?;
    ancestors(snapshot, index)
        .into_iter()
        .filter_map(|above| snapshot.tree.get(above).and_then(frame))
        .try_fold(own, Rect::intersection)
}

/// The element under a display point: the deepest one whose frame holds
/// it, the smallest of those on a tie, and the latest in the snapshot
/// (drawn in front) when they are the same size too. Wrappers are passed
/// over when they are hidden.
pub fn element_at(snapshot: &AxSnapshot, at: Point, wrappers: Wrappers) -> Option<NodeIndex> {
    let depth = |index: NodeIndex| ancestors(snapshot, index).len();
    snapshot
        .tree
        .iter()
        .filter(|(_, node)| !(wrappers == Wrappers::Hide && wrapper(node)))
        .filter_map(|(index, _)| {
            let frame = seen(snapshot, index)?;
            frame
                .contains(at)
                .then(|| (index, depth(index), frame.width() * frame.height()))
        })
        // `max_by` keeps the last of equals: the latest in the snapshot.
        .max_by(|a, b| a.1.cmp(&b.1).then(b.2.total_cmp(&a.2)))
        .map(|(index, _, _)| index)
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_rows_text_is_one_short_line() {
        assert_eq!(
            one_line("Last login:\n  Fri\tNov 13\n"),
            "Last login: Fri Nov 13"
        );
        let long = "word ".repeat(100);
        let line = one_line(&long);
        assert!(line.ends_with('…'));
        assert_eq!(line.chars().count(), ONE_LINE_MAX + 1);
        assert_eq!(one_line("short"), "short");
    }

    use super::*;
    use doppel_desktop::agent::{ProcessId, RunningApp};
    use doppel_desktop::ax::tree::{AxText, AxTree, SnapshotId, States, Truncation};

    fn node(role: Role, title: &str, frame: Option<(f64, f64, f64, f64)>) -> AxNode {
        AxNode {
            role,
            subrole: None,
            title: AxText::new(title),
            description: None,
            identifier: None,
            value: None,
            frame: frame.and_then(|(x, y, width, height)| Rect::new(x, y, width, height).ok()),
            states: States::default(),
            actions: Vec::new(),
        }
    }

    /// An app holding a window (which holds an unnamed group with a button
    /// and a field) and a menu bar with one menu.
    fn snapshot() -> AxSnapshot {
        let at = |index| Some(NodeIndex::new(index));
        let tree = AxTree::new(vec![
            (None, node(Role::Application, "Calculator", None)),
            (
                at(0),
                node(
                    Role::Window,
                    "Calculator",
                    Some((100.0, 80.0, 400.0, 300.0)),
                ),
            ),
            (
                at(1),
                node(Role::Group, "", Some((110.0, 120.0, 380.0, 240.0))),
            ),
            (
                at(2),
                node(Role::Button, "Equals", Some((400.0, 300.0, 48.0, 48.0))),
            ),
            (
                at(2),
                node(Role::TextField, "Amount", Some((120.0, 130.0, 200.0, 24.0))),
            ),
            (
                at(0),
                node(Role::MenuBar, "", Some((0.0, 0.0, 1280.0, 24.0))),
            ),
            (
                at(5),
                node(Role::MenuBarItem, "File", Some((10.0, 0.0, 40.0, 24.0))),
            ),
        ])
        .unwrap();
        AxSnapshot {
            id: SnapshotId::new(3),
            app: RunningApp {
                pid: ProcessId::new(412),
                bundle_id: None,
                name: "Calculator".into(),
            },
            tree,
            truncation: Truncation::Complete,
        }
    }

    fn at(index: u32) -> NodeIndex {
        NodeIndex::new(index)
    }

    #[test]
    fn what_is_seen_of_an_element_is_inside_every_enclosing_one() {
        let up = |index| Some(NodeIndex::new(index));
        let tree = AxTree::new(vec![
            (None, node(Role::Application, "Terminal", None)),
            (
                up(0),
                node(Role::Window, "zsh", Some((100.0, 300.0, 800.0, 400.0))),
            ),
            // A scroll area with no frame bounds nothing more.
            (up(1), node(Role::ScrollArea, "", None)),
            // The terminal's text: its whole scrollback, mostly above.
            (
                up(2),
                node(
                    Role::TextArea,
                    "shell",
                    Some((100.0, -2000.0, 780.0, 2700.0)),
                ),
            ),
            // Scrolled wholly out of the window.
            (
                up(3),
                node(Role::StaticText, "old", Some((120.0, -1500.0, 100.0, 20.0))),
            ),
        ])
        .unwrap();
        let snapshot = AxSnapshot { tree, ..snapshot() };
        assert_eq!(
            seen(&snapshot, at(3)),
            Rect::new(100.0, 300.0, 780.0, 400.0).ok()
        );
        assert_eq!(seen(&snapshot, at(4)), None);
        assert_eq!(seen(&snapshot, at(2)), None);
        // Pointing above the window finds nothing there.
        assert_eq!(
            element_at(&snapshot, Point::new(300.0, 100.0).unwrap(), Wrappers::Show),
            None
        );
        assert_eq!(
            element_at(&snapshot, Point::new(300.0, 500.0).unwrap(), Wrappers::Show),
            Some(at(3))
        );
    }

    #[test]
    fn refs_are_found_by_node_and_only_for_their_own_snapshot() {
        use doppel_desktop::ax::refs::Generation;
        let shown = snapshot();
        let refs = |id| {
            SnapshotRefs::new(
                SnapshotId::new(id),
                Generation::new(4).unwrap(),
                vec![at(3), at(4)],
            )
        };
        let named = Named::of(&shown, Some(&refs(3)));
        assert!(named.known());
        assert_eq!(named.get(at(4)).map(|r| r.to_string()), Some("s4e2".into()));
        assert_eq!(named.get(at(1)), None);
        let other = Named::of(&shown, Some(&refs(9)));
        assert!(!other.known());
        assert_eq!(other.get(at(3)), None);
        assert_eq!(Named::of(&shown, None), Named::default());
    }

    #[test]
    fn rows_draw_each_branch_and_lift_wrappers_when_hidden() {
        let snapshot = snapshot();
        let shown = rows(&snapshot, Wrappers::Show, &HashSet::new());
        let row = |index| shown.iter().find(|row| row.index == at(index)).unwrap();
        assert_eq!(shown.len(), 7);
        assert!(row(0).branch && row(0).last && row(0).lines.is_empty());
        assert_eq!((row(1).depth, row(1).last), (1, false));
        // Beside the button: the app's line carries on (the menu bar is to
        // come after the window), the window's doesn't (the group is its only child).
        assert_eq!(row(3).lines, vec![true, false]);
        assert!(!row(3).last && row(4).last);
        let hidden = rows(&snapshot, Wrappers::Hide, &HashSet::new());
        let button = hidden.iter().find(|row| row.index == at(3)).unwrap();
        assert_eq!((button.parent, button.depth), (Some(at(1)), 2));
        assert!(!hidden.iter().any(|row| row.index == at(2)));
    }

    #[test]
    fn a_folded_branch_hides_what_is_under_it_and_menus_start_folded() {
        let snapshot = snapshot();
        let folded = initially_folded(&snapshot);
        assert_eq!(folded, HashSet::from([at(5)]));
        let shown: Vec<u32> = rows(&snapshot, Wrappers::Show, &folded)
            .iter()
            .map(|row| row.index.get())
            .collect();
        assert_eq!(shown, vec![0, 1, 2, 3, 4, 5]);
        let opened = reveal(&snapshot, &HashSet::from([at(1), at(5)]), at(3));
        assert_eq!(opened, HashSet::from([at(5)]));
    }

    #[test]
    fn the_element_under_a_point_is_the_deepest_there() {
        let snapshot = snapshot();
        assert_eq!(
            element_at(&snapshot, Point::new(420.0, 320.0).unwrap(), Wrappers::Show),
            Some(at(3))
        );
        assert_eq!(
            element_at(&snapshot, Point::new(300.0, 340.0).unwrap(), Wrappers::Show),
            Some(at(2))
        );
        assert_eq!(
            element_at(&snapshot, Point::new(300.0, 340.0).unwrap(), Wrappers::Hide),
            Some(at(1))
        );
        assert_eq!(
            element_at(&snapshot, Point::new(20.0, 10.0).unwrap(), Wrappers::Show),
            Some(at(6))
        );
        assert_eq!(
            element_at(&snapshot, Point::new(900.0, 700.0).unwrap(), Wrappers::Show),
            None
        );
    }

    #[test]
    fn kinds_and_labels_read_as_a_person_would() {
        let snapshot = snapshot();
        let button = snapshot.tree.get(at(3)).unwrap();
        assert_eq!(Kind::of(&button.role), Kind::Control);
        assert!(Kind::Control.outlined() && !Kind::Container.outlined());
        assert_eq!(label(button), "button “Equals”");
        assert!(wrapper(snapshot.tree.get(at(2)).unwrap()));
        assert_eq!(Kind::of(&Role::parse("AXHologram")), Kind::Other);
    }
}
