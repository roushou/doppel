//! Compaction: a raw AX tree in, a short text listing with refs out.
//!
//! A raw window dump is ~20k tokens, mostly wrapper groups
//! (web content) or menus (native apps). Compaction:
//! - drops chrome with nothing to read or act on (scroll bars, splitters);
//! - replaces unnamed wrappers (groups, scroll areas) by their children;
//! - folds a lone text child into its parent's name, and drops text that
//!   repeats its parent's name;
//! - gives refs only to elements that can be acted on;
//! - stops at a token budget, and says so.

use std::fmt::{self, Write as _};

use super::changes::{Carried, Entry, Known};
use super::refs::{Generation, Ref, RefTable, RefTarget};
use super::tree::{AxSnapshot, AxValue, ElementTarget, NodeIndex, Role, RoleKind};

/// An estimated token count: one token per four characters, rounded up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Tokens(u32);

impl Tokens {
    /// Below this, a listing can't show anything useful.
    pub const MIN_BUDGET: Self = Self(200);
    pub const DEFAULT_BUDGET: Self = Self(4_000);

    #[must_use]
    pub fn estimate(text: &str) -> Self {
        let chars = u32::try_from(text.chars().count()).unwrap_or(u32::MAX);
        Self(chars.div_ceil(4))
    }

    /// A budget of `n` tokens.
    ///
    /// # Errors
    /// Budgets below [`Self::MIN_BUDGET`].
    pub fn budget(n: u32) -> Result<Self, BudgetError> {
        if n < Self::MIN_BUDGET.0 {
            Err(BudgetError(n))
        } else {
            Ok(Self(n))
        }
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for Tokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} tokens", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a token budget must be at least 200 (got {0})")]
pub struct BudgetError(u32);

/// How much a listing may show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionPolicy {
    pub budget: Tokens,
    /// Longer names and values are cut to this many characters.
    pub max_text: usize,
    pub show: Show,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self {
            budget: Tokens::DEFAULT_BUDGET,
            max_text: 160,
            show: Show::All,
        }
    }
}

/// What a listing lists: every element, or what changed since the model's
/// previous listing of the same app ([`CompactSnapshot::changes`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Show {
    All,
    Changes,
}

/// A snapshot as a model sees it, with the refs it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactSnapshot {
    pub text: String,
    /// The refs this listing issued.
    pub refs: RefTable,
    /// Refs from earlier listings that still name an element: it is as it was.
    pub carried: Carried,
    /// What the model now knows of the UI, for the next listing of changes.
    pub known: Known,
    /// Elements listed.
    pub shown: usize,
    /// Elements cut by the token budget.
    pub omitted: usize,
    pub tokens: Tokens,
}

/// An element that survived compaction.
#[derive(Debug, Clone, PartialEq)]
struct Item {
    role: Role,
    name: Option<String>,
    value: Option<String>,
    states: Vec<&'static str>,
    /// Set when the element gets a ref.
    target: Option<NodeIndex>,
    children: Vec<Self>,
}

impl Item {
    fn count(&self) -> usize {
        1 + self.children.iter().map(Self::count).sum::<usize>()
    }

    fn is_bare_text(&self) -> bool {
        self.role == Role::StaticText && self.children.is_empty()
    }

    /// Text, or an empty wrapper, that only repeats its parent's name (a web
    /// radio's inner group). A control that repeats it stays: a combo box's
    /// own button is pressable.
    fn repeats(&self, parent: Option<&str>) -> bool {
        let empty_wrapper = self.role.kind() == RoleKind::Wrapper
            && self.children.is_empty()
            && self.value.is_none();
        (self.is_bare_text() || empty_wrapper)
            && self.name.is_some()
            && self.name.as_deref() == parent
    }
}

impl CompactSnapshot {
    /// Every element, as far as the budget allows.
    #[must_use]
    pub fn build(snapshot: &AxSnapshot, generation: Generation, policy: CompactionPolicy) -> Self {
        Self::listing(snapshot, generation, policy, None)
    }

    /// The elements of a snapshot, in listing order, each with its parent's place.
    pub(super) fn entries(snapshot: &AxSnapshot, policy: CompactionPolicy) -> Vec<Entry> {
        Self::flat(&Self::items(snapshot, NodeIndex::ROOT, true, policy))
    }

    fn flat(items: &[Item]) -> Vec<Entry> {
        fn flatten(item: &Item, parent: Option<usize>, out: &mut Vec<Entry>) {
            let at = out.len();
            out.push(Entry {
                role: item.role.clone(),
                name: item.name.clone(),
                value: item.value.clone(),
                states: item.states.clone(),
                target: item.target,
                parent,
            });
            for child in &item.children {
                flatten(child, Some(at), out);
            }
        }
        let mut out = Vec::new();
        for item in items {
            flatten(item, None, &mut out);
        }
        out
    }

    /// Every element, with a line under the header saying why, when `note` says.
    pub(super) fn listing(
        snapshot: &AxSnapshot,
        generation: Generation,
        policy: CompactionPolicy,
        note: Option<&str>,
    ) -> Self {
        let items = Self::items(snapshot, NodeIndex::ROOT, true, policy);
        let total: usize = items.iter().map(Item::count).sum();

        let mut text = String::new();
        let app = &snapshot.app;
        let _ = write!(text, "# {}", app.name);
        if let Some(id) = &app.bundle_id {
            let _ = write!(text, " ({id})");
        }
        let _ = writeln!(
            text,
            " · snapshot {generation} · {} AX nodes",
            snapshot.tree.len()
        );
        if let Some(note) = note {
            let _ = writeln!(text, "# {note}");
        }
        if !snapshot.truncation.is_complete() {
            let _ = writeln!(
                text,
                "# partial: the agent {}; later parts of the UI are missing",
                snapshot.truncation
            );
        }

        let mut out = Listing {
            snapshot,
            refs: RefTable::new(generation),
            text,
            // Room for the footer that says what was cut.
            limit: policy.budget.0.saturating_sub(40),
            used: 0,
            shown: 0,
            full: false,
            issued: Vec::with_capacity(total),
        };
        out.used = Tokens::estimate(&out.text).0;
        for item in &items {
            out.render(item, 0);
        }
        let omitted = total - out.shown;
        if omitted > 0 {
            let _ = writeln!(
                out.text,
                "… {omitted} more elements not shown ({} budget); snapshot a narrower scope or raise max_tokens",
                policy.budget
            );
        }
        let tokens = Tokens::estimate(&out.text);
        // Elements cut by the budget have no ref the model holds.
        let mut issued = out.issued;
        issued.resize(total, None);
        let entries = Self::flat(&items);
        Self {
            text: out.text,
            known: Known::new(generation, snapshot.app.pid, entries, issued),
            refs: out.refs,
            carried: Carried::default(),
            shown: out.shown,
            omitted,
            tokens,
        }
    }

    /// The compacted form of one node: itself, or its children when it's a
    /// wrapper with nothing to say.
    fn items(
        snapshot: &AxSnapshot,
        index: NodeIndex,
        is_root: bool,
        policy: CompactionPolicy,
    ) -> Vec<Item> {
        let tree = &snapshot.tree;
        let Some(node) = tree.get(index) else {
            return Vec::new();
        };
        let kind = node.role.kind();
        if kind == RoleKind::Noise {
            return Vec::new();
        }
        let mut children: Vec<Item> = tree
            .children(index)
            .iter()
            .flat_map(|&child| Self::items(snapshot, child, false, policy))
            .collect();

        let name = node.name().map(|n| Self::excerpt(n, policy.max_text));
        let (value, mut states) =
            Self::value(&node.role, node.value.as_ref(), name.as_deref(), policy);
        let actionable = node.is_actionable();
        let primary = node.has_primary_action();
        let keep = is_root
            || match kind {
                RoleKind::Control | RoleKind::Landmark => true,
                RoleKind::Content => name.is_some() || value.is_some() || primary,
                RoleKind::Wrapper => name.is_some() || primary,
                RoleKind::Noise => false,
            };
        if !keep {
            return children;
        }
        // A wrapper that says nothing and holds nothing can't be used, even if
        // it is clickable (a web app's bare <div onclick>).
        if !is_root
            && name.is_none()
            && value.is_none()
            && children.is_empty()
            && matches!(kind, RoleKind::Wrapper | RoleKind::Content)
        {
            return children;
        }
        // Icon-font glyphs (Private Use Area characters) read as nothing.
        if node.role == Role::StaticText && name.as_deref().is_some_and(Self::is_glyphs) {
            return children;
        }

        let s = node.states;
        if s.focused == Some(true) {
            states.push("focused");
        }
        if s.selected == Some(true) {
            states.push("selected");
        }
        match s.expanded {
            Some(true) => states.push("expanded"),
            Some(false) if node.role.can_expand() => states.push("collapsed"),
            Some(false) | None => {}
        }
        if s.enabled == Some(false) {
            states.push("disabled");
        }

        // An unnamed element with exactly one text child (beside icons, say)
        // is named by that text: a sidebar row, a link, a labelled button.
        let mut name = name;
        let texts: Vec<usize> = (0..children.len())
            .filter(|&i| children[i].is_bare_text())
            .collect();
        if name.is_none()
            && let [only] = texts.as_slice()
        {
            name = children.remove(*only).name;
        }
        children.retain(|child| !child.repeats(name.as_deref()));

        let gets_ref = kind == RoleKind::Control
            || actionable
            || matches!(node.role, Role::Window | Role::Sheet);
        vec![Item {
            role: node.role.clone(),
            name,
            value,
            states,
            target: gets_ref.then_some(index),
            children,
        }]
    }

    /// How a value reads: checkable controls show it as a state, text and
    /// numbers as `= value`. Text that repeats the name is left out.
    fn value(
        role: &Role,
        value: Option<&AxValue>,
        name: Option<&str>,
        policy: CompactionPolicy,
    ) -> (Option<String>, Vec<&'static str>) {
        let checkable = matches!(
            role,
            Role::CheckBox | Role::RadioButton | Role::DisclosureTriangle
        );
        match value {
            None => (None, Vec::new()),
            Some(AxValue::Text(_)) if *role == Role::StaticText => (None, Vec::new()),
            Some(AxValue::Text(text)) => {
                let trimmed = text.trim();
                if trimmed.is_empty() || Some(trimmed) == name {
                    return (None, Vec::new());
                }
                let mut shown = format!("{:?}", Self::excerpt(trimmed, policy.max_text));
                let chars = trimmed.chars().count();
                if chars > policy.max_text {
                    let _ = write!(shown, " ({chars} chars)");
                }
                (Some(shown), Vec::new())
            }
            Some(AxValue::Number(n)) if checkable => (None, vec![Self::check_state(*n)]),
            Some(AxValue::Boolean(b)) if checkable => {
                (None, vec![if *b { "checked" } else { "unchecked" }])
            }
            Some(AxValue::Number(n)) => (Some(format!("{n}")), Vec::new()),
            Some(AxValue::Boolean(b)) => (Some(format!("{b}")), Vec::new()),
        }
    }

    fn check_state(n: f64) -> &'static str {
        if n == 0.0 {
            "unchecked"
        } else if n == 1.0 {
            "checked"
        } else {
            "mixed"
        }
    }

    /// Only Private Use Area characters (and spaces): an icon font's glyphs.
    fn is_glyphs(text: &str) -> bool {
        let mut chars = text.chars().filter(|c| !c.is_whitespace()).peekable();
        chars.peek().is_some()
            && chars.all(|c| matches!(c, '\u{E000}'..='\u{F8FF}' | '\u{F0000}'..='\u{10FFFD}'))
    }

    /// At most `max` characters, newlines flattened.
    fn excerpt(text: &str, max: usize) -> String {
        let flat: String = text
            .trim()
            .chars()
            .map(|c| match c {
                '\n' | '\r' => '⏎',
                c if c.is_whitespace() => ' ',
                c => c,
            })
            .collect();
        if flat.chars().count() <= max {
            flat
        } else {
            let mut cut: String = flat.chars().take(max).collect();
            cut.push('…');
            cut
        }
    }
}

/// Rendering state: the text so far and the refs issued for it.
struct Listing<'a> {
    snapshot: &'a AxSnapshot,
    refs: RefTable,
    text: String,
    limit: u32,
    used: u32,
    shown: usize,
    /// Once one line doesn't fit, nothing after it is shown, so the listing
    /// never skips an element and then shows a later one.
    full: bool,
    /// The ref of each element listed, in listing order.
    issued: Vec<Option<Ref>>,
}

impl Listing<'_> {
    fn render(&mut self, item: &Item, depth: usize) {
        if self.full {
            return;
        }
        let mut line = format!("{}- {}", "  ".repeat(depth), item.role);
        if let Some(name) = &item.name {
            let _ = write!(line, " {name:?}");
        }
        if let Some(value) = &item.value {
            let _ = write!(line, " = {value}");
        }
        // Reserve the ref's place before issuing it, so a line that doesn't
        // fit doesn't use up a ref.
        let ref_len: u32 = if item.target.is_some() { 12 } else { 0 };
        let states: String = item.states.iter().map(|s| format!(" {s}")).collect();
        let cost = Tokens::estimate(&line).0 + Tokens::estimate(&states).0 + ref_len / 4 + 1;
        if self.used + cost > self.limit {
            self.full = true;
            return;
        }
        let mut issued = None;
        if let Some(node) = item.target {
            let r = self.refs.issue(RefTarget {
                element: ElementTarget {
                    snapshot: self.snapshot.id,
                    node,
                },
                role: item.role.clone(),
                name: item.name.clone(),
            });
            let _ = write!(line, " [{r}]");
            issued = Some(r);
        }
        self.issued.push(issued);
        line.push_str(&states);
        line.push('\n');
        self.used += cost;
        self.text.push_str(&line);
        self.shown += 1;
        for child in &item.children {
            self.render(child, depth + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{BundleId, ProcessId, RunningApp};
    use crate::ax::tree::tests::node;
    use crate::ax::tree::{ActionName, AxNode, AxText, AxTree, SnapshotId, Truncation};

    fn snapshot(nodes: Vec<(Option<u32>, AxNode)>) -> AxSnapshot {
        AxSnapshot {
            id: SnapshotId::new(500),
            app: RunningApp {
                pid: ProcessId::new(42),
                bundle_id: BundleId::new("com.example.Editor"),
                name: "Editor".into(),
            },
            tree: AxTree::new(
                nodes
                    .into_iter()
                    .map(|(p, n)| (p.map(NodeIndex::new), n))
                    .collect(),
            )
            .unwrap(),
            truncation: Truncation::Complete,
        }
    }

    fn titled(role: &str, title: &str) -> AxNode {
        AxNode {
            title: AxText::new(title),
            ..node(role)
        }
    }

    fn text(value: &str) -> AxNode {
        AxNode {
            value: Some(AxValue::Text(value.into())),
            ..node("AXStaticText")
        }
    }

    fn compact(nodes: Vec<(Option<u32>, AxNode)>) -> CompactSnapshot {
        CompactSnapshot::build(
            &snapshot(nodes),
            Generation::FIRST,
            CompactionPolicy::default(),
        )
    }

    fn body(c: &CompactSnapshot) -> Vec<&str> {
        c.text.lines().skip(1).collect()
    }

    #[test]
    fn wrappers_collapse_and_chrome_disappears() {
        let c = compact(vec![
            (None, titled("AXWindow", "Untitled")),
            (Some(0), node("AXGroup")),
            (Some(1), node("AXScrollArea")),
            (Some(2), titled("AXButton", "Save")),
            (Some(2), node("AXScrollBar")),
            (Some(4), titled("AXValueIndicator", "inside the scroll bar")),
            (Some(0), titled("AXGroup", "Formatting")),
        ]);
        assert_eq!(
            body(&c),
            [
                "- window \"Untitled\" [s1e1]",
                "  - button \"Save\" [s1e2]",
                "  - group \"Formatting\"",
            ]
        );
        assert_eq!(c.refs.len(), 2);
        assert_eq!(c.omitted, 0);
    }

    /// A Chromium node: every element offers a context menu and reports
    /// AXExpanded, whether or not it can expand.
    fn web(mut n: AxNode, actions: &[ActionName]) -> AxNode {
        n.actions = [ActionName::ShowMenu, ActionName::ScrollToVisible]
            .into_iter()
            .chain(actions.iter().cloned())
            .collect();
        n.states.expanded = Some(false);
        n
    }

    #[test]
    fn a_context_menu_alone_keeps_no_wrapper_but_still_earns_a_ref() {
        let c = compact(vec![
            (None, titled("AXWindow", "W")),
            (Some(0), web(node("AXGroup"), &[])),
            (
                Some(1),
                web(titled("AXButton", "Run"), &[ActionName::Press]),
            ),
            (Some(0), web(titled("AXList", "Files"), &[])),
            (
                Some(3),
                web(titled("AXGroup", "Pressable"), &[ActionName::Press]),
            ),
            (Some(4), text("inside")),
        ]);
        assert_eq!(
            body(&c),
            [
                "- window \"W\" [s1e1]",
                // The unnamed group offering only a context menu is gone.
                "  - button \"Run\" [s1e2]",
                // Kept for its name, a context menu still earns a ref (Finder's
                // icon view: New Folder lives there).
                "  - list \"Files\" [s1e3]",
                "    - group \"Pressable\" [s1e4]",
                "      - text \"inside\"",
            ]
        );
    }

    #[test]
    fn collapsed_shows_only_where_something_can_expand() {
        let c = compact(vec![
            (None, titled("AXWindow", "W")),
            (
                Some(0),
                web(titled("AXButton", "Plain"), &[ActionName::Press]),
            ),
            (
                Some(0),
                web(titled("AXPopUpButton", "More"), &[ActionName::Press]),
            ),
            (Some(0), {
                let mut open = titled("AXDisclosureTriangle", "Details");
                open.states.expanded = Some(true);
                open
            }),
        ]);
        assert_eq!(
            body(&c),
            [
                "- window \"W\" [s1e1]",
                "  - button \"Plain\" [s1e2]",
                "  - popup \"More\" [s1e3] collapsed",
                "  - disclosure \"Details\" [s1e4] expanded",
            ]
        );
    }

    #[test]
    fn empty_clickables_icon_glyphs_and_repeated_wrappers_are_dropped() {
        let c = compact(vec![
            (None, titled("AXWindow", "W")),
            // A bare clickable <div>: nothing says what it does.
            (Some(0), web(node("AXGroup"), &[ActionName::Press])),
            (
                Some(0),
                web(titled("AXRadioButton", "Explorer"), &[ActionName::Press]),
            ),
            (
                Some(2),
                web(titled("AXGroup", "Explorer"), &[ActionName::Press]),
            ),
            (
                Some(0),
                web(titled("AXButton", "Close"), &[ActionName::Press]),
            ),
            (Some(4), text("\u{ea9b}")),
            (Some(0), titled("AXComboBox", "font size")),
            (Some(6), titled("AXButton", "font size")),
        ]);
        assert_eq!(
            body(&c),
            [
                "- window \"W\" [s1e1]",
                "  - radio \"Explorer\" [s1e2]",
                "  - button \"Close\" [s1e3]",
                // A control repeating its parent's name stays: it can be pressed.
                "  - combobox \"font size\" [s1e4]",
                "    - button \"font size\" [s1e5]",
            ]
        );
    }

    #[test]
    fn lone_text_becomes_the_name_and_repeated_text_is_dropped() {
        let c = compact(vec![
            (None, titled("AXWindow", "W")),
            (Some(0), node("AXLink")),
            (Some(1), text("Docs")),
            (Some(0), titled("AXButton", "Go")),
            (Some(3), text("Go")),
            (Some(0), text("   ")),
            (Some(0), node("AXImage")),
        ]);
        assert_eq!(
            body(&c),
            [
                "- window \"W\" [s1e1]",
                "  - link \"Docs\" [s1e2]",
                "  - button \"Go\" [s1e3]",
            ]
        );
    }

    #[test]
    fn rows_get_refs_and_take_their_text_beside_icons() {
        let c = compact(vec![
            (None, titled("AXWindow", "W")),
            (Some(0), titled("AXOutline", "sidebar")),
            (Some(1), node("AXRow")),
            (Some(2), node("AXCell")),
            (Some(3), text("iCloud\u{a0}Drive")),
            (Some(3), titled("AXImage", "iCloud")),
            (Some(1), node("AXRow")),
            (Some(6), text("A")),
            (Some(6), text("B")),
        ]);
        assert_eq!(
            body(&c)[1..],
            [
                "  - outline \"sidebar\"",
                "    - row \"iCloud Drive\" [s1e2]",
                "      - image \"iCloud\"",
                // Two texts: neither names the row.
                "    - row [s1e3]",
                "      - text \"A\"",
                "      - text \"B\"",
            ]
        );
    }

    #[test]
    fn values_and_states_read_naturally() {
        let mut field = titled("AXTextField", "Search");
        field.value = Some(AxValue::Text("line one\nline two".into()));
        field.states.focused = Some(true);
        let mut check = titled("AXCheckBox", "Bold");
        check.value = Some(AxValue::Number(1.0));
        check.states.enabled = Some(false);
        let mut slider = titled("AXSlider", "Zoom");
        slider.value = Some(AxValue::Number(0.5));
        let c = compact(vec![
            (None, titled("AXWindow", "W")),
            (Some(0), field),
            (Some(0), check),
            (Some(0), slider),
        ]);
        assert_eq!(
            body(&c)[1..],
            [
                "  - textfield \"Search\" = \"line one⏎line two\" [s1e2] focused",
                "  - checkbox \"Bold\" [s1e3] checked disabled",
                "  - slider \"Zoom\" = 0.5 [s1e4]",
            ]
        );
    }

    #[test]
    fn actionable_wrappers_get_refs_but_hover_actions_dont_count() {
        let mut clickable = node("AXGroup");
        clickable.actions = vec![ActionName::ScrollToVisible, ActionName::Press];
        let mut hover_only = titled("AXGroup", "Card");
        hover_only.actions = vec![ActionName::ScrollToVisible, ActionName::ShowDefaultUi];
        let c = compact(vec![
            (None, titled("AXWindow", "W")),
            (Some(0), clickable),
            (Some(1), text("Open")),
            (Some(0), hover_only),
        ]);
        assert_eq!(
            body(&c)[1..],
            ["  - group \"Open\" [s1e2]", "  - group \"Card\""]
        );
    }

    #[test]
    fn long_text_is_cut_and_says_how_long_it_was() {
        let mut area = node("AXTextArea");
        area.value = Some(AxValue::Text("x".repeat(500)));
        let c = CompactSnapshot::build(
            &snapshot(vec![(None, titled("AXWindow", "W")), (Some(0), area)]),
            Generation::FIRST,
            CompactionPolicy {
                max_text: 5,
                ..CompactionPolicy::default()
            },
        );
        assert_eq!(body(&c)[1], "  - textarea = \"xxxxx…\" (500 chars) [s1e2]");
    }

    #[test]
    fn the_budget_cuts_in_order_and_reports_what_was_omitted() {
        let mut nodes = vec![(None, titled("AXWindow", "W"))];
        for i in 0..400 {
            nodes.push((Some(0), titled("AXButton", &format!("Button number {i}"))));
        }
        let budget = Tokens::budget(300).unwrap();
        let c = CompactSnapshot::build(
            &snapshot(nodes),
            Generation::FIRST,
            CompactionPolicy {
                budget,
                ..CompactionPolicy::default()
            },
        );
        assert!(c.tokens <= budget, "{} > {budget}", c.tokens);
        assert!(c.omitted > 0);
        assert_eq!(c.shown + c.omitted, 401);
        // Refs are dense: every shown control has one, and none was issued for a cut line.
        assert_eq!(c.refs.len(), c.shown);
        let last = c.text.lines().last().unwrap();
        assert!(
            last.starts_with(&format!("… {} more elements", c.omitted)),
            "{last}"
        );
        // Cut in order: the last shown button is the one before the first omitted.
        assert!(c.text.contains(&format!("Button number {}\"", c.shown - 2)));
        assert!(!c.text.contains(&format!("Button number {}\"", c.shown - 1)));
    }

    #[test]
    fn partial_snapshots_say_so() {
        let mut snap = snapshot(vec![(None, titled("AXWindow", "W"))]);
        snap.truncation = Truncation::NodeBudget;
        let c = CompactSnapshot::build(&snap, Generation::FIRST, CompactionPolicy::default());
        assert!(
            c.text.lines().nth(1).unwrap().contains("partial"),
            "{}",
            c.text
        );
        let complete = compact(vec![(None, titled("AXWindow", "W"))]);
        assert!(!complete.text.contains("partial"));
    }

    #[test]
    fn tiny_budgets_are_refused() {
        assert!(Tokens::budget(199).is_err());
        assert_eq!(Tokens::budget(200).unwrap().get(), 200);
        assert_eq!(Tokens::estimate("abcde").get(), 2);
        assert_eq!(Tokens::estimate("").get(), 0);
    }
}
