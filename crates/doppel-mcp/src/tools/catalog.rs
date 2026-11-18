//! The tools, one type each: its name, what the model is told it does, the
//! arguments it takes, and the call they make. A tool's JSON schema is
//! derived from its argument type, so what `tools/list` promises is what
//! `tools/call` parses. [`ToolName`] lists them once.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use doppel_desktop::ax::compact::{CompactionPolicy, Show, Tokens};
use doppel_desktop::ax::refs::Ref;
use doppel_desktop::ax::tree::{ActOn, ActionName, ElementAction, Menus};
use doppel_desktop::ax::tree::{SnapshotScope, SnapshotSpec};
use doppel_desktop::exec::ExecRequest;
use doppel_desktop::files::{FileMode, ReadRequest, WritePolicy, WriteRequest};
use doppel_desktop::geometry::Point;
use doppel_desktop::guest_path::GuestPath;
use doppel_desktop::input::{Button, ClickCount, Input, KeyChord};
use doppel_desktop::mark::{
    Area, DEFAULT_LASTS, Lasts, MAX_LASTS, MarkAt, NOTE_MAX_CHARS, Note, Shape,
};
use doppel_desktop::screen_text::TextQuery;
use doppel_desktop::wait::{self, Condition, TextCondition, WaitSpec};
use doppel_machine::bundle::CheckpointLabel;
use doppel_machine::machine::MachineName;
use rmcp::model::{JsonObject, Tool as Definition};
use rmcp::schemars::generate::SchemaSettings;
use rmcp::schemars::transform::RecursiveTransform;
use rmcp::schemars::{JsonSchema, Schema};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use doppel_desktop::input::Via;
use doppel_desktop::screen::ScreenshotSize;

use super::{
    ArgumentError, MCP_MAX_FILE, MCP_MAX_OUTPUT, SnapshotListing, ToolAction, ToolCall, Why,
};
use crate::refs::Refs;

/// One MCP tool.
trait Tool {
    /// Its name in `tools/list` and `tools/call`.
    const NAME: &'static str;
    /// What the model is told it does.
    const DESCRIPTION: &'static str;
    /// Its arguments, whose schema the model is shown.
    type Args: DeserializeOwned + JsonSchema;

    /// The call its arguments make.
    ///
    /// # Errors
    /// An invalid value, or no machine to target.
    fn call(args: Self::Args, at: &Parsing<'_>) -> Result<ToolCall, ArgumentError>;
}

/// What parsing a call may consult: the server's default machine, and the
/// refs of the session's latest snapshots.
pub(crate) struct Parsing<'a> {
    pub(crate) default_machine: Option<&'a MachineName>,
    pub(crate) refs: &'a Refs,
}

impl Parsing<'_> {
    /// The machine a call names, or the server's default.
    fn machine(&self, raw: Option<String>) -> Result<MachineName, ArgumentError> {
        match raw {
            Some(raw) => raw
                .parse()
                .map_err(|e| ArgumentError::Value(format!("machine: {e}"))),
            None => self
                .default_machine
                .cloned()
                .ok_or(ArgumentError::NoMachine),
        }
    }

    /// A ref of the machine's latest snapshot, resolved.
    fn resolve(
        &self,
        machine: &MachineName,
        raw: &str,
    ) -> Result<(Ref, doppel_desktop::ax::refs::RefTarget), ArgumentError> {
        let r: Ref = raw.parse().map_err(|e| value(&e))?;
        let target = self
            .refs
            .resolve(machine, r)
            .map_err(|e| value(&e))?
            .clone();
        Ok((r, target))
    }
}

macro_rules! tools {
    ($($variant:ident),* $(,)?) => {
        /// Every tool, in the order `tools/list` gives them.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(crate) enum ToolName {
            $($variant,)*
        }

        impl ToolName {
            pub(crate) const ALL: &[Self] = &[$(Self::$variant,)*];

            /// The name a client calls it by.
            pub(crate) const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => <$variant as Tool>::NAME,)*
                }
            }

            fn definition(self) -> Definition {
                match self {
                    $(Self::$variant => definition::<$variant>(),)*
                }
            }

            fn call(
                self,
                arguments: Map<String, Value>,
                at: &Parsing<'_>,
            ) -> Result<ToolCall, ArgumentError> {
                match self {
                    $(Self::$variant => call::<$variant>(arguments, at),)*
                }
            }
        }
    };
}

tools! {
    Status, Screenshot, FindText, Snapshot, Act, WaitFor, Mark, Exec, ReadFile, WriteFile,
    Click, Type, Key, Checkpoint, Rollback, Fork, Start, Stop,
}

impl std::str::FromStr for ToolName {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        Self::ALL
            .iter()
            .copied()
            .find(|tool| tool.as_str() == s)
            .ok_or(())
    }
}

impl ToolName {
    /// The `tools/list` entries.
    #[must_use]
    pub(crate) fn definitions() -> Vec<Definition> {
        Self::ALL.iter().map(|tool| tool.definition()).collect()
    }
}

impl ToolCall {
    /// Parses `tools/call` arguments into a typed call.
    ///
    /// # Errors
    /// Arguments of the wrong shape, invalid values, or no machine to target.
    pub(crate) fn parse(
        tool: ToolName,
        arguments: Map<String, Value>,
        default_machine: Option<&MachineName>,
        refs: &Refs,
    ) -> Result<Self, ArgumentError> {
        tool.call(
            arguments,
            &Parsing {
                default_machine,
                refs,
            },
        )
    }
}

/// What every tool takes besides its own arguments.
const WHY: &str = "Optional: why you make this call, in a few words (\"to check the signup form\"). The people watching see it beside your pointer.";

fn definition<T: Tool>() -> Definition {
    let mut schema = input_schema::<T::Args>();
    if let Some(Value::Object(properties)) = schema.get_mut("properties") {
        properties.insert(
            "why".to_owned(),
            serde_json::json!({ "type": "string", "maxLength": Why::MAX_CHARS, "description": WHY }),
        );
    }
    Definition::new(T::NAME, T::DESCRIPTION, Arc::new(schema))
}

fn call<T: Tool>(
    mut arguments: Map<String, Value>,
    at: &Parsing<'_>,
) -> Result<ToolCall, ArgumentError> {
    let why = match arguments.remove("why") {
        None | Some(Value::Null) => None,
        Some(Value::String(raw)) if raw.trim().is_empty() => None,
        Some(Value::String(raw)) => Some(raw.parse::<Why>().map_err(ArgumentError::Value)?),
        Some(_) => return Err(ArgumentError::Shape("why must be a string".into())),
    };
    let args = serde_json::from_value(Value::Object(arguments))
        .map_err(|e| ArgumentError::Shape(e.to_string()))?;
    Ok(ToolCall {
        why,
        ..T::call(args, at)?
    })
}

/// An argument type's schema as a model reads it: inline, an optional
/// argument as its own type (it may be left out, not sent as null), and no
/// numeric formats.
fn input_schema<T: JsonSchema>() -> JsonObject {
    let mut settings = SchemaSettings::draft2020_12();
    settings.inline_subschemas = true;
    settings.meta_schema = None;
    settings = settings.with_transform(RecursiveTransform(compact));
    let schema = settings.into_generator().into_root_schema_for::<T>();
    // A struct's schema is always an object, never `true` or `false`.
    let mut object = schema.as_object().cloned().unwrap_or_default();
    // The Rust type's name means nothing to the model.
    object.remove("title");
    object
        .entry("required")
        .or_insert_with(|| Value::Array(Vec::new()));
    object
}

/// Leaves out what a model doesn't need: numeric formats, and the null an
/// optional argument may also be (leaving it out says the same).
fn compact(schema: &mut Schema) {
    schema.remove("format");
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    if let Some(Value::Array(types)) = object.get_mut("type") {
        types.retain(|kind| kind != "null");
        if let [only] = types.as_slice() {
            let only = only.clone();
            object.insert("type".to_owned(), only);
        }
    }
    if let Some(Value::Array(values)) = object.get_mut("enum") {
        values.retain(|value| !value.is_null());
    }
    if let Some(Value::Array(any_of)) = object.get("anyOf") {
        let not_null: Vec<&Value> = any_of
            .iter()
            .filter(|branch| branch.get("type").and_then(Value::as_str) != Some("null"))
            .collect();
        if let [only] = not_null.as_slice()
            && let Value::Object(only) = (*only).clone()
        {
            object.remove("anyOf");
            for (key, value) in only {
                object.entry(key).or_insert(value);
            }
        }
    }
}

fn value(e: &dyn std::fmt::Display) -> ArgumentError {
    ArgumentError::Value(e.to_string())
}

fn label(raw: &str, field: &str) -> Result<CheckpointLabel, ArgumentError> {
    raw.parse()
        .map_err(|e| ArgumentError::Value(format!("{field}: {e}")))
}

fn millis(time: Duration) -> u64 {
    u64::try_from(time.as_millis()).unwrap_or(u64::MAX)
}

// --- What several tools share --------------------------------------------------

const MACHINE: &str =
    "Machine name (see `doppel ls`). Optional when the server was started with --machine.";
const VIA: &str = "agent (default): doppel-agent inside the logged-in guest. host: the VM's own display and virtual keyboard/pointer; works before login and without the agent, but types US-layout characters only.";
const SCOPE: &str = "window (default): the frontmost app's focused window. app: all of the frontmost app's windows.";
const MENUS: &str = "Menus are most of a native app's tree; excluded by default.";
const SHOW: &str = "all (default): every element. changes: only what appeared, changed or went since this machine's previous listing; the rest keeps its refs. Use it after acting, to see what the act did.";
const CHECKPOINT: &str = "Checkpoint label: 1–64 of a-z, 0-9, '-' and '_'. The status tool lists a machine's checkpoints.";

fn max_tokens_description() -> String {
    format!(
        "Budget for the listing (estimated tokens). Default {}.",
        Tokens::DEFAULT_BUDGET.get()
    )
}

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "lowercase")]
enum WireVia {
    Agent,
    Host,
}

impl WireVia {
    /// The path input takes: the agent unless asked otherwise.
    fn input(raw: Option<Self>) -> Via {
        match raw {
            None | Some(Self::Agent) => Via::Agent,
            Some(Self::Host) => Via::Host,
        }
    }
}

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "lowercase")]
enum WireScope {
    Window,
    App,
}

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "lowercase")]
enum WireMenus {
    Exclude,
    Include,
}

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "lowercase")]
enum WireShow {
    All,
    Changes,
}

fn snapshot_spec(
    scope: Option<WireScope>,
    menus: Option<WireMenus>,
    max_tokens: Option<u32>,
    show: Option<WireShow>,
) -> Result<SnapshotListing, ArgumentError> {
    let budget = match max_tokens {
        None => Tokens::DEFAULT_BUDGET,
        Some(n) => Tokens::budget(n).map_err(|e| ArgumentError::Value(e.to_string()))?,
    };
    Ok(SnapshotListing {
        spec: SnapshotSpec {
            scope: match scope {
                None | Some(WireScope::Window) => SnapshotScope::FocusedWindow,
                Some(WireScope::App) => SnapshotScope::FrontmostApp,
            },
            menus: match menus {
                None | Some(WireMenus::Exclude) => Menus::Exclude,
                Some(WireMenus::Include) => Menus::Include,
            },
        },
        policy: CompactionPolicy {
            budget,
            show: match show {
                None | Some(WireShow::All) => Show::All,
                Some(WireShow::Changes) => Show::Changes,
            },
            ..CompactionPolicy::default()
        },
    })
}

// The arguments of a tool that only names its machine.
#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct MachineArgs {
    #[schemars(description = MACHINE)]
    machine: Option<String>,
}

// --- Looking ---------------------------------------------------------------------

struct Status;

impl Tool for Status {
    const NAME: &'static str = "status";
    const DESCRIPTION: &'static str = "Report a machine's run state and, when it's running, its guest agent: session, display size, frontmost app and missing permissions.";
    type Args = MachineArgs;

    fn call(args: MachineArgs, at: &Parsing<'_>) -> Result<ToolCall, ArgumentError> {
        Ok(ToolCall {
            why: None,
            machine: at.machine(args.machine)?,
            action: ToolAction::Status,
        })
    }
}

struct Screenshot;

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "lowercase")]
enum ScreenshotVia {
    Host,
    Agent,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ScreenshotArgs {
    #[schemars(description = MACHINE)]
    machine: Option<String>,
    #[schemars(
        description = "host (default): the VM's own display, as you'd see it in its window; works locked, before login and without the agent. agent: captured inside the guest by doppel-agent (needs Screen Recording there)."
    )]
    via: Option<ScreenshotVia>,
    #[schemars(
        range(min = 1),
        description = "Instead of matching display points, downscale so the longest side is at most this many pixels."
    )]
    max_dimension: Option<u32>,
}

impl Tool for Screenshot {
    const NAME: &'static str = "screenshot";
    const DESCRIPTION: &'static str = "Capture the machine's screen. By default one image pixel is one display point, so coordinates read off the image can be passed to click as they are.";
    type Args = ScreenshotArgs;

    fn call(args: ScreenshotArgs, at: &Parsing<'_>) -> Result<ToolCall, ArgumentError> {
        let size = match args.max_dimension {
            None => ScreenshotSize::MatchPoints,
            Some(0) => {
                return Err(ArgumentError::Value(
                    "max_dimension must be at least 1".into(),
                ));
            }
            Some(n) => ScreenshotSize::MaxPixels(n),
        };
        Ok(ToolCall {
            why: None,
            machine: at.machine(args.machine)?,
            action: ToolAction::Screenshot {
                // The host by default: no guest permission, works locked.
                via: match args.via {
                    None | Some(ScreenshotVia::Host) => Via::Host,
                    Some(ScreenshotVia::Agent) => Via::Agent,
                },
                size,
            },
        })
    }
}

struct FindText;

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct FindTextArgs {
    #[schemars(description = MACHINE)]
    machine: Option<String>,
    #[schemars(description = "Text to find. Omit to read everything.")]
    text: Option<String>,
}

impl Tool for FindText {
    const NAME: &'static str = "find_text";
    const DESCRIPTION: &'static str = "Read the text on screen by text recognition (OCR of the VM's own display), with a point to click for each line. With text, list only where it appears (case-insensitive), pointing at the matched words. For apps whose snapshot shows no useful elements; works locked, before login and without the agent.";
    type Args = FindTextArgs;

    fn call(args: FindTextArgs, at: &Parsing<'_>) -> Result<ToolCall, ArgumentError> {
        let query = args
            .text
            .map(|raw| raw.parse::<TextQuery>())
            .transpose()
            .map_err(|e| ArgumentError::Value(format!("text: {e}")))?;
        Ok(ToolCall {
            why: None,
            machine: at.machine(args.machine)?,
            action: ToolAction::FindText(query),
        })
    }
}

struct Snapshot;

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct SnapshotArgs {
    #[schemars(description = MACHINE)]
    machine: Option<String>,
    #[schemars(description = SCOPE)]
    scope: Option<WireScope>,
    #[schemars(description = MENUS)]
    menus: Option<WireMenus>,
    #[schemars(range(min = Tokens::MIN_BUDGET.get()), description = max_tokens_description())]
    max_tokens: Option<u32>,
    #[schemars(description = SHOW)]
    show: Option<WireShow>,
}

impl Tool for Snapshot {
    const NAME: &'static str = "snapshot";
    const DESCRIPTION: &'static str = "List the UI's accessibility elements as compact text. Elements you can act on carry a ref like [s3e12]; pass it to act. Each snapshot's refs replace the previous ones, except that with show=changes the elements as they were keep theirs. Needs the guest agent.";
    type Args = SnapshotArgs;

    fn call(args: SnapshotArgs, at: &Parsing<'_>) -> Result<ToolCall, ArgumentError> {
        Ok(ToolCall {
            why: None,
            machine: at.machine(args.machine)?,
            action: ToolAction::Snapshot(snapshot_spec(
                args.scope,
                args.menus,
                args.max_tokens,
                args.show,
            )?),
        })
    }
}

struct Act;

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "snake_case")]
enum WireAction {
    Press,
    Select,
    Focus,
    SetValue,
    ShowMenu,
    Raise,
    Increment,
    Decrement,
    Confirm,
    Cancel,
    Pick,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ActArgs {
    #[schemars(description = MACHINE)]
    machine: Option<String>,
    #[serde(rename = "ref")]
    #[schemars(description = "e.g. s3e12")]
    target: String,
    action: WireAction,
    #[schemars(description = "The text for set_value, and only for it.")]
    value: Option<String>,
}

impl Tool for Act {
    const NAME: &'static str = "act";
    const DESCRIPTION: &'static str = "Act on an element by its ref from the latest snapshot: press (buttons, links, menu items), select (rows in lists, tables, sidebars; a cell or its text selects its row), focus, set_value (text fields; give value), show_menu, raise (windows), increment/decrement (sliders, steppers), confirm, cancel, pick. A ref from an older snapshot, or to an element that has gone, is an error: snapshot again. Effects are asynchronous: use wait_for or snapshot to see them.";
    type Args = ActArgs;

    fn call(args: ActArgs, at: &Parsing<'_>) -> Result<ToolCall, ArgumentError> {
        let machine = at.machine(args.machine)?;
        let action = match (args.action, args.value) {
            (WireAction::SetValue, Some(text)) => ElementAction::SetValue(text),
            (WireAction::SetValue, None) => {
                return Err(ArgumentError::Value("set_value needs a value".into()));
            }
            (_, Some(_)) => {
                return Err(ArgumentError::Value("value is only for set_value".into()));
            }
            (WireAction::Focus, None) => ElementAction::Focus,
            (WireAction::Select, None) => ElementAction::Select,
            (WireAction::Press, None) => ElementAction::Perform(ActionName::Press),
            (WireAction::ShowMenu, None) => ElementAction::Perform(ActionName::ShowMenu),
            (WireAction::Raise, None) => ElementAction::Perform(ActionName::Raise),
            (WireAction::Increment, None) => ElementAction::Perform(ActionName::Increment),
            (WireAction::Decrement, None) => ElementAction::Perform(ActionName::Decrement),
            (WireAction::Confirm, None) => ElementAction::Perform(ActionName::Confirm),
            (WireAction::Cancel, None) => ElementAction::Perform(ActionName::Cancel),
            (WireAction::Pick, None) => ElementAction::Perform(ActionName::Pick),
        };
        let (r, target) = at.resolve(&machine, &args.target)?;
        Ok(ToolCall {
            why: None,
            machine,
            action: ToolAction::Act {
                r,
                act: ActOn {
                    target: target.element,
                    action,
                },
                target,
            },
        })
    }
}

struct WaitFor;

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "snake_case")]
enum WireUntil {
    Appears,
    Disappears,
    Frontmost,
    TextAppears,
    TextDisappears,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct WaitArgs {
    #[schemars(description = MACHINE)]
    machine: Option<String>,
    until: WireUntil,
    text: String,
    #[schemars(
        range(min = 1, max = millis(wait::MAX_TIMEOUT)),
        description = format!("Default {}.", wait::DEFAULT_TIMEOUT.as_millis())
    )]
    timeout_ms: Option<u64>,
    #[schemars(description = SCOPE)]
    scope: Option<WireScope>,
    #[schemars(description = MENUS)]
    menus: Option<WireMenus>,
    #[schemars(range(min = Tokens::MIN_BUDGET.get()), description = max_tokens_description())]
    max_tokens: Option<u32>,
    #[schemars(description = SHOW)]
    show: Option<WireShow>,
}

impl Tool for WaitFor {
    const NAME: &'static str = "wait_for";
    const DESCRIPTION: &'static str = "Poll until a condition holds. appears/disappears: some accessibility element's title, description, identifier or text value contains text (case-sensitive); returns a fresh snapshot (with new refs; show=changes lists only what changed). frontmost: the frontmost app's name or bundle id equals text. text_appears/text_disappears: text recognized on screen, as find_text reads it (case-insensitive); returns where it appeared, with points to click, and works without the agent. It sees only what is drawn: text scrolled out of view never appears, so for text an app exposes (most do), wait with appears. On timeout, returns an error with what was last seen.";
    type Args = WaitArgs;

    fn call(args: WaitArgs, at: &Parsing<'_>) -> Result<ToolCall, ArgumentError> {
        let WaitArgs {
            machine,
            until,
            text,
            timeout_ms,
            scope,
            menus,
            max_tokens,
            show,
        } = args;
        let timeout = timeout_ms.map_or(wait::DEFAULT_TIMEOUT, Duration::from_millis);
        let ax = |condition| -> Result<ToolAction, ArgumentError> {
            Ok(ToolAction::WaitFor {
                wait: WaitSpec::new(condition, timeout).map_err(|e| value(&e))?,
                snapshot: snapshot_spec(scope, menus, max_tokens, show)?,
            })
        };
        let on_screen = |condition: fn(TextQuery) -> TextCondition| {
            if scope.is_some() || menus.is_some() || max_tokens.is_some() || show.is_some() {
                return Err(ArgumentError::Value(
                    "scope, menus, max_tokens and show apply to snapshots, not to text on screen"
                        .into(),
                ));
            }
            let query = text
                .parse()
                .map_err(|e| ArgumentError::Value(format!("text: {e}")))?;
            Ok(ToolAction::WaitForText(
                WaitSpec::new(condition(query), timeout).map_err(|e| value(&e))?,
            ))
        };
        let action = match until {
            WireUntil::Appears => ax(Condition::Appears(text.clone()))?,
            WireUntil::Disappears => ax(Condition::Disappears(text.clone()))?,
            WireUntil::Frontmost => ax(Condition::Frontmost(text.clone()))?,
            WireUntil::TextAppears => on_screen(TextCondition::Appears)?,
            WireUntil::TextDisappears => on_screen(TextCondition::Disappears)?,
        };
        Ok(ToolCall {
            why: None,
            machine: at.machine(machine)?,
            action,
        })
    }
}

struct Mark;

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "lowercase")]
enum WireShape {
    Box,
    Dot,
    Arrow,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "snake_case")]
enum WireLasts {
    Time,
    NextAction,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct MarkArgs {
    #[schemars(description = MACHINE)]
    machine: Option<String>,
    #[serde(rename = "ref")]
    #[schemars(description = "An element of the latest snapshot, e.g. s3e12.")]
    target: Option<String>,
    #[schemars(range(min = 0))]
    x: Option<f64>,
    #[schemars(range(min = 0))]
    y: Option<f64>,
    #[schemars(
        extend("exclusiveMinimum" = 0),
        description = "With height, marks an area from x, y."
    )]
    width: Option<f64>,
    #[schemars(extend("exclusiveMinimum" = 0))]
    height: Option<f64>,
    #[schemars(description = "Default: box on an element or an area, dot on a point.")]
    shape: Option<WireShape>,
    #[schemars(
        range(min = 0),
        description = "Where an arrow starts; with from_y. Default: wherever there is room."
    )]
    from_x: Option<f64>,
    #[schemars(range(min = 0))]
    from_y: Option<f64>,
    #[schemars(
        length(max = NOTE_MAX_CHARS),
        description = "A short caption shown with the mark."
    )]
    note: Option<String>,
    #[schemars(
        range(min = 1, max = MAX_LASTS.as_secs()),
        description = format!("How long it stays. Default {}.", DEFAULT_LASTS.as_secs())
    )]
    seconds: Option<u64>,
    #[schemars(
        description = "time (default): it stays for seconds. next_action: it also goes when you next act on the screen (click, type, key, act), so it can show what you are about to do."
    )]
    until: Option<WireLasts>,
}

impl Tool for Mark {
    const NAME: &'static str = "mark";
    const DESCRIPTION: &'static str = "Point something out on the machine's screen to the people watching (the viewers draw it over the screen, and recordings keep it): a box, a dot or an arrow on an element by its ref, on a point, or on an area, with a short note. It changes nothing on the machine and the guest never sees it. Use it to show what you are about to act on, what you found, or what looks wrong. Give ref, or x and y (and width and height for an area), in display points.";
    type Args = MarkArgs;

    fn call(args: MarkArgs, at: &Parsing<'_>) -> Result<ToolCall, ArgumentError> {
        let machine = at.machine(args.machine)?;
        let point = |x: f64, y: f64| Point::new(x, y).map_err(|e| value(&e));
        let (place, on) = match (args.target, args.x, args.y, args.width, args.height) {
            (Some(raw), None, None, None, None) => {
                let (r, target) = at.resolve(&machine, &raw)?;
                (MarkAt::Element(target.element), Some((r, target)))
            }
            (None, Some(x), Some(y), None, None) => (MarkAt::Point(point(x, y)?), None),
            (None, Some(x), Some(y), Some(width), Some(height)) => (
                MarkAt::Area(Area::new(x, y, width, height).map_err(|e| value(&e))?),
                None,
            ),
            _ => {
                return Err(ArgumentError::Value(
                    "give ref, or x and y, or x, y, width and height".into(),
                ));
            }
        };
        let from = match (args.from_x, args.from_y) {
            (Some(x), Some(y)) => Some(point(x, y)?),
            (None, None) => None,
            (Some(_), None) | (None, Some(_)) => {
                return Err(ArgumentError::Value(
                    "give from_x and from_y together".into(),
                ));
            }
        };
        let shape = match (args.shape, place, from) {
            (Some(WireShape::Arrow), _, from) => Shape::Arrow { from },
            (_, _, Some(_)) => {
                return Err(ArgumentError::Value(
                    "from_x and from_y are only for an arrow".into(),
                ));
            }
            (Some(WireShape::Box), _, None) => Shape::Box,
            (Some(WireShape::Dot), _, None) | (None, MarkAt::Point(_), None) => Shape::Dot,
            (None, MarkAt::Area(_) | MarkAt::Element(_), None) => Shape::Box,
        };
        let time = args.seconds.map_or(DEFAULT_LASTS, Duration::from_secs);
        let lasts = match args.until {
            None | Some(WireLasts::Time) => Lasts::For(time),
            Some(WireLasts::NextAction) => Lasts::UntilActed(time),
        };
        let note = Note::new(args.note.as_deref().unwrap_or_default()).map_err(|e| value(&e))?;
        Ok(ToolCall {
            why: None,
            machine,
            action: ToolAction::Mark {
                mark: doppel_desktop::mark::Mark::new(place, shape, note, lasts)
                    .map_err(|e| value(&e))?,
                on,
            },
        })
    }
}

// --- Running in the guest ----------------------------------------------------------

struct Exec;

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ExecArgs {
    #[schemars(description = MACHINE)]
    machine: Option<String>,
    #[schemars(description = "Absolute path, e.g. /usr/bin/sw_vers.")]
    program: String,
    #[serde(default)]
    args: Vec<String>,
    #[schemars(description = "Text written to the program's standard input.")]
    stdin: Option<String>,
    #[schemars(description = "Absolute working directory.")]
    cwd: Option<String>,
    #[schemars(
        range(min = 1, max = millis(ExecRequest::MAX_TIMEOUT)),
        description = format!(
            "The program is stopped after this. Default {}.",
            ExecRequest::DEFAULT_TIMEOUT.as_millis()
        )
    )]
    timeout_ms: Option<u64>,
    #[schemars(
        range(min = 1, max = MCP_MAX_OUTPUT),
        description = format!(
            "Kept per stream; the rest is dropped and reported. Default {}.",
            ExecRequest::DEFAULT_OUTPUT
        )
    )]
    max_output_bytes: Option<u32>,
}

impl Tool for Exec {
    const NAME: &'static str = "exec";
    const DESCRIPTION: &'static str = "Run a program in the guest as the logged-in user and return its exit status and output. No shell: give an absolute program path and its arguments (use /bin/sh with [\"-c\", \"...\"] for shell syntax). Needs the guest agent.";
    type Args = ExecArgs;

    fn call(args: ExecArgs, at: &Parsing<'_>) -> Result<ToolCall, ArgumentError> {
        let max_output = args.max_output_bytes.unwrap_or(ExecRequest::DEFAULT_OUTPUT);
        if max_output > MCP_MAX_OUTPUT {
            return Err(ArgumentError::Value(format!(
                "max_output_bytes is at most {MCP_MAX_OUTPUT} here"
            )));
        }
        let mut request = ExecRequest::new(
            args.program.parse().map_err(|e| value(&e))?,
            args.args,
            args.timeout_ms
                .map_or(ExecRequest::DEFAULT_TIMEOUT, Duration::from_millis),
            max_output,
        )
        .map_err(|e| value(&e))?;
        if let Some(stdin) = args.stdin {
            request = request.with_stdin(stdin.into_bytes());
        }
        if let Some(cwd) = args.cwd {
            request = request.in_directory(cwd.parse().map_err(|e| value(&e))?);
        }
        Ok(ToolCall {
            why: None,
            machine: at.machine(args.machine)?,
            action: ToolAction::Exec(request),
        })
    }
}

struct ReadFile;

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ReadFileArgs {
    #[schemars(description = MACHINE)]
    machine: Option<String>,
    #[schemars(description = "Absolute path in the guest.")]
    path: String,
    #[schemars(
        range(min = 1, max = MCP_MAX_FILE),
        description = format!(
            "Default and maximum here: {MCP_MAX_FILE}. Use `doppel cp` for larger files."
        )
    )]
    max_bytes: Option<u32>,
}

impl Tool for ReadFile {
    const NAME: &'static str = "read_file";
    const DESCRIPTION: &'static str = "Read a file in the guest. UTF-8 files come back as text, others as base64. A file over max_bytes is refused, never cut short. Needs the guest agent.";
    type Args = ReadFileArgs;

    fn call(args: ReadFileArgs, at: &Parsing<'_>) -> Result<ToolCall, ArgumentError> {
        let max_bytes = args.max_bytes.unwrap_or(MCP_MAX_FILE);
        if max_bytes > MCP_MAX_FILE {
            return Err(ArgumentError::Value(format!(
                "max_bytes is at most {MCP_MAX_FILE} here; use `doppel cp` for larger files"
            )));
        }
        let path: GuestPath = args.path.parse().map_err(|e| value(&e))?;
        Ok(ToolCall {
            why: None,
            machine: at.machine(args.machine)?,
            action: ToolAction::ReadFile(ReadRequest::new(path, max_bytes).map_err(|e| value(&e))?),
        })
    }
}

struct WriteFile;

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "snake_case")]
enum WireIfExists {
    Fail,
    Replace,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct WriteFileArgs {
    #[schemars(description = MACHINE)]
    machine: Option<String>,
    #[schemars(description = "Absolute path in the guest.")]
    path: String,
    text: Option<String>,
    #[schemars(description = "Binary content, standard base64.")]
    base64: Option<String>,
    #[schemars(description = "Octal permissions, e.g. \"755\". Default 644.")]
    mode: Option<String>,
    #[schemars(
        description = "fail (default): refuse to touch an existing file. replace: replace it."
    )]
    if_exists: Option<WireIfExists>,
}

impl Tool for WriteFile {
    const NAME: &'static str = "write_file";
    const DESCRIPTION: &'static str = "Write a file in the guest, atomically (readers see the old file or the new one). Give exactly one of text or base64. The parent directory must exist. Needs the guest agent.";
    type Args = WriteFileArgs;

    fn call(args: WriteFileArgs, at: &Parsing<'_>) -> Result<ToolCall, ArgumentError> {
        let data = match (args.text, args.base64) {
            (Some(text), None) => text.into_bytes(),
            (None, Some(encoded)) => base64::engine::general_purpose::STANDARD
                .decode(encoded.as_bytes())
                .map_err(|e| ArgumentError::Value(format!("base64: {e}")))?,
            (Some(_), Some(_)) | (None, None) => {
                return Err(ArgumentError::Value(
                    "give exactly one of text or base64".into(),
                ));
            }
        };
        if data.len() > MCP_MAX_FILE as usize {
            return Err(ArgumentError::Value(format!(
                "at most {MCP_MAX_FILE} bytes here; use `doppel cp` for larger files"
            )));
        }
        let mode = match args.mode {
            None => FileMode::DEFAULT,
            Some(text) => {
                let digits = text.strip_prefix("0o").unwrap_or(&text);
                let bits = u32::from_str_radix(digits, 8).map_err(|_| {
                    ArgumentError::Value(format!("mode {text:?} is not octal, e.g. \"755\""))
                })?;
                FileMode::new(bits).map_err(|e| value(&e))?
            }
        };
        let policy = match args.if_exists {
            None | Some(WireIfExists::Fail) => WritePolicy::CreateNew,
            Some(WireIfExists::Replace) => WritePolicy::Replace,
        };
        let path: GuestPath = args.path.parse().map_err(|e| value(&e))?;
        Ok(ToolCall {
            why: None,
            machine: at.machine(args.machine)?,
            action: ToolAction::WriteFile(
                WriteRequest::new(path, data, mode, policy).map_err(|e| value(&e))?,
            ),
        })
    }
}

// --- Input -----------------------------------------------------------------------

struct Click;

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "lowercase")]
enum WireButton {
    Left,
    Right,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ClickArgs {
    #[schemars(description = MACHINE)]
    machine: Option<String>,
    #[schemars(range(min = 0))]
    x: f64,
    #[schemars(range(min = 0))]
    y: f64,
    #[schemars(description = "Default left.")]
    button: Option<WireButton>,
    #[schemars(
        range(min = 1, max = 3),
        description = "2 for a double click. Default 1."
    )]
    count: Option<u32>,
    #[schemars(description = VIA)]
    via: Option<WireVia>,
}

impl Tool for Click {
    const NAME: &'static str = "click";
    const DESCRIPTION: &'static str =
        "Click at a point in display points (top-left origin), as read off a default screenshot.";
    type Args = ClickArgs;

    fn call(args: ClickArgs, at: &Parsing<'_>) -> Result<ToolCall, ArgumentError> {
        let input = Input::Click {
            at: Point::new(args.x, args.y).map_err(|e| value(&e))?,
            button: match args.button {
                None | Some(WireButton::Left) => Button::Left,
                Some(WireButton::Right) => Button::Right,
            },
            count: ClickCount::try_from(args.count.unwrap_or(1)).map_err(|e| value(&e))?,
        };
        Ok(ToolCall {
            why: None,
            machine: at.machine(args.machine)?,
            action: ToolAction::Input {
                via: WireVia::input(args.via),
                input,
            },
        })
    }
}

struct Type;

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct TypeArgs {
    #[schemars(description = MACHINE)]
    machine: Option<String>,
    text: String,
    #[schemars(description = VIA)]
    via: Option<WireVia>,
}

impl Tool for Type {
    const NAME: &'static str = "type";
    const DESCRIPTION: &'static str =
        "Type text into whatever has keyboard focus. Newlines press Return.";
    type Args = TypeArgs;

    fn call(args: TypeArgs, at: &Parsing<'_>) -> Result<ToolCall, ArgumentError> {
        if args.text.is_empty() {
            return Err(ArgumentError::Value("text is empty".into()));
        }
        Ok(ToolCall {
            why: None,
            machine: at.machine(args.machine)?,
            action: ToolAction::Input {
                via: WireVia::input(args.via),
                input: Input::Type(args.text),
            },
        })
    }
}

struct Key;

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct KeyArgs {
    #[schemars(description = MACHINE)]
    machine: Option<String>,
    chord: String,
    #[schemars(description = VIA)]
    via: Option<WireVia>,
}

impl Tool for Key {
    const NAME: &'static str = "key";
    const DESCRIPTION: &'static str =
        "Press a key or chord: e.g. return, escape, tab, left, f5, cmd+space, cmd+shift+4, ctrl+c.";
    type Args = KeyArgs;

    fn call(args: KeyArgs, at: &Parsing<'_>) -> Result<ToolCall, ArgumentError> {
        let chord: KeyChord = args.chord.parse().map_err(|e| value(&e))?;
        Ok(ToolCall {
            why: None,
            machine: at.machine(args.machine)?,
            action: ToolAction::Input {
                via: WireVia::input(args.via),
                input: Input::Key(chord),
            },
        })
    }
}

// --- Lifecycle -------------------------------------------------------------------

struct Checkpoint;

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct CheckpointArgs {
    #[schemars(description = MACHINE)]
    machine: Option<String>,
    #[schemars(description = CHECKPOINT)]
    label: String,
}

impl Tool for Checkpoint {
    const NAME: &'static str = "checkpoint";
    const DESCRIPTION: &'static str = "Save the running machine's memory and disk at this instant under a label, then keep running (the guest pauses for a few seconds). Return to it with rollback, or make a new machine from it with fork. An existing label is refused.";
    type Args = CheckpointArgs;

    fn call(args: CheckpointArgs, at: &Parsing<'_>) -> Result<ToolCall, ArgumentError> {
        Ok(ToolCall {
            why: None,
            machine: at.machine(args.machine)?,
            action: ToolAction::Checkpoint(label(&args.label, "label")?),
        })
    }
}

struct Rollback;

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct RollbackArgs {
    #[schemars(description = MACHINE)]
    machine: Option<String>,
    #[schemars(description = CHECKPOINT)]
    checkpoint: String,
}

impl Tool for Rollback {
    const NAME: &'static str = "rollback";
    const DESCRIPTION: &'static str = "Return the machine to one of its checkpoints. DISCARDS everything since: if the machine runs it is powered off (open apps, unsaved work and disk changes are lost), then it resumes exactly at the checkpoint and the call waits for the desktop. Refs from earlier snapshots are invalid afterwards.";
    type Args = RollbackArgs;

    fn call(args: RollbackArgs, at: &Parsing<'_>) -> Result<ToolCall, ArgumentError> {
        Ok(ToolCall {
            why: None,
            machine: at.machine(args.machine)?,
            action: ToolAction::Rollback(label(&args.checkpoint, "checkpoint")?),
        })
    }
}

struct Fork;

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ForkArgs {
    #[schemars(description = MACHINE)]
    machine: Option<String>,
    #[schemars(description = CHECKPOINT)]
    checkpoint: String,
    #[schemars(description = "Name for the new machine: lowercase letters, digits and '-'.")]
    into: String,
}

impl Tool for Fork {
    const NAME: &'static str = "fork";
    const DESCRIPTION: &'static str = "Create a new, stopped machine from a checkpoint of this one; start it with start. A fork shares its source's identity, so it can't resume while its source or another fork of it runs (stop that one first), and at most two machines run at once.";
    type Args = ForkArgs;

    fn call(args: ForkArgs, at: &Parsing<'_>) -> Result<ToolCall, ArgumentError> {
        let source = at.machine(args.machine)?;
        let into: MachineName = args
            .into
            .parse()
            .map_err(|e| ArgumentError::Value(format!("into: {e}")))?;
        if into == source {
            return Err(ArgumentError::Value(
                "into must name a new machine, not the source".into(),
            ));
        }
        Ok(ToolCall {
            why: None,
            machine: source,
            action: ToolAction::Fork {
                checkpoint: label(&args.checkpoint, "checkpoint")?,
                into,
            },
        })
    }
}

struct Start;

impl Tool for Start {
    const NAME: &'static str = "start";
    const DESCRIPTION: &'static str = "Start a stopped machine, resuming its saved state if it has one (a fork, or after a rollback), and wait for the desktop.";
    type Args = MachineArgs;

    fn call(args: MachineArgs, at: &Parsing<'_>) -> Result<ToolCall, ArgumentError> {
        Ok(ToolCall {
            why: None,
            machine: at.machine(args.machine)?,
            action: ToolAction::Start,
        })
    }
}

struct Stop;

impl Tool for Stop {
    const NAME: &'static str = "stop";
    const DESCRIPTION: &'static str = "Power the machine off immediately, like pulling the plug: open apps and unsaved work are lost; the disk keeps what was written. To keep the exact state, checkpoint first.";
    type Args = MachineArgs;

    fn call(args: MachineArgs, at: &Parsing<'_>) -> Result<ToolCall, ArgumentError> {
        Ok(ToolCall {
            why: None,
            machine: at.machine(args.machine)?,
            action: ToolAction::Stop,
        })
    }
}
