use doppel_desktop::agent::RunningApp;
use doppel_desktop::ax::tree::{
    ActOn, ActionName, AxNode, AxSnapshot, AxText, AxTree, AxValue, ElementAction, ElementTarget,
    Menus, NodeIndex, Retention, Role, Scope, SnapshotId, SnapshotRequest, States, Subrole,
    Truncation,
};
use doppel_desktop::geometry::Rect;

use super::{Millis, ParseError};
use crate::pb::agent::v1 as agent;

impl TryFrom<agent::AxSnapshot> for AxSnapshot {
    type Error = ParseError;
    fn try_from(wire: agent::AxSnapshot) -> Result<Self, ParseError> {
        let nodes = wire
            .nodes
            .into_iter()
            .map(|n| (n.parent.map(NodeIndex::new), WireNode(n).parse()))
            .collect();
        let tree = AxTree::new(nodes).map_err(|e| ParseError::Invalid {
            field: "nodes",
            reason: e.to_string(),
        })?;
        let truncation = match agent::Truncation::try_from(wire.truncation) {
            Ok(agent::Truncation::Complete) => Truncation::Complete,
            Ok(agent::Truncation::NodeBudget) => Truncation::NodeBudget,
            Ok(agent::Truncation::DepthBudget) => Truncation::DepthBudget,
            Ok(agent::Truncation::Deadline) => Truncation::Deadline,
            Ok(agent::Truncation::Unspecified) | Err(_) => Truncation::Unreported(wire.truncation),
        };
        Ok(Self {
            id: SnapshotId::new(wire.snapshot_id),
            app: RunningApp::from(wire.app.ok_or(ParseError::Missing("app"))?),
            tree,
            truncation,
        })
    }
}

/// One node as the agent sent it.
struct WireNode(agent::AxNode);

impl WireNode {
    /// Lenient per node: an odd frame or value is dropped, not fatal, since
    /// apps report all sorts. The tree's shape is what must be right.
    fn parse(self) -> AxNode {
        let wire = self.0;
        let value = wire.value.and_then(|v| v.value).and_then(|v| match v {
            common_value::Value::Text(t) => Some(AxValue::Text(t)),
            common_value::Value::Number(n) if n.is_finite() => Some(AxValue::Number(n)),
            common_value::Value::Number(_) => None,
            common_value::Value::Boolean(b) => Some(AxValue::Boolean(b)),
        });
        // A frame that isn't a rectangle (not finite, or a negative
        // size) is dropped like any other odd one.
        let frame = wire.frame.and_then(|r| Rect::try_from(r).ok());
        let flags = wire.flags.unwrap_or_default();
        AxNode {
            role: Role::parse(&wire.role),
            subrole: Subrole::parse(&wire.subrole),
            title: AxText::new(wire.title),
            description: AxText::new(wire.description),
            identifier: AxText::new(wire.identifier),
            value,
            frame,
            states: States {
                enabled: flags.enabled,
                focused: flags.focused,
                selected: flags.selected,
                expanded: flags.expanded,
            },
            actions: wire.actions.iter().map(|a| ActionName::parse(a)).collect(),
        }
    }
}

use agent::ax_value as common_value;

impl From<&AxSnapshot> for agent::AxSnapshot {
    fn from(snapshot: &AxSnapshot) -> Self {
        let tree = &snapshot.tree;
        Self {
            snapshot_id: snapshot.id.get(),
            app: Some((&snapshot.app).into()),
            nodes: tree
                .iter()
                .map(|(index, node)| WireNode::encode(tree.parent(index), node))
                .collect(),
            truncation: match snapshot.truncation {
                Truncation::Complete => agent::Truncation::Complete as i32,
                Truncation::NodeBudget => agent::Truncation::NodeBudget as i32,
                Truncation::DepthBudget => agent::Truncation::DepthBudget as i32,
                Truncation::Deadline => agent::Truncation::Deadline as i32,
                Truncation::Unreported(raw) => raw,
            },
        }
    }
}

impl WireNode {
    /// An element's text as the wire carries it: none is empty.
    fn text(text: Option<&AxText>) -> String {
        text.map_or_else(String::new, |text| text.as_str().to_owned())
    }

    fn encode(parent: Option<NodeIndex>, node: &AxNode) -> agent::AxNode {
        agent::AxNode {
            parent: parent.map(NodeIndex::get),
            role: node.role.as_ax().to_owned(),
            subrole: node
                .subrole
                .as_ref()
                .map(|subrole| subrole.as_ax().to_owned())
                .unwrap_or_default(),
            title: Self::text(node.title.as_ref()),
            description: Self::text(node.description.as_ref()),
            identifier: Self::text(node.identifier.as_ref()),
            value: node.value.as_ref().map(|value| agent::AxValue {
                value: Some(match value {
                    AxValue::Text(text) => common_value::Value::Text(text.clone()),
                    AxValue::Number(n) => common_value::Value::Number(*n),
                    AxValue::Boolean(b) => common_value::Value::Boolean(*b),
                }),
            }),
            frame: node.frame.map(Into::into),
            flags: Some(agent::AxFlags {
                enabled: node.states.enabled,
                focused: node.states.focused,
                selected: node.states.selected,
                expanded: node.states.expanded,
            }),
            actions: node
                .actions
                .iter()
                .map(|action| action.as_ax().to_owned())
                .collect(),
        }
    }
}

impl TryFrom<agent::PerformAxAction> for ActOn {
    type Error = ParseError;
    fn try_from(wire: agent::PerformAxAction) -> Result<Self, ParseError> {
        use agent::perform_ax_action::Action;
        let action = match wire.action.ok_or(ParseError::Missing("action"))? {
            Action::Press(_) => ElementAction::Perform(ActionName::Press),
            Action::ShowMenu(_) => ElementAction::Perform(ActionName::ShowMenu),
            Action::Raise(_) => ElementAction::Perform(ActionName::Raise),
            Action::Named(name) => ElementAction::Perform(ActionName::parse(&name)),
            Action::Focus(_) => ElementAction::Focus,
            Action::Select(_) => ElementAction::Select,
            Action::SetValue(text) => ElementAction::SetValue(text),
        };
        Ok(Self {
            target: ElementTarget {
                snapshot: SnapshotId::new(wire.snapshot_id),
                node: NodeIndex::new(wire.node_index),
            },
            action,
        })
    }
}

impl ParseError {
    /// A menu policy; unspecified reads as leaving menus out.
    pub fn menus(field: &'static str, raw: i32) -> Result<Menus, Self> {
        match agent::MenuPolicy::try_from(raw) {
            Ok(agent::MenuPolicy::Exclude | agent::MenuPolicy::Unspecified) => Ok(Menus::Exclude),
            Ok(agent::MenuPolicy::Include) => Ok(Menus::Include),
            Err(_) => Err(Self::UnknownEnum { field, value: raw }),
        }
    }
}

impl From<Menus> for agent::MenuPolicy {
    fn from(menus: Menus) -> Self {
        match menus {
            Menus::Exclude => Self::Exclude,
            Menus::Include => Self::Include,
        }
    }
}

impl agent::TakeAxSnapshot {
    /// The request, its deadline in whole milliseconds: below one is
    /// dropped, and past `u32::MAX` saturates.
    #[must_use]
    pub fn in_millis(request: &SnapshotRequest) -> Self {
        Self {
            scope: Some(match request.scope {
                Scope::FocusedWindow => {
                    agent::take_ax_snapshot::Scope::FocusedWindow(agent::Empty {})
                }
                Scope::App(pid) => agent::take_ax_snapshot::Scope::ApplicationPid(pid.get()),
            }),
            max_nodes: request.max_nodes,
            max_depth: request.max_depth,
            deadline_ms: Millis::saturating(request.deadline).get(),
            menus: match request.menus {
                Menus::Exclude => agent::MenuPolicy::Exclude,
                Menus::Include => agent::MenuPolicy::Include,
            } as i32,
            retention: match request.retention {
                Retention::KeptForActions => agent::Retention::KeptForActions,
                Retention::Observation => agent::Retention::Observation,
            } as i32,
        }
    }
}

impl From<&ActOn> for agent::PerformAxAction {
    fn from(act: &ActOn) -> Self {
        use agent::perform_ax_action::Action;
        let action = match &act.action {
            ElementAction::Perform(ActionName::Press) => Action::Press(agent::Empty {}),
            ElementAction::Perform(ActionName::ShowMenu) => Action::ShowMenu(agent::Empty {}),
            ElementAction::Perform(ActionName::Raise) => Action::Raise(agent::Empty {}),
            ElementAction::Perform(
                other @ (ActionName::Increment
                | ActionName::Decrement
                | ActionName::Confirm
                | ActionName::Cancel
                | ActionName::Pick
                | ActionName::ScrollToVisible
                | ActionName::ShowAlternateUi
                | ActionName::ShowDefaultUi
                | ActionName::Other(_)),
            ) => Action::Named(other.as_ax().to_owned()),
            ElementAction::Focus => Action::Focus(agent::Empty {}),
            ElementAction::Select => Action::Select(agent::Empty {}),
            ElementAction::SetValue(text) => Action::SetValue(text.clone()),
        };
        Self {
            snapshot_id: act.target.snapshot.get(),
            node_index: act.target.node.get(),
            action: Some(action),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pb::common::v1 as common;

    fn wire_node(parent: Option<u32>, role: &str) -> agent::AxNode {
        agent::AxNode {
            parent,
            role: role.into(),
            ..Default::default()
        }
    }

    fn wire(nodes: Vec<agent::AxNode>) -> agent::AxSnapshot {
        agent::AxSnapshot {
            snapshot_id: 9,
            app: Some(agent::RunningApp {
                pid: 3,
                bundle_id: String::new(),
                name: "App".into(),
            }),
            nodes,
            truncation: agent::Truncation::Complete as i32,
        }
    }

    #[test]
    fn snapshots_parse_into_typed_trees() {
        let mut button = wire_node(Some(0), "AXButton");
        button.title = "OK".into();
        button.description = String::new();
        button.actions = vec!["AXPress".into(), "AXCustom".into()];
        button.value = Some(agent::AxValue {
            value: Some(common_value::Value::Number(f64::NAN)),
        });
        button.frame = Some(common::Rect {
            origin: Some(common::Point { x: 1.0, y: 2.0 }),
            size: Some(common::Size {
                width: 3.0,
                height: f64::INFINITY,
            }),
        });
        button.flags = Some(agent::AxFlags {
            enabled: Some(false),
            ..Default::default()
        });
        let snap = AxSnapshot::try_from(wire(vec![wire_node(None, "AXWindow"), button])).unwrap();
        assert_eq!(snap.id, SnapshotId::new(9));
        assert_eq!(snap.app.bundle_id, None);
        let ok = snap.tree.get(NodeIndex::new(1)).unwrap();
        assert_eq!(ok.role, Role::Button);
        assert_eq!(ok.title.as_deref(), Some("OK"));
        assert_eq!(ok.description, None, "empty strings are absent");
        assert_eq!(ok.value, None, "NaN is dropped");
        assert_eq!(ok.frame, None, "an infinite size is dropped");
        assert_eq!(ok.states.enabled, Some(false));
        assert_eq!(ok.states.focused, None);
        assert_eq!(
            ok.actions,
            [ActionName::Press, ActionName::parse("AXCustom")]
        );
    }

    #[test]
    fn snapshots_round_trip() {
        let mut button = wire_node(Some(0), "AXButton");
        button.subrole = "AXCloseButton".into();
        button.title = "Save".into();
        button.actions = vec!["AXPress".into(), "AXCustom".into()];
        button.flags = Some(agent::AxFlags {
            enabled: Some(true),
            focused: None,
            selected: Some(false),
            expanded: None,
        });
        button.frame = Some(common::Rect {
            origin: Some(common::Point { x: 1.0, y: 2.0 }),
            size: Some(common::Size {
                width: 3.0,
                height: 4.0,
            }),
        });
        let mut text = wire_node(Some(0), "AXSomethingNew");
        text.value = Some(agent::AxValue {
            value: Some(common_value::Value::Text("hi".into())),
        });
        let snapshot =
            AxSnapshot::try_from(wire(vec![wire_node(None, "AXWindow"), button, text])).unwrap();
        let again = AxSnapshot::try_from(agent::AxSnapshot::from(&snapshot)).unwrap();
        assert_eq!(again, snapshot);
    }

    #[test]
    fn actions_parse_back() {
        let act = |action| ActOn {
            target: ElementTarget {
                snapshot: SnapshotId::new(4),
                node: NodeIndex::new(2),
            },
            action,
        };
        for action in [
            ElementAction::Perform(ActionName::Press),
            ElementAction::Perform(ActionName::ShowMenu),
            ElementAction::Perform(ActionName::Raise),
            ElementAction::Perform(ActionName::Increment),
            ElementAction::Perform(ActionName::parse("AXCustom")),
            ElementAction::Focus,
            ElementAction::Select,
            ElementAction::SetValue("v".into()),
        ] {
            let act = act(action);
            assert_eq!(ActOn::try_from(agent::PerformAxAction::from(&act)), Ok(act));
        }
    }

    #[test]
    fn bad_shapes_and_missing_apps_are_rejected() {
        let orphan = wire(vec![
            wire_node(None, "AXWindow"),
            wire_node(Some(5), "AXButton"),
        ]);
        assert!(matches!(
            AxSnapshot::try_from(orphan),
            Err(ParseError::Invalid { field: "nodes", .. })
        ));
        assert!(AxSnapshot::try_from(wire(Vec::new())).is_err());
        let mut no_app = wire(vec![wire_node(None, "AXWindow")]);
        no_app.app = None;
        assert_eq!(
            AxSnapshot::try_from(no_app),
            Err(ParseError::Missing("app"))
        );
        let mut odd = wire(vec![wire_node(None, "AXWindow")]);
        odd.truncation = 42;
        assert_eq!(
            AxSnapshot::try_from(odd).unwrap().truncation,
            Truncation::Unreported(42)
        );
    }

    #[test]
    fn actions_map_onto_the_wire() {
        use agent::perform_ax_action::Action;
        let target = doppel_desktop::ax::tree::ElementTarget {
            snapshot: SnapshotId::new(5),
            node: NodeIndex::new(2),
        };
        let wire = |action| agent::PerformAxAction::from(&ActOn { target, action }).action;
        assert_eq!(
            wire(ElementAction::Perform(ActionName::Press)),
            Some(Action::Press(agent::Empty {}))
        );
        assert_eq!(
            wire(ElementAction::Perform(ActionName::Increment)),
            Some(Action::Named("AXIncrement".into()))
        );
        assert_eq!(
            wire(ElementAction::SetValue("é".into())),
            Some(Action::SetValue("é".into()))
        );
    }
}
