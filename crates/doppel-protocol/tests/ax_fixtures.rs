//! Compaction against AX snapshots recorded from a live guest
//! (`doppel debug ax-dump dev --out fixtures/ax/NAME.pb`): the raw agent
//! bytes go through the same parse and compaction as `doppel mcp`.
//!
//! The listings are insta snapshots, for review; the properties below are
//! asserted independently of them.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration test helpers: a failed unwrap is a failed test"
)]

use std::path::PathBuf;

use doppel_desktop::ax::compact::{CompactSnapshot, CompactionPolicy, Tokens};
use doppel_desktop::ax::refs::{Generation, Ref};
use doppel_desktop::ax::tree::AxSnapshot;
use doppel_protocol::pb::agent::v1 as agent;
use prost::Message as _;

fn recorded(name: &str) -> AxSnapshot {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/ax")
        .join(format!("{name}.pb"));
    let bytes = std::fs::read(&path).unwrap();
    agent::AxSnapshot::decode(bytes.as_slice())
        .unwrap()
        .try_into()
        .unwrap()
}

fn compact(snapshot: &AxSnapshot, budget: u32) -> CompactSnapshot {
    CompactSnapshot::build(
        snapshot,
        Generation::FIRST,
        CompactionPolicy {
            budget: Tokens::budget(budget).unwrap(),
            ..CompactionPolicy::default()
        },
    )
}

/// Properties every listing must have, whatever the app.
fn check(snapshot: &AxSnapshot, c: &CompactSnapshot, budget: u32) {
    assert!(c.tokens.get() <= budget, "{} over {budget}", c.tokens);
    let refs: Vec<Ref> = c
        .text
        .lines()
        .filter_map(|line| {
            let start = line.find("[s")?;
            let end = start + line[start..].find(']')?;
            Some(line[start + 1..end].parse().unwrap())
        })
        .collect();
    // Every ref shown resolves, to a node of the role its line names, and no
    // ref was issued without being shown.
    assert_eq!(refs.len(), c.refs.len());
    for (r, line) in refs.iter().zip(c.text.lines().filter(|l| l.contains("[s"))) {
        let target = c.refs.resolve(*r).unwrap();
        let node = snapshot.tree.get(target.element.node).unwrap();
        assert_eq!(target.element.snapshot, snapshot.id);
        assert_eq!(node.role, target.role);
        assert!(
            line.trim_start().starts_with(&format!("- {} ", node.role)),
            "{line} vs {}",
            node.role
        );
    }
}

#[test]
fn textedit_window() {
    let snapshot = recorded("textedit-window");
    let c = compact(&snapshot, 4000);
    check(&snapshot, &c, 4000);
    assert_eq!(c.omitted, 0);
    // The window buttons are named by their subrole.
    for name in ["close", "minimize", "full screen"] {
        assert!(c.text.contains(&format!("button \"{name}\"")), "{}", c.text);
    }
    insta::assert_snapshot!(c.text);
}

#[test]
fn finder_window_sidebar_rows_are_actionable() {
    let snapshot = recorded("finder-window");
    let c = compact(&snapshot, 4000);
    check(&snapshot, &c, 4000);
    for place in ["Applications", "Desktop", "Downloads"] {
        let line = c
            .text
            .lines()
            .find(|l| l.contains(&format!("\"{place}\"")))
            .unwrap();
        assert!(
            line.trim_start().starts_with("- row") && line.contains("[s1e"),
            "{line}"
        );
    }
    insta::assert_snapshot!(c.text);
}

#[test]
fn textedit_with_menus_is_cut_to_the_budget() {
    let snapshot = recorded("textedit-app-menus");
    assert!(snapshot.tree.len() > 300, "the recording covers the menus");
    let c = compact(&snapshot, 1000);
    check(&snapshot, &c, 1000);
    assert!(c.omitted > 0);
    assert!(
        c.text
            .lines()
            .last()
            .unwrap()
            .contains("more elements not shown")
    );
    // With room, nothing is cut, and the listing is far below the raw tree.
    let roomy = compact(&snapshot, 20_000);
    check(&snapshot, &roomy, 20_000);
    assert_eq!(roomy.omitted, 0);
    assert!(roomy.shown < snapshot.tree.len());
}

#[test]
fn vscode_welcome_exposes_electron_content() {
    // Recorded after the agent set AXManualAccessibility: without it, VS Code
    // exposes only its window frame (13 nodes, measured live).
    let snapshot = recorded("vscode-welcome");
    assert!(
        snapshot.tree.len() > 300,
        "the recording holds Electron's tree"
    );
    let budget = Tokens::DEFAULT_BUDGET.get();
    let c = compact(&snapshot, budget);
    check(&snapshot, &c, budget);
    for control in ["Toggle Chat", "Sign In", "Customize Layout..."] {
        let line = c
            .text
            .lines()
            .find(|l| l.contains(&format!("\"{control}\"")))
            .unwrap_or_else(|| panic!("{control} missing:\n{}", c.text));
        assert!(line.contains("[s1e"), "{line}");
    }
    insta::assert_snapshot!(c.text);
}
