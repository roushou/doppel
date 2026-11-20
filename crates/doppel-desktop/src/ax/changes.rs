//! Changes: a listing of only what differs from the model's previous
//! listing of the same app, so a model that just acted reads what its act
//! did, not the whole UI again.
//!
//! Elements are matched as the model would: by their place in the listing's
//! tree, their role and their name, the n-th "button \"OK\"" under a matched
//! parent pairing with the n-th one before. A matched element whose value or
//! states differ has changed; one with no match appeared or went. An element
//! as it was keeps the ref the model holds for it, so the model can act on
//! anything it saw without listing it again.

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;

use super::compact::{CompactSnapshot, CompactionPolicy, Tokens};
use super::refs::{Generation, Ref, RefTable, RefTarget};
use super::tree::{AxSnapshot, ElementTarget, NodeIndex, Role};
use crate::agent::ProcessId;

/// One element of a listing, as the model reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Entry {
    pub(super) role: Role,
    pub(super) name: Option<String>,
    /// As the listing shows it (`"draft"`, `42`).
    pub(super) value: Option<String>,
    pub(super) states: Vec<&'static str>,
    /// The node a ref would act on; `None` for elements that get no ref.
    pub(super) target: Option<NodeIndex>,
    /// Its parent's place in the same list; parents come first.
    pub(super) parent: Option<usize>,
}

impl Entry {
    /// `button "Save"`.
    fn label(&self) -> String {
        match &self.name {
            Some(name) => format!("{} {name:?}", self.role),
            None => self.role.to_string(),
        }
    }

    fn same_place(&self, other: &Self) -> bool {
        self.role == other.role && self.name == other.name
    }
}

/// What a model knows of an app's UI after a listing: every element of the
/// snapshot listed (in listing order, budget or not), and the ref it holds
/// for each, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Known {
    generation: Generation,
    app: ProcessId,
    entries: Vec<Entry>,
    /// One per entry.
    refs: Vec<Option<Ref>>,
}

impl Known {
    pub(super) fn new(
        generation: Generation,
        app: ProcessId,
        entries: Vec<Entry>,
        mut refs: Vec<Option<Ref>>,
    ) -> Self {
        refs.resize(entries.len(), None);
        Self {
            generation,
            app,
            entries,
            refs,
        }
    }

    /// The listing it was learned from.
    #[must_use]
    pub const fn generation(&self) -> Generation {
        self.generation
    }
}

/// What refs from earlier listings name now: an element that hasn't moved
/// in the tree, nor changed its role or name; or, for an element this
/// listing reported gone, what it was.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Carried {
    here: HashMap<Ref, RefTarget>,
    gone: HashMap<Ref, String>,
}

impl Carried {
    /// The element an earlier ref still names.
    #[must_use]
    pub fn get(&self, r: Ref) -> Option<&RefTarget> {
        self.here.get(&r)
    }

    /// What an earlier ref named, when this listing reported it gone:
    /// `button "Save"`.
    #[must_use]
    pub fn gone(&self, r: Ref) -> Option<&str> {
        self.gone.get(&r).map(String::as_str)
    }

    /// Earlier refs that still name an element.
    #[must_use]
    pub fn len(&self) -> usize {
        self.here.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.here.is_empty()
    }
}

/// For each element after, its match before, if it has one.
fn pair(before: &[Entry], after: &[Entry]) -> Vec<Option<usize>> {
    fn levels(entries: &[Entry]) -> (Vec<usize>, Vec<Vec<usize>>) {
        let mut roots = Vec::new();
        let mut children = vec![Vec::new(); entries.len()];
        for (at, entry) in entries.iter().enumerate() {
            match entry.parent.and_then(|p| children.get_mut(p)) {
                Some(siblings) => siblings.push(at),
                None => roots.push(at),
            }
        }
        (roots, children)
    }
    let (before_roots, before_children) = levels(before);
    let (after_roots, after_children) = levels(after);
    let mut matched = vec![None; after.len()];
    // Sibling lists to pair, a level at a time: no recursion, however deep.
    let mut work: Vec<(&[usize], &[usize])> = vec![(&before_roots, &after_roots)];
    while let Some((was, now)) = work.pop() {
        let mut free: VecDeque<usize> = was.iter().copied().collect();
        for &a in now {
            let found = free
                .iter()
                .position(|&b| before[b].same_place(&after[a]))
                .and_then(|at| free.remove(at));
            if let Some(b) = found {
                matched[a] = Some(b);
                work.push((&before_children[b], &after_children[a]));
            }
        }
    }
    matched
}

/// What became of an element of the earlier listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fate {
    Appeared,
    Changed(usize),
    Same(usize),
}

/// Most of the UI is new: listing changes would be longer than listing it.
fn mostly_new(appeared: usize, gone: usize, after: usize) -> bool {
    appeared + gone > after.max(1) / 2 && appeared + gone > 8
}

/// The rendering state of a listing of changes, as `Listing` is of a full one.
struct Lines {
    text: String,
    limit: u32,
    used: u32,
    /// Elements listed.
    shown: usize,
    full: bool,
}

impl Lines {
    /// Whether a line this long fits; once one doesn't, none after it does,
    /// so the listing never skips a change and then shows a later one.
    fn fits(&mut self, chars: usize) -> bool {
        let cost = u32::try_from(chars.div_ceil(4))
            .unwrap_or(u32::MAX)
            .saturating_add(1);
        if self.full || self.used.saturating_add(cost) > self.limit {
            self.full = true;
            return false;
        }
        self.used += cost;
        true
    }

    fn header(&mut self, line: &str) {
        self.used += Tokens::estimate(line).get() + 1;
        self.text.push_str(line);
        self.text.push('\n');
    }

    fn element(&mut self, line: &str) {
        self.text.push_str(line);
        self.text.push('\n');
        self.shown += 1;
    }
}

impl CompactSnapshot {
    /// What changed in `snapshot` since the listing `before` was learned
    /// from: elements that appeared, changed (their value or states) or
    /// went. Those that appeared or changed get refs of this listing; those
    /// as they were keep the refs the model holds ([`CompactSnapshot::carried`]).
    ///
    /// Lists everything instead, saying why, when the frontmost app is
    /// another or most of the UI is new.
    #[must_use]
    pub fn changes(
        snapshot: &AxSnapshot,
        generation: Generation,
        policy: CompactionPolicy,
        before: &Known,
    ) -> Self {
        let since = before.generation;
        if snapshot.app.pid != before.app {
            let note = format!("another app is frontmost than at {since}: showing all");
            return Self::listing(snapshot, generation, policy, Some(&note));
        }
        let after = Self::entries(snapshot, policy);
        let matched = pair(&before.entries, &after);
        let fates: Vec<Fate> = matched
            .iter()
            .zip(&after)
            .map(|(m, now)| match *m {
                None => Fate::Appeared,
                Some(b) => {
                    let was = &before.entries[b];
                    if was.value == now.value && was.states == now.states {
                        Fate::Same(b)
                    } else {
                        Fate::Changed(b)
                    }
                }
            })
            .collect();
        let mut kept = vec![false; before.entries.len()];
        for b in matched.iter().flatten() {
            kept[*b] = true;
        }
        // Of what went, only the topmost is listed, with how many went inside it.
        let gone: Vec<usize> = (0..before.entries.len())
            .filter(|&b| !kept[b] && before.entries[b].parent.is_none_or(|p| kept[p]))
            .collect();
        let gone_all = kept.iter().filter(|k| !**k).count();
        let appeared = fates.iter().filter(|f| **f == Fate::Appeared).count();
        let changed = fates
            .iter()
            .filter(|f| matches!(f, Fate::Changed(_)))
            .count();
        if mostly_new(appeared, gone_all, after.len()) {
            let note = format!("most of the UI is new since {since}: showing all");
            return Self::listing(snapshot, generation, policy, Some(&note));
        }

        let mut header = String::new();
        let app = &snapshot.app;
        let _ = write!(header, "# {}", app.name);
        if let Some(id) = &app.bundle_id {
            let _ = write!(header, " ({id})");
        }
        if appeared + changed + gone.len() == 0 {
            let _ = write!(
                header,
                " · snapshot {generation} · no changes since {since}; its refs still work"
            );
        } else {
            let _ = write!(
                header,
                " · snapshot {generation} · changes since {since}: {appeared} appeared, {changed} changed, {} gone; the rest is as it was and its refs still work",
                gone.len()
            );
        }
        let mut lines = Lines {
            text: String::new(),
            limit: policy.budget.get().saturating_sub(40),
            used: 0,
            shown: 0,
            full: false,
        };
        lines.header(&header);
        if !snapshot.truncation.is_complete() {
            lines.header(&format!(
                "# partial: the agent {}; later parts of the UI are missing",
                snapshot.truncation
            ));
        }

        let mut refs = RefTable::new(generation);
        let mut issued: Vec<Option<Ref>> = vec![None; after.len()];
        // How deep each appeared element is drawn: under its parent, when that appeared too.
        let mut depth: Vec<Option<usize>> = vec![None; after.len()];
        let mut left = 0;
        for (at, now) in after.iter().enumerate() {
            let fate = fates[at];
            let (mark, was) = match fate {
                Fate::Same(_) => continue,
                Fate::Appeared => ("+", None),
                Fate::Changed(b) => ("~", Some(&before.entries[b])),
            };
            let under = now
                .parent
                .filter(|&p| fates[p] == Fate::Appeared)
                .and_then(|p| depth[p]);
            let level = under.map_or(0, |d| d + 1);
            let mut head = format!("{}{mark} {}", "  ".repeat(level), now.label());
            if let Some(value) = &now.value {
                let _ = write!(head, " = {value}");
            }
            let mut tail = String::new();
            for state in &now.states {
                let _ = write!(tail, " {state}");
            }
            if under.is_none()
                && let Some(parent) = now.parent
            {
                let _ = write!(tail, " · in {}", after[parent].label());
            }
            if let Some(was) = was {
                tail.push_str(&Self::was(was, now));
            }
            // Room for the ref, counted before issuing it, so a line that
            // doesn't fit doesn't use one up.
            let room = if now.target.is_some() { 12 } else { 0 };
            if !lines.fits(head.chars().count() + room + tail.chars().count()) {
                left += 1;
                continue;
            }
            if let Some(node) = now.target {
                let r = refs.issue(RefTarget {
                    element: ElementTarget {
                        snapshot: snapshot.id,
                        node,
                    },
                    role: now.role.clone(),
                    name: now.name.clone(),
                });
                let _ = write!(head, " [{r}]");
                issued[at] = Some(r);
            }
            lines.element(&format!("{head}{tail}"));
            depth[at] = Some(level);
        }
        for &b in &gone {
            let was = &before.entries[b];
            let mut line = format!("- {}", was.label());
            if let Some(r) = before.refs[b] {
                let _ = write!(line, " (was {r})");
            }
            if let Some(parent) = was.parent {
                let _ = write!(line, " · from {}", before.entries[parent].label());
            }
            let inside = Self::inside(&before.entries, &kept, b);
            if inside > 0 {
                let _ = write!(line, ", with {inside} inside");
            }
            if lines.fits(line.chars().count()) {
                lines.element(&line);
            } else {
                left += 1;
            }
        }
        if left > 0 {
            let _ = writeln!(
                lines.text,
                "… {left} more changes not shown ({} budget); snapshot with show=all or raise max_tokens",
                policy.budget
            );
        }

        // What the model holds now: this listing's refs, else the ones it had.
        let mut carried = HashMap::new();
        let holds: Vec<Option<Ref>> = after
            .iter()
            .enumerate()
            .map(|(at, now)| {
                let earlier = matched[at].and_then(|b| before.refs[b]);
                if let (Some(r), Some(node)) = (earlier, now.target) {
                    carried.insert(
                        r,
                        RefTarget {
                            element: ElementTarget {
                                snapshot: snapshot.id,
                                node,
                            },
                            role: now.role.clone(),
                            name: now.name.clone(),
                        },
                    );
                }
                issued[at].or(earlier.filter(|_| now.target.is_some()))
            })
            .collect();
        // Every earlier ref whose element went, listed or not.
        let gone = (0..before.entries.len())
            .filter(|&b| !kept[b])
            .filter_map(|b| before.refs[b].map(|r| (r, before.entries[b].label())))
            .collect();
        let tokens = Tokens::estimate(&lines.text);
        Self {
            text: lines.text,
            refs,
            carried: Carried {
                here: carried,
                gone,
            },
            known: Known::new(generation, snapshot.app.pid, after, holds),
            shown: lines.shown,
            omitted: left,
            tokens,
        }
    }

    /// How a changed element was: ` (was = "", not focused)`.
    fn was(was: &Entry, now: &Entry) -> String {
        let mut parts = Vec::new();
        if was.value != now.value {
            parts.push(
                was.value
                    .as_ref()
                    .map_or_else(|| "no value".to_owned(), |v| format!("= {v}")),
            );
        }
        if was.states != now.states {
            parts.push(if was.states.is_empty() {
                "no states".to_owned()
            } else {
                was.states.join(" ")
            });
        }
        format!(" (was {})", parts.join(", "))
    }

    /// How many elements went with `top`: its descendants, none kept.
    fn inside(entries: &[Entry], kept: &[bool], top: usize) -> usize {
        let mut within = vec![false; entries.len()];
        within[top] = true;
        let mut count = 0;
        for (at, entry) in entries.iter().enumerate().skip(top + 1) {
            if entry.parent.is_some_and(|p| within[p]) && !kept[at] {
                within[at] = true;
                count += 1;
            }
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{BundleId, RunningApp};
    use crate::ax::compact::Show;
    use crate::ax::tree::tests::node;
    use crate::ax::tree::{AxNode, AxText, AxTree, AxValue, SnapshotId, States, Truncation};

    fn snapshot(id: u64, pid: i32, nodes: Vec<(Option<u32>, AxNode)>) -> AxSnapshot {
        AxSnapshot {
            id: SnapshotId::new(id),
            app: RunningApp {
                pid: ProcessId::new(pid),
                bundle_id: BundleId::new("com.example.Mail"),
                name: "Mail".into(),
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

    fn field(title: &str, value: &str, focused: bool) -> AxNode {
        AxNode {
            value: Some(AxValue::Text(value.into())),
            states: States {
                focused: Some(focused),
                ..States::default()
            },
            ..titled("AXTextField", title)
        }
    }

    fn text(value: &str) -> AxNode {
        AxNode {
            value: Some(AxValue::Text(value.into())),
            ..node("AXStaticText")
        }
    }

    const POLICY: CompactionPolicy = CompactionPolicy {
        budget: Tokens::DEFAULT_BUDGET,
        max_text: 160,
        show: Show::Changes,
    };

    /// A compose window: a field, Send and Cancel, and a list of drafts.
    fn compose(
        id: u64,
        email: &str,
        focused: bool,
        extra: Vec<(Option<u32>, AxNode)>,
    ) -> AxSnapshot {
        let mut nodes = vec![
            (None, titled("AXWindow", "Compose")),
            (Some(0), field("To", email, focused)),
            (Some(0), titled("AXButton", "Send")),
            (Some(0), titled("AXButton", "Cancel")),
            (Some(0), titled("AXList", "Drafts")),
            (Some(4), titled("AXButton", "Draft")),
            (Some(4), titled("AXButton", "Draft")),
        ];
        nodes.extend(extra);
        snapshot(id, 7, nodes)
    }

    fn first(s: &AxSnapshot) -> CompactSnapshot {
        CompactSnapshot::build(s, Generation::FIRST, POLICY)
    }

    fn second(s: &AxSnapshot, before: &CompactSnapshot) -> CompactSnapshot {
        CompactSnapshot::changes(s, Generation::FIRST.next(), POLICY, &before.known)
    }

    #[test]
    fn lists_what_changed_and_keeps_the_refs_of_the_rest() {
        let before = first(&compose(1, "", false, Vec::new()));
        assert_eq!(
            before.text.lines().skip(1).collect::<Vec<_>>(),
            [
                "- window \"Compose\" [s1e1]",
                "  - textfield \"To\" [s1e2]",
                "  - button \"Send\" [s1e3]",
                "  - button \"Cancel\" [s1e4]",
                "  - list \"Drafts\"",
                "    - button \"Draft\" [s1e5]",
                "    - button \"Draft\" [s1e6]",
            ]
        );
        let after = second(
            &compose(2, "ann@example.com", true, vec![(Some(0), text("Saved"))]),
            &before,
        );
        assert_eq!(
            after.text.lines().collect::<Vec<_>>(),
            [
                "# Mail (com.example.Mail) · snapshot s2 · changes since s1: 1 appeared, 1 changed, 0 gone; the rest is as it was and its refs still work",
                "~ textfield \"To\" = \"ann@example.com\" [s2e1] focused · in window \"Compose\" (was no value, no states)",
                "+ text \"Saved\" · in window \"Compose\"",
            ]
        );
        // The changed field has a new ref; its old one, like Send's, still works.
        let send = after.carried.get("s1e3".parse().unwrap()).unwrap();
        assert_eq!(
            (send.element.snapshot, send.element.node),
            (SnapshotId::new(2), NodeIndex::new(2))
        );
        assert!(after.carried.get("s1e2".parse().unwrap()).is_some());
        assert_eq!(after.refs.len(), 1);
        // Two drafts alike pair in order: the second keeps the second's ref.
        assert_eq!(
            after
                .carried
                .get("s1e6".parse().unwrap())
                .unwrap()
                .element
                .node,
            NodeIndex::new(6)
        );
    }

    #[test]
    fn says_so_when_nothing_changed_and_refs_carry_on_through_listings() {
        let one = first(&compose(1, "", false, Vec::new()));
        let two = second(&compose(2, "", false, Vec::new()), &one);
        assert_eq!(
            two.text,
            "# Mail (com.example.Mail) · snapshot s2 · no changes since s1; its refs still work\n"
        );
        let three = CompactSnapshot::changes(
            &compose(3, "", false, Vec::new()),
            Generation::FIRST.next().next(),
            POLICY,
            &two.known,
        );
        // A ref from the first listing still names its element two listings on.
        assert_eq!(
            three
                .carried
                .get("s1e4".parse().unwrap())
                .unwrap()
                .element
                .snapshot,
            SnapshotId::new(3)
        );
    }

    #[test]
    fn lists_what_went_once_with_what_was_inside_it() {
        let before = first(&compose(1, "", false, Vec::new()));
        let mut gone = compose(2, "", false, Vec::new());
        gone.tree = AxTree::new(vec![
            (None, titled("AXWindow", "Compose")),
            (Some(NodeIndex::new(0)), field("To", "", false)),
            (Some(NodeIndex::new(0)), titled("AXButton", "Send")),
            (Some(NodeIndex::new(0)), titled("AXButton", "Cancel")),
        ])
        .unwrap();
        let after = second(&gone, &before);
        assert_eq!(
            after.text.lines().skip(1).collect::<Vec<_>>(),
            ["- list \"Drafts\" · from window \"Compose\", with 2 inside"]
        );
        assert!(after.carried.get("s1e5".parse().unwrap()).is_none());
        // The refs of what went inside it, listed or not, say what went.
        assert_eq!(
            after.carried.gone("s1e5".parse().unwrap()),
            Some("button \"Draft\"")
        );
        assert!(after.carried.gone("s1e2".parse().unwrap()).is_none());
    }

    #[test]
    fn nests_what_appeared_inside_what_appeared() {
        let before = first(&compose(1, "", false, Vec::new()));
        let after = second(
            &compose(
                2,
                "",
                false,
                vec![
                    (Some(0), titled("AXSheet", "Discard?")),
                    (Some(7), titled("AXButton", "Discard")),
                ],
            ),
            &before,
        );
        assert_eq!(
            after.text.lines().skip(1).collect::<Vec<_>>(),
            [
                "+ sheet \"Discard?\" [s2e1] · in window \"Compose\"",
                "  + button \"Discard\" [s2e2]",
            ]
        );
    }

    #[test]
    fn lists_everything_for_another_app_or_a_mostly_new_ui() {
        let before = first(&compose(1, "", false, Vec::new()));
        let mut other = compose(2, "", false, Vec::new());
        other.app.pid = ProcessId::new(8);
        let listed = second(&other, &before);
        assert_eq!(
            listed.text.lines().nth(1),
            Some("# another app is frontmost than at s1: showing all")
        );
        assert_eq!(listed.refs.len(), 6);

        let renamed = snapshot(
            2,
            7,
            (0..12)
                .map(|n| {
                    (
                        if n == 0 { None } else { Some(0) },
                        titled("AXButton", &format!("b{n}")),
                    )
                })
                .collect(),
        );
        let listed = second(&renamed, &before);
        assert_eq!(
            listed.text.lines().nth(1),
            Some("# most of the UI is new since s1: showing all")
        );
    }

    #[test]
    fn cuts_a_long_listing_of_changes_at_its_budget() {
        let before = first(&compose(1, "", false, Vec::new()));
        let many: Vec<_> = (0..5)
            .map(|n| {
                (
                    Some(0),
                    titled("AXButton", &format!("New {n} {}", "long ".repeat(28))),
                )
            })
            .collect();
        let tight = CompactionPolicy {
            budget: Tokens::budget(Tokens::MIN_BUDGET.get()).unwrap(),
            ..POLICY
        };
        let after = CompactSnapshot::changes(
            &compose(2, "", false, many),
            Generation::FIRST.next(),
            tight,
            &before.known,
        );
        assert!(after.omitted > 0);
        assert!(after.text.ends_with(&format!("… {} more changes not shown (200 tokens budget); snapshot with show=all or raise max_tokens\n", after.omitted)));
        assert_eq!(after.refs.len(), after.shown);
    }
}
