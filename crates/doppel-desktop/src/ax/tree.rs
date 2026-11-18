//! A snapshot of an accessibility tree, parsed from the agent's flat pre-order
//! list. Construction checks the shape once (a root, parents before children),
//! so code downstream walks a tree that is known to be well formed.

use crate::geometry::Rect;
use std::fmt;
use std::time::Duration;

use crate::agent::{ProcessId, RunningApp};

/// Identifies one snapshot taken by one agent process. Chosen by the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SnapshotId(u64);

impl SnapshotId {
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Position of a node in its snapshot's pre-order list. Meaningless without the snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeIndex(u32);

impl NodeIndex {
    pub const ROOT: Self = Self(0);

    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
    const fn slot(self) -> usize {
        self.0 as usize
    }
}

/// An AX name (a role, subrole or action) this build has no variant for,
/// as the agent reported it. Only the parsers make one, so it is never a
/// name the build knows: an unknown role never stands for a known one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UnknownName(String);

impl UnknownName {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for UnknownName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Text an element reports: its title, description or identifier. Never
/// empty: an element with none reports nothing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AxText(String);

impl AxText {
    /// `None` for empty text, which an element reports as having none.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Option<Self> {
        let text = text.into();
        (!text.is_empty()).then_some(Self(text))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for AxText {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AxText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An `AXRole`. Roles the core doesn't know keep their raw string.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Role {
    Application,
    Window,
    Sheet,
    Popover,
    Toolbar,
    Group,
    ScrollArea,
    SplitGroup,
    Splitter,
    ScrollBar,
    LayoutArea,
    LayoutItem,
    TabGroup,
    Button,
    CheckBox,
    RadioButton,
    PopUpButton,
    MenuButton,
    ComboBox,
    TextField,
    TextArea,
    StaticText,
    Link,
    Image,
    Heading,
    WebArea,
    List,
    RadioGroup,
    Table,
    Outline,
    Browser,
    Row,
    Column,
    Cell,
    Slider,
    Incrementor,
    DisclosureTriangle,
    ColorWell,
    ProgressIndicator,
    BusyIndicator,
    ValueIndicator,
    LevelIndicator,
    MenuBar,
    MenuBarItem,
    Menu,
    MenuItem,
    Ruler,
    RulerMarker,
    Unknown(UnknownName),
}

impl Role {
    /// Roles whose collapsed state says something. Chromium reports
    /// AXExpanded false on every element, so elsewhere it is noise.
    #[must_use]
    pub const fn can_expand(&self) -> bool {
        match self {
            Self::DisclosureTriangle
            | Self::Row
            | Self::Outline
            | Self::ComboBox
            | Self::PopUpButton
            | Self::MenuButton
            | Self::MenuItem => true,
            Self::Application
            | Self::Window
            | Self::Sheet
            | Self::Popover
            | Self::Toolbar
            | Self::Group
            | Self::ScrollArea
            | Self::SplitGroup
            | Self::Splitter
            | Self::ScrollBar
            | Self::LayoutArea
            | Self::LayoutItem
            | Self::TabGroup
            | Self::Button
            | Self::CheckBox
            | Self::RadioButton
            | Self::TextField
            | Self::TextArea
            | Self::StaticText
            | Self::Link
            | Self::Image
            | Self::Heading
            | Self::WebArea
            | Self::List
            | Self::RadioGroup
            | Self::Table
            | Self::Browser
            | Self::Column
            | Self::Cell
            | Self::Slider
            | Self::Incrementor
            | Self::ColorWell
            | Self::ProgressIndicator
            | Self::BusyIndicator
            | Self::ValueIndicator
            | Self::LevelIndicator
            | Self::MenuBar
            | Self::MenuBarItem
            | Self::Menu
            | Self::Ruler
            | Self::RulerMarker
            | Self::Unknown(_) => false,
        }
    }
}

/// How compaction treats a role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleKind {
    /// Something to act on: always shown, always gets a ref.
    Control,
    /// Structure a reader needs (windows, toolbars, lists): always shown.
    Landmark,
    /// Shown when it carries a name or value.
    Content,
    /// Layout wrappers: replaced by their children unless named.
    Wrapper,
    /// Chrome with nothing to read or act on: dropped with its subtree.
    Noise,
}

impl Role {
    /// The parser: the only place raw role strings are compared.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        match raw {
            "AXApplication" => Self::Application,
            "AXWindow" => Self::Window,
            "AXSheet" => Self::Sheet,
            "AXPopover" => Self::Popover,
            "AXToolbar" => Self::Toolbar,
            "AXGroup" => Self::Group,
            "AXScrollArea" => Self::ScrollArea,
            "AXSplitGroup" => Self::SplitGroup,
            "AXSplitter" => Self::Splitter,
            "AXScrollBar" => Self::ScrollBar,
            "AXLayoutArea" => Self::LayoutArea,
            "AXLayoutItem" => Self::LayoutItem,
            "AXTabGroup" => Self::TabGroup,
            "AXButton" => Self::Button,
            "AXCheckBox" => Self::CheckBox,
            "AXRadioButton" => Self::RadioButton,
            "AXPopUpButton" => Self::PopUpButton,
            "AXMenuButton" => Self::MenuButton,
            "AXComboBox" => Self::ComboBox,
            "AXTextField" => Self::TextField,
            "AXTextArea" => Self::TextArea,
            "AXStaticText" => Self::StaticText,
            "AXLink" => Self::Link,
            "AXImage" => Self::Image,
            "AXHeading" => Self::Heading,
            "AXWebArea" => Self::WebArea,
            "AXList" => Self::List,
            "AXRadioGroup" => Self::RadioGroup,
            "AXTable" => Self::Table,
            "AXOutline" => Self::Outline,
            "AXBrowser" => Self::Browser,
            "AXRow" => Self::Row,
            "AXColumn" => Self::Column,
            "AXCell" => Self::Cell,
            "AXSlider" => Self::Slider,
            "AXIncrementor" => Self::Incrementor,
            "AXDisclosureTriangle" => Self::DisclosureTriangle,
            "AXColorWell" => Self::ColorWell,
            "AXProgressIndicator" => Self::ProgressIndicator,
            "AXBusyIndicator" => Self::BusyIndicator,
            "AXValueIndicator" => Self::ValueIndicator,
            "AXLevelIndicator" => Self::LevelIndicator,
            "AXMenuBar" => Self::MenuBar,
            "AXMenuBarItem" => Self::MenuBarItem,
            "AXMenu" => Self::Menu,
            "AXMenuItem" => Self::MenuItem,
            "AXRuler" => Self::Ruler,
            "AXRulerMarker" => Self::RulerMarker,
            other => Self::Unknown(UnknownName(other.to_owned())),
        }
    }

    /// The `AXRole` string: the inverse of [`Self::parse`].
    #[must_use]
    pub fn as_ax(&self) -> &str {
        match self {
            Self::Application => "AXApplication",
            Self::Window => "AXWindow",
            Self::Sheet => "AXSheet",
            Self::Popover => "AXPopover",
            Self::Toolbar => "AXToolbar",
            Self::Group => "AXGroup",
            Self::ScrollArea => "AXScrollArea",
            Self::SplitGroup => "AXSplitGroup",
            Self::Splitter => "AXSplitter",
            Self::ScrollBar => "AXScrollBar",
            Self::LayoutArea => "AXLayoutArea",
            Self::LayoutItem => "AXLayoutItem",
            Self::TabGroup => "AXTabGroup",
            Self::Button => "AXButton",
            Self::CheckBox => "AXCheckBox",
            Self::RadioButton => "AXRadioButton",
            Self::PopUpButton => "AXPopUpButton",
            Self::MenuButton => "AXMenuButton",
            Self::ComboBox => "AXComboBox",
            Self::TextField => "AXTextField",
            Self::TextArea => "AXTextArea",
            Self::StaticText => "AXStaticText",
            Self::Link => "AXLink",
            Self::Image => "AXImage",
            Self::Heading => "AXHeading",
            Self::WebArea => "AXWebArea",
            Self::List => "AXList",
            Self::RadioGroup => "AXRadioGroup",
            Self::Table => "AXTable",
            Self::Outline => "AXOutline",
            Self::Browser => "AXBrowser",
            Self::Row => "AXRow",
            Self::Column => "AXColumn",
            Self::Cell => "AXCell",
            Self::Slider => "AXSlider",
            Self::Incrementor => "AXIncrementor",
            Self::DisclosureTriangle => "AXDisclosureTriangle",
            Self::ColorWell => "AXColorWell",
            Self::ProgressIndicator => "AXProgressIndicator",
            Self::BusyIndicator => "AXBusyIndicator",
            Self::ValueIndicator => "AXValueIndicator",
            Self::LevelIndicator => "AXLevelIndicator",
            Self::MenuBar => "AXMenuBar",
            Self::MenuBarItem => "AXMenuBarItem",
            Self::Menu => "AXMenu",
            Self::MenuItem => "AXMenuItem",
            Self::Ruler => "AXRuler",
            Self::RulerMarker => "AXRulerMarker",
            Self::Unknown(raw) => raw.as_str(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> RoleKind {
        match self {
            Self::Button
            | Self::CheckBox
            | Self::RadioButton
            | Self::PopUpButton
            | Self::MenuButton
            | Self::ComboBox
            | Self::TextField
            | Self::TextArea
            | Self::Link
            | Self::Slider
            | Self::Incrementor
            | Self::DisclosureTriangle
            | Self::ColorWell
            | Self::MenuBarItem
            | Self::MenuItem
            | Self::Row => RoleKind::Control,
            Self::Application
            | Self::Window
            | Self::Sheet
            | Self::Popover
            | Self::Toolbar
            | Self::TabGroup
            | Self::WebArea
            | Self::List
            | Self::Table
            | Self::Outline
            | Self::Browser
            | Self::MenuBar
            | Self::Menu => RoleKind::Landmark,
            Self::StaticText
            | Self::Image
            | Self::Heading
            | Self::Column
            | Self::Cell
            | Self::ProgressIndicator
            | Self::BusyIndicator
            | Self::ValueIndicator
            | Self::LevelIndicator
            | Self::Ruler => RoleKind::Content,
            Self::Group
            | Self::ScrollArea
            | Self::SplitGroup
            | Self::LayoutArea
            | Self::LayoutItem
            | Self::RadioGroup
            | Self::Unknown(_) => RoleKind::Wrapper,
            Self::Splitter | Self::ScrollBar | Self::RulerMarker => RoleKind::Noise,
        }
    }

    /// The short name a snapshot shows.
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::Application => "app",
            Self::Window => "window",
            Self::Sheet => "sheet",
            Self::Popover => "popover",
            Self::Toolbar => "toolbar",
            Self::Group => "group",
            Self::ScrollArea => "scrollarea",
            Self::SplitGroup => "splitgroup",
            Self::Splitter => "splitter",
            Self::ScrollBar => "scrollbar",
            Self::LayoutArea => "layoutarea",
            Self::LayoutItem => "layoutitem",
            Self::TabGroup => "tabgroup",
            Self::Button => "button",
            Self::CheckBox => "checkbox",
            Self::RadioButton => "radio",
            Self::PopUpButton => "popup",
            Self::MenuButton => "menubutton",
            Self::ComboBox => "combobox",
            Self::TextField => "textfield",
            Self::TextArea => "textarea",
            Self::StaticText => "text",
            Self::Link => "link",
            Self::Image => "image",
            Self::Heading => "heading",
            Self::WebArea => "webarea",
            Self::List => "list",
            Self::RadioGroup => "radiogroup",
            Self::Table => "table",
            Self::Outline => "outline",
            Self::Browser => "browser",
            Self::Row => "row",
            Self::Column => "column",
            Self::Cell => "cell",
            Self::Slider => "slider",
            Self::Incrementor => "stepper",
            Self::DisclosureTriangle => "disclosure",
            Self::ColorWell => "colorwell",
            Self::ProgressIndicator => "progress",
            Self::BusyIndicator => "busy",
            Self::ValueIndicator => "valueindicator",
            Self::LevelIndicator => "level",
            Self::MenuBar => "menubar",
            Self::MenuBarItem => "menubaritem",
            Self::Menu => "menu",
            Self::MenuItem => "menuitem",
            Self::Ruler => "ruler",
            Self::RulerMarker => "rulermarker",
            Self::Unknown(raw) => raw.as_str().strip_prefix("AX").unwrap_or(raw.as_str()),
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// An `AXSubrole`. Only the ones the core uses are named.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Subrole {
    CloseButton,
    MinimizeButton,
    ZoomButton,
    FullScreenButton,
    SearchField,
    /// A password field: what it holds is a secret, and the screen hides it.
    SecureTextField,
    Other(UnknownName),
}

impl Subrole {
    /// The parser; `None` for an empty subrole, which an element reports
    /// as having none.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "" => return None,
            "AXCloseButton" => Self::CloseButton,
            "AXMinimizeButton" => Self::MinimizeButton,
            "AXZoomButton" => Self::ZoomButton,
            "AXFullScreenButton" => Self::FullScreenButton,
            "AXSearchField" => Self::SearchField,
            "AXSecureTextField" => Self::SecureTextField,
            other => Self::Other(UnknownName(other.to_owned())),
        })
    }

    /// The `AXSubrole` string: the inverse of [`Self::parse`].
    #[must_use]
    pub fn as_ax(&self) -> &str {
        match self {
            Self::CloseButton => "AXCloseButton",
            Self::MinimizeButton => "AXMinimizeButton",
            Self::ZoomButton => "AXZoomButton",
            Self::FullScreenButton => "AXFullScreenButton",
            Self::SearchField => "AXSearchField",
            Self::SecureTextField => "AXSecureTextField",
            Self::Other(raw) => raw.as_str(),
        }
    }

    /// A name for elements whose subrole says what they are when nothing else does.
    #[must_use]
    pub const fn implied_name(&self) -> Option<&'static str> {
        match self {
            Self::CloseButton => Some("close"),
            Self::MinimizeButton => Some("minimize"),
            Self::ZoomButton => Some("zoom"),
            Self::FullScreenButton => Some("full screen"),
            Self::SearchField | Self::SecureTextField | Self::Other(_) => None,
        }
    }
}

/// An AX action name a node advertises (`AXPress`, ...).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ActionName {
    Press,
    ShowMenu,
    Raise,
    Increment,
    Decrement,
    Confirm,
    Cancel,
    Pick,
    ScrollToVisible,
    ShowAlternateUi,
    ShowDefaultUi,
    Other(UnknownName),
}

impl ActionName {
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        match raw {
            "AXPress" => Self::Press,
            "AXShowMenu" => Self::ShowMenu,
            "AXRaise" => Self::Raise,
            "AXIncrement" => Self::Increment,
            "AXDecrement" => Self::Decrement,
            "AXConfirm" => Self::Confirm,
            "AXCancel" => Self::Cancel,
            "AXPick" => Self::Pick,
            "AXScrollToVisible" => Self::ScrollToVisible,
            "AXShowAlternateUI" => Self::ShowAlternateUi,
            "AXShowDefaultUI" => Self::ShowDefaultUi,
            other => Self::Other(UnknownName(other.to_owned())),
        }
    }

    /// The name the AX API takes.
    #[must_use]
    pub fn as_ax(&self) -> &str {
        match self {
            Self::Press => "AXPress",
            Self::ShowMenu => "AXShowMenu",
            Self::Raise => "AXRaise",
            Self::Increment => "AXIncrement",
            Self::Decrement => "AXDecrement",
            Self::Confirm => "AXConfirm",
            Self::Cancel => "AXCancel",
            Self::Pick => "AXPick",
            Self::ScrollToVisible => "AXScrollToVisible",
            Self::ShowAlternateUi => "AXShowAlternateUI",
            Self::ShowDefaultUi => "AXShowDefaultUI",
            Self::Other(raw) => raw.as_str(),
        }
    }

    /// Whether advertising this action makes a node worth a ref. Hover-style
    /// and scrolling actions are on nearly every web node, so they don't count.
    #[must_use]
    pub const fn makes_actionable(&self) -> bool {
        match self {
            Self::Press
            | Self::ShowMenu
            | Self::Increment
            | Self::Decrement
            | Self::Confirm
            | Self::Pick => true,
            Self::Raise
            | Self::Cancel
            | Self::ScrollToVisible
            | Self::ShowAlternateUi
            | Self::ShowDefaultUi
            | Self::Other(_) => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AxValue {
    Text(String),
    Number(f64),
    Boolean(bool),
}

/// State an element reports. `None`: the element doesn't report it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct States {
    pub enabled: Option<bool>,
    pub focused: Option<bool>,
    pub selected: Option<bool>,
    pub expanded: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AxNode {
    pub role: Role,
    pub subrole: Option<Subrole>,
    pub title: Option<AxText>,
    pub description: Option<AxText>,
    pub identifier: Option<AxText>,
    pub value: Option<AxValue>,
    /// Its frame on the display.
    pub frame: Option<Rect>,
    pub states: States,
    pub actions: Vec<ActionName>,
}

impl AxNode {
    /// What a reader would call the element: its title, else its description,
    /// else (for text) its value, else what its subrole implies (window buttons).
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        let text_value = match (&self.role, &self.value) {
            (Role::StaticText, Some(AxValue::Text(text))) => Some(text.as_str()),
            (_, Some(AxValue::Text(_) | AxValue::Number(_) | AxValue::Boolean(_)) | None) => None,
        };
        [
            self.title.as_deref(),
            self.description.as_deref(),
            text_value,
        ]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .or_else(|| self.subrole.as_ref().and_then(Subrole::implied_name))
    }

    /// Whether any advertised action makes this node worth acting on.
    #[must_use]
    pub fn is_actionable(&self) -> bool {
        self.actions.iter().any(ActionName::makes_actionable)
    }

    /// Actionable for something besides its context menu. Chromium gives
    /// every element AXShowMenu, so that alone doesn't make an unnamed
    /// wrapper worth a line; a node kept for other reasons still gets a ref.
    #[must_use]
    pub fn has_primary_action(&self) -> bool {
        self.actions
            .iter()
            .any(|action| action.makes_actionable() && *action != ActionName::ShowMenu)
    }

    /// Whether `needle` occurs in any of the node's texts.
    #[must_use]
    pub fn mentions(&self, needle: &str) -> bool {
        let value = match &self.value {
            Some(AxValue::Text(text)) => Some(text.as_str()),
            Some(AxValue::Number(_) | AxValue::Boolean(_)) | None => None,
        };
        [
            self.title.as_deref(),
            self.description.as_deref(),
            self.identifier.as_deref(),
            value,
        ]
        .into_iter()
        .flatten()
        .any(|text| text.contains(needle))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TreeError {
    #[error("the snapshot has no nodes")]
    Empty,
    #[error("the root (node 0) has a parent")]
    RootHasParent,
    #[error("node {0} has no parent; only node 0 may be the root")]
    SecondRoot(u32),
    #[error("node {node} names parent {parent}, which doesn't come before it")]
    ParentNotBefore { node: u32, parent: u32 },
    #[error("the snapshot has more nodes than fit an index")]
    TooLarge,
}

/// A well-formed tree: node 0 is the root, and every other node's parent
/// comes before it (pre-order), which also rules out cycles.
#[derive(Debug, Clone, PartialEq)]
pub struct AxTree {
    nodes: Vec<AxNode>,
    parents: Vec<Option<NodeIndex>>,
    children: Vec<Vec<NodeIndex>>,
}

impl AxTree {
    /// # Errors
    /// [`TreeError`] when the list isn't a pre-order tree.
    pub fn new(nodes: Vec<(Option<NodeIndex>, AxNode)>) -> Result<Self, TreeError> {
        if nodes.is_empty() {
            return Err(TreeError::Empty);
        }
        let count = u32::try_from(nodes.len()).map_err(|_| TreeError::TooLarge)?;
        let mut parents = Vec::with_capacity(nodes.len());
        let mut children = vec![Vec::new(); nodes.len()];
        let mut kept = Vec::with_capacity(nodes.len());
        for (index, (parent, node)) in (0..count).zip(nodes) {
            match (index, parent) {
                (0, None) => {}
                (0, Some(_)) => return Err(TreeError::RootHasParent),
                (_, None) => return Err(TreeError::SecondRoot(index)),
                (_, Some(parent)) if parent.get() >= index => {
                    return Err(TreeError::ParentNotBefore {
                        node: index,
                        parent: parent.get(),
                    });
                }
                (_, Some(parent)) => children[parent.slot()].push(NodeIndex(index)),
            }
            parents.push(parent);
            kept.push(node);
        }
        Ok(Self {
            nodes: kept,
            parents,
            children,
        })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Never true: a tree has at least its root.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    #[must_use]
    pub fn get(&self, index: NodeIndex) -> Option<&AxNode> {
        self.nodes.get(index.slot())
    }

    #[must_use]
    pub fn parent(&self, index: NodeIndex) -> Option<NodeIndex> {
        self.parents.get(index.slot()).copied().flatten()
    }

    #[must_use]
    pub fn children(&self, index: NodeIndex) -> &[NodeIndex] {
        self.children.get(index.slot()).map_or(&[], Vec::as_slice)
    }

    /// Every node with its index, in pre-order.
    pub fn iter(&self) -> impl Iterator<Item = (NodeIndex, &AxNode)> {
        (0u32..).map(NodeIndex).zip(&self.nodes)
    }
}

/// Why the agent stopped walking before the end of the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Truncation {
    Complete,
    NodeBudget,
    DepthBudget,
    Deadline,
    /// The agent didn't say (an older or newer peer).
    Unreported(i32),
}

impl Truncation {
    #[must_use]
    pub const fn is_complete(self) -> bool {
        match self {
            Self::Complete => true,
            Self::NodeBudget | Self::DepthBudget | Self::Deadline | Self::Unreported(_) => false,
        }
    }
}

impl fmt::Display for Truncation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Complete => f.write_str("complete"),
            Self::NodeBudget => f.write_str("stopped at the node budget"),
            Self::DepthBudget => f.write_str("stopped at the depth budget"),
            Self::Deadline => f.write_str("stopped at the deadline"),
            Self::Unreported(raw) => write!(f, "completeness unreported ({raw})"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AxSnapshot {
    pub id: SnapshotId,
    pub app: RunningApp,
    pub tree: AxTree,
    pub truncation: Truncation,
}

/// Which part of the guest's UI to snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The frontmost app's focused (or main) window.
    FocusedWindow,
    /// Every window of one app.
    App(ProcessId),
}

/// Which elements a snapshot takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotScope {
    FocusedWindow,
    /// Resolved to the app's process when the snapshot is taken.
    FrontmostApp,
}

/// What a snapshot takes: its scope, and whether menus are in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotSpec {
    pub scope: SnapshotScope,
    pub menus: Menus,
}

/// Menus are most of a native app's tree, so they are opt-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Menus {
    Exclude,
    Include,
}

/// Whether the agent keeps a snapshot's elements for later actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retention {
    /// Kept among the agent's last few, so actions can name its nodes.
    KeptForActions,
    /// A person's look: answered, nothing kept, so the snapshots an AI
    /// agent acts on stay where they are.
    Observation,
}

/// What the agent walks and where it stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotRequest {
    pub scope: Scope,
    pub menus: Menus,
    pub max_nodes: u32,
    pub max_depth: u32,
    pub deadline: Duration,
    pub retention: Retention,
}

impl SnapshotRequest {
    /// Generous agent-side limits: the core's token budget does the real cutting.
    #[must_use]
    pub const fn new(scope: Scope, menus: Menus) -> Self {
        Self {
            scope,
            menus,
            max_nodes: 5_000,
            max_depth: 64,
            deadline: Duration::from_secs(3),
            retention: Retention::KeptForActions,
        }
    }

    /// The same walk, as a person's look the agent doesn't keep.
    #[must_use]
    pub const fn observation(self) -> Self {
        Self {
            retention: Retention::Observation,
            ..self
        }
    }
}

/// One node of one snapshot, as the agent knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElementTarget {
    pub snapshot: SnapshotId,
    pub node: NodeIndex,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElementAction {
    Perform(ActionName),
    Focus,
    /// Sets `AXSelected` (rows, which don't press).
    Select,
    SetValue(String),
}

impl fmt::Display for ElementAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Perform(ActionName::Press) => f.write_str("pressed"),
            Self::Perform(other) => write!(f, "performed {}", other.as_ax()),
            Self::Focus => f.write_str("focused"),
            Self::Select => f.write_str("selected"),
            Self::SetValue(text) => {
                write!(f, "set the value ({} characters) of", text.chars().count())
            }
        }
    }
}

/// An action on one element of one snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActOn {
    pub target: ElementTarget,
    pub action: ElementAction,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn ax_names_parse_back_to_themselves() {
        let roles = [
            "AXApplication",
            "AXWindow",
            "AXSheet",
            "AXPopover",
            "AXToolbar",
            "AXGroup",
            "AXScrollArea",
            "AXSplitGroup",
            "AXSplitter",
            "AXScrollBar",
            "AXLayoutArea",
            "AXLayoutItem",
            "AXTabGroup",
            "AXButton",
            "AXCheckBox",
            "AXRadioButton",
            "AXPopUpButton",
            "AXMenuButton",
            "AXComboBox",
            "AXTextField",
            "AXTextArea",
            "AXStaticText",
            "AXLink",
            "AXImage",
            "AXHeading",
            "AXWebArea",
            "AXList",
            "AXRadioGroup",
            "AXTable",
            "AXOutline",
            "AXBrowser",
            "AXRow",
            "AXColumn",
            "AXCell",
            "AXSlider",
            "AXIncrementor",
            "AXDisclosureTriangle",
            "AXColorWell",
            "AXProgressIndicator",
            "AXBusyIndicator",
            "AXValueIndicator",
            "AXLevelIndicator",
            "AXMenuBar",
            "AXMenuBarItem",
            "AXMenu",
            "AXMenuItem",
            "AXRuler",
            "AXRulerMarker",
            "AXSomethingNew",
        ];
        for raw in roles {
            assert_eq!(Role::parse(raw).as_ax(), raw);
        }
        for raw in [
            "AXCloseButton",
            "AXMinimizeButton",
            "AXZoomButton",
            "AXFullScreenButton",
            "AXSearchField",
            "AXSecureTextField",
            "AXOther",
        ] {
            assert_eq!(Subrole::parse(raw).unwrap().as_ax(), raw);
        }
        assert_eq!(Subrole::parse(""), None, "an empty subrole is none");
    }

    pub(crate) fn node(role: &str) -> AxNode {
        AxNode {
            role: Role::parse(role),
            subrole: None,
            title: None,
            description: None,
            identifier: None,
            value: None,
            frame: None,
            states: States::default(),
            actions: Vec::new(),
        }
    }

    fn at(parent: u32) -> Option<NodeIndex> {
        Some(NodeIndex::new(parent))
    }

    #[test]
    fn builds_children_from_preorder_parents() {
        let tree = AxTree::new(vec![
            (None, node("AXWindow")),
            (at(0), node("AXGroup")),
            (at(1), node("AXButton")),
            (at(0), node("AXStaticText")),
        ])
        .unwrap();
        assert_eq!(
            tree.children(NodeIndex::ROOT),
            &[NodeIndex::new(1), NodeIndex::new(3)]
        );
        assert_eq!(tree.parent(NodeIndex::new(2)), Some(NodeIndex::new(1)));
        assert_eq!(tree.parent(NodeIndex::ROOT), None);
        assert!(tree.children(NodeIndex::new(9)).is_empty());
    }

    #[test]
    fn malformed_trees_are_rejected() {
        assert_eq!(AxTree::new(Vec::new()), Err(TreeError::Empty));
        assert_eq!(
            AxTree::new(vec![(at(0), node("AXWindow"))]),
            Err(TreeError::RootHasParent)
        );
        assert_eq!(
            AxTree::new(vec![(None, node("AXWindow")), (None, node("AXWindow"))]),
            Err(TreeError::SecondRoot(1))
        );
        // A self-parent and a forward parent are both cycles or dangling links.
        assert_eq!(
            AxTree::new(vec![(None, node("AXWindow")), (at(1), node("AXButton"))]),
            Err(TreeError::ParentNotBefore { node: 1, parent: 1 })
        );
        assert_eq!(
            AxTree::new(vec![
                (None, node("AXWindow")),
                (at(2), node("AXButton")),
                (at(0), node("AXGroup"))
            ]),
            Err(TreeError::ParentNotBefore { node: 1, parent: 2 })
        );
    }

    #[test]
    fn element_texts_and_unknown_names_are_never_empty_or_known() {
        assert_eq!(AxText::new(""), None);
        assert_eq!(AxText::new("OK").as_deref(), Some("OK"));
        // A known name never parses as unknown, so it equals its variant.
        assert_eq!(Role::parse("AXWindow"), Role::Window);
        assert_eq!(ActionName::parse("AXPress"), ActionName::Press);
    }

    #[test]
    fn unknown_roles_and_actions_keep_their_raw_names() {
        assert_eq!(Role::parse("AXButton"), Role::Button);
        let odd = Role::parse("AXSparkline");
        assert!(matches!(&odd, Role::Unknown(name) if name.as_str() == "AXSparkline"));
        assert_eq!(odd.label(), "Sparkline");
        assert_eq!(odd.kind(), RoleKind::Wrapper);
        assert_eq!(ActionName::parse("AXPress"), ActionName::Press);
        let custom = ActionName::parse("Copy link");
        assert_eq!(custom.as_ax(), "Copy link");
        assert!(!custom.makes_actionable());
    }

    #[test]
    fn names_prefer_title_then_description_then_text_value() {
        let mut button = node("AXButton");
        button.title = AxText::new("  ");
        button.description = AxText::new("Bold");
        button.value = Some(AxValue::Text("ignored for controls".into()));
        assert_eq!(button.name(), Some("Bold"));
        let mut text = node("AXStaticText");
        text.value = Some(AxValue::Text("Hello".into()));
        assert_eq!(text.name(), Some("Hello"));
        assert_eq!(node("AXGroup").name(), None);
        let mut close = node("AXButton");
        close.subrole = Subrole::parse("AXCloseButton");
        assert_eq!(close.name(), Some("close"));
        close.description = AxText::new("Close window");
        assert_eq!(close.name(), Some("Close window"), "a real name wins");
        assert!(matches!(
            Subrole::parse("AXSortButton"),
            Some(Subrole::Other(name)) if name.as_str() == "AXSortButton"
        ));
    }

    #[test]
    fn mentions_searches_every_text() {
        let mut field = node("AXTextField");
        field.value = Some(AxValue::Text("café au lait".into()));
        assert!(field.mentions("café"));
        assert!(!field.mentions("Café"));
        field.value = Some(AxValue::Number(3.0));
        assert!(!field.mentions("3"));
    }
}
