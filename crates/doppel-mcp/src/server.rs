//! Doppel's MCP server: an [`rmcp`] handler that parses `tools/call` into a
//! typed [`ToolCall`], hands it to a [`Runner`], and renders the outcome.
//!
//! rmcp owns JSON-RPC, the session lifecycle and protocol versions. This
//! module owns what is Doppel's: the tool list, argument parsing, and the
//! refs each snapshot hands out.

use std::future::Future;
use std::sync::{Mutex, MutexGuard, PoisonError};

use doppel_machine::machine::MachineName;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, ContentBlock, Implementation, InitializeRequestParams,
    InitializeResult, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;

use doppel_desktop::ax::refs::SnapshotRefs;

use crate::refs::Refs;
use crate::tools::{ToolCall, ToolName, ToolOutcome};

const INSTRUCTIONS: &str = "Doppel drives macOS virtual machines. Prefer snapshot, which lists the UI's \
accessibility elements as text with refs like [s3e12], then act on a ref; it's cheaper and more precise than \
pixels. Each snapshot's refs replace the previous ones. act, click, type and key (through the agent) return \
what changed in the frontmost app half a second later, with refs: act on it directly, without a snapshot in \
between. For a slower effect (a page loading, an app opening), use wait_for (it returns a fresh snapshot) \
rather than assuming it. With show=\"changes\", snapshot and wait_for list only what appeared, changed or went \
since your previous listing, and the rest keeps its refs. Use screenshot and click when an app \
exposes no useful accessibility: by default one image pixel is one display point, so x/y read off it go \
straight into click. To click text, find_text is exact where reading points off an image is an estimate: it \
recognizes the screen's text (OCR) and gives the point to click for each line, or for the words you ask for; \
wait_for with until=\"text_appears\" or \"text_disappears\" waits on that text the same way. \
Screenshots default to the VM's own display (via=\"host\"), which works even when the \
guest is locked. Input defaults to the guest agent (via=\"agent\"); before login, or when status says the agent \
is unreachable, use via=\"host\". To try something you may want to undo, checkpoint first; rollback returns \
to it (discarding everything since). Chrome exposes page content only when launched with \
--force-renderer-accessibility (e.g. exec /usr/bin/open -a \"Google Chrome\" --args \
--force-renderer-accessibility URL); otherwise snapshots show its toolbar and tabs only.";

/// The client's cancellation of one call (`notifications/cancelled`). The
/// client has stopped waiting: the runner should stop the work too.
#[derive(Debug, Clone, Default)]
pub struct Cancellation(CancellationToken);

impl Cancellation {
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }

    /// Resolves once the call is cancelled.
    pub async fn cancelled(&self) {
        self.0.cancelled().await;
    }
}

/// What running a call produced: its outcome, and what the model should
/// know besides.
#[derive(Debug)]
pub struct Ran<E> {
    pub outcome: ToolOutcome<E>,
    pub notices: Vec<Notice>,
}

/// An outcome with nothing to add.
impl<E> From<ToolOutcome<E>> for Ran<E> {
    fn from(outcome: ToolOutcome<E>) -> Self {
        Self {
            outcome,
            notices: Vec::new(),
        }
    }
}

/// Something that happened outside the model's calls, told with a call's
/// result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// A person had control of `machine` for `held` and handed it back. The
    /// call ran after that, so its own result is fresh.
    HandedBack {
        machine: MachineName,
        held: std::time::Duration,
    },
}

impl Notice {
    fn machine(&self) -> &MachineName {
        match self {
            Self::HandedBack { machine, .. } => machine,
        }
    }

    fn text(&self) -> String {
        match self {
            Self::HandedBack { machine, held } => format!(
                "Note: a person had control of {machine} for {:.0} s and handed it back. \
                 What you saw before may have changed: earlier refs on {machine} no longer \
                 apply, so take a new snapshot before acting on them.",
                held.as_secs_f64()
            ),
        }
    }
}

/// The name an MCP client gives itself in `initialize` (`clientInfo.name`),
/// as people watching its session see it: one trimmed line of at most
/// [`ClientName::MAX`] characters, never empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientName(String);

impl ClientName {
    /// The longest name kept, in characters; a longer one is cut.
    pub const MAX: usize = 64;

    /// The client's name, or `None` when it gave none worth showing: empty,
    /// or only whitespace and control characters.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let line: String = raw
            .split(char::is_control)
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let kept: String = line.chars().take(Self::MAX).collect();
        let kept = kept.trim_end();
        (!kept.is_empty()).then(|| Self(kept.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ClientName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Runs typed tool calls against machines. The shell implements it; this
/// crate never touches a machine itself.
pub trait Runner: Send + Sync + 'static {
    /// Why a call failed. The model sees its message.
    type Error: std::fmt::Display + Send;

    /// Runs one call. Failures are [`ToolOutcome::Failed`], which the model
    /// sees, with whether the call may still have taken effect.
    fn run(
        &self,
        call: ToolCall,
        cancellation: Cancellation,
    ) -> impl Future<Output = Ran<Self::Error>> + Send;

    /// Tells consoles what the model was shown the elements of a snapshot of
    /// `machine` as. Best effort: the model never hears of a failure.
    fn name(&self, machine: MachineName, refs: SnapshotRefs) -> impl Future<Output = ()> + Send;

    /// The client introduced itself, by `client` when it gave a name. Called
    /// once per connection, before any call runs: when a client that shakes
    /// hands sends `initialize` (a failure fails the handshake), or else at
    /// a stateless client's first call, by the name that call carries (a
    /// failure fails that call, and the next call introduces it again).
    fn introduce(
        &self,
        client: Option<ClientName>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

#[derive(Debug)]
pub struct Server<R> {
    runner: R,
    default_machine: Option<MachineName>,
    refs: Mutex<Refs>,
    /// Set once the runner has been introduced to the client; empty until
    /// an introduction succeeds.
    introduced: OnceCell<()>,
}

impl<R: Runner> Server<R> {
    #[must_use]
    pub fn new(runner: R, default_machine: Option<MachineName>) -> Self {
        Self {
            runner,
            default_machine,
            refs: Mutex::new(Refs::default()),
            introduced: OnceCell::new(),
        }
    }

    /// The refs are plain data, valid after any panic that poisoned the lock.
    fn refs(&self) -> MutexGuard<'_, Refs> {
        self.refs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Introduces the client to the runner, unless that's done: concurrent
    /// first calls wait on one introduction, and a failed one is tried again.
    async fn introduce(&self, client: Option<&Implementation>) -> Result<(), ErrorData> {
        self.introduced
            .get_or_try_init(|| {
                self.runner
                    .introduce(client.and_then(|client| ClientName::parse(&client.name)))
            })
            .await
            .map(|&()| ())
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))
    }
}

impl<R: Runner> ServerHandler for Server<R> {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("doppel", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    /// Introduces the client to the runner before answering: the runner's
    /// session takes its name.
    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        self.introduce(Some(&request.client_info)).await?;
        context.peer.set_peer_info(request.clone());
        self.negotiate_initialize(&request)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(ToolName::definitions()))
    }

    /// Unknown tools are protocol errors; bad arguments and failed runs are
    /// tool errors (`isError`), so the model sees them and can correct itself.
    /// A stateless client never sends `initialize`: its first call
    /// introduces it, by the name in the call's metadata.
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let tool: ToolName = request.name.parse().map_err(|()| {
            ErrorData::invalid_params(format!("unknown tool {}", request.name), None)
        })?;
        self.introduce(context.client_info().as_ref()).await?;
        let arguments = request.arguments.unwrap_or_default();
        // Parse under the lock (refs resolve here), run without it.
        let parsed = ToolCall::parse(tool, arguments, self.default_machine.as_ref(), &self.refs());
        let result = match parsed {
            Err(error) => ToolOutcome::<R::Error>::rejected(&error),
            Ok(call) => {
                // Before running, whatever the outcome: a failed rollback may still
                // have powered the guest off.
                if call.action.replaces_guest() {
                    self.refs().forget(&call.machine);
                }
                let ran = self.runner.run(call, Cancellation(context.ct)).await;
                // The lock is released before naming waits on the daemon.
                let (mut result, shown) = {
                    let mut refs = self.refs();
                    // Before the outcome: its own snapshot is fresh.
                    for notice in &ran.notices {
                        refs.forget(notice.machine());
                    }
                    let result = ran.outcome.into_result(&mut refs);
                    (result, refs.take_shown())
                };
                for (machine, named) in shown {
                    self.runner.name(machine, named).await;
                }
                result.content.extend(
                    ran.notices
                        .iter()
                        .map(|notice| ContentBlock::text(notice.text())),
                );
                result
            }
        };
        Ok(result.into())
    }
}

#[cfg(test)]
mod tests {
    use rmcp::ServiceExt;
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines};

    use doppel_desktop::outcome::Unconfirmed;

    use super::*;
    use crate::tools::ToolAction;
    use doppel_desktop::input::Via;

    /// Records each call and fails it, so tests see what the server parsed.
    #[derive(Debug, Default)]
    struct Recorder(std::sync::Arc<Mutex<Vec<ToolCall>>>);

    impl Runner for Recorder {
        type Error = &'static str;

        async fn introduce(&self, _: Option<ClientName>) -> Result<(), Self::Error> {
            Ok(())
        }

        async fn run(&self, call: ToolCall, _: Cancellation) -> Ran<Self::Error> {
            self.0.lock().unwrap().push(call);
            ToolOutcome::Failed(Unconfirmed::Failed("recorded")).into()
        }

        async fn name(&self, _: MachineName, _: SnapshotRefs) {}
    }

    /// A client speaking raw JSON-RPC lines to a served [`Server`].
    struct Wire {
        to: DuplexStream,
        from: Lines<BufReader<DuplexStream>>,
    }

    impl Wire {
        async fn start<R: Runner>(runner: R) -> Self {
            let (to_server, server_in) = tokio::io::duplex(1 << 16);
            let (server_out, from_server) = tokio::io::duplex(1 << 16);
            let handler = Server::new(runner, Some("dev".parse().unwrap()));
            tokio::spawn(async move {
                let running = handler.serve((server_in, server_out)).await.unwrap();
                let _ = running.waiting().await;
            });
            Self {
                to: to_server,
                from: BufReader::new(from_server).lines(),
            }
        }

        async fn send(&mut self, message: Value) {
            let mut line = message.to_string();
            line.push('\n');
            self.to.write_all(line.as_bytes()).await.unwrap();
        }

        async fn request(&mut self, message: Value) -> Value {
            self.send(message).await;
            let line = self.from.next_line().await.unwrap().unwrap();
            serde_json::from_str(&line).unwrap()
        }

        async fn initialized<R: Runner>(runner: R) -> Self {
            let mut wire = Self::start(runner).await;
            let init = wire
                .request(
                    json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
                    "protocolVersion":"2025-06-18","capabilities":{},
                    "clientInfo":{"name":"t","version":"0"}}}),
                )
                .await;
            assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
            assert_eq!(init["result"]["serverInfo"]["name"], "doppel");
            assert!(init["result"]["capabilities"]["tools"].is_object());
            wire.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                .await;
            wire
        }
    }

    #[tokio::test]
    async fn lists_tools_and_dispatches_calls() {
        let runner = Recorder::default();
        let calls = runner.0.clone();
        let mut wire = Wire::initialized(runner).await;
        let list = wire
            .request(json!({"jsonrpc":"2.0","id":"l","method":"tools/list"}))
            .await;
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 18);
        let done = wire
            .request(json!({"jsonrpc":"2.0","id":7,"method":"tools/call",
                "params":{"name":"key","arguments":{"chord":"cmd+space"}}}))
            .await;
        assert_eq!(done["id"], 7);
        assert_eq!(done["result"]["isError"], true);
        let calls = calls.lock().unwrap();
        assert!(matches!(
            calls.as_slice(),
            [ToolCall {
                action: ToolAction::Input {
                    via: Via::Agent,
                    ..
                },
                ..
            }]
        ));
    }

    /// Shows a snapshot for every snapshot call; status says a person had
    /// the machine; anything else succeeds as a stop.
    #[derive(Debug, Default)]
    struct Scripted(std::sync::Arc<Mutex<Vec<(MachineName, SnapshotRefs)>>>);

    impl Runner for Scripted {
        type Error = &'static str;

        async fn introduce(&self, _: Option<ClientName>) -> Result<(), Self::Error> {
            Ok(())
        }

        async fn name(&self, machine: MachineName, refs: SnapshotRefs) {
            self.0.lock().unwrap().push((machine, refs));
        }

        async fn run(&self, call: ToolCall, _: Cancellation) -> Ran<Self::Error> {
            match call.action {
                ToolAction::Snapshot(spec) => ToolOutcome::Snapshot {
                    machine: call.machine,
                    snapshot: crate::tools::tests::ax::snapshot(1, "Save"),
                    policy: spec.policy,
                }
                .into(),
                ToolAction::Status => Ran {
                    notices: vec![Notice::HandedBack {
                        machine: call.machine.clone(),
                        held: std::time::Duration::from_secs(42),
                    }],
                    outcome: ToolOutcome::Stopped {
                        machine: call.machine,
                    },
                },
                ToolAction::Screenshot { .. }
                | ToolAction::FindText(_)
                | ToolAction::Input { .. }
                | ToolAction::Act { .. }
                | ToolAction::WaitFor { .. }
                | ToolAction::WaitForText(_)
                | ToolAction::Exec(_)
                | ToolAction::ReadFile(_)
                | ToolAction::WriteFile(_)
                | ToolAction::Checkpoint(_)
                | ToolAction::Rollback(_)
                | ToolAction::Fork { .. }
                | ToolAction::Start
                | ToolAction::Stop
                | ToolAction::Mark { .. } => ToolOutcome::Stopped {
                    machine: call.machine,
                }
                .into(),
            }
        }
    }

    #[tokio::test]
    async fn refs_from_before_a_rollback_never_resolve() {
        let mut wire = Wire::initialized(Scripted::default()).await;
        let call = |id: u32, name: &str, arguments: Value| {
            json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
                "params":{"name":name,"arguments":arguments}})
        };
        let listing = wire.request(call(1, "snapshot", json!({}))).await;
        let text = listing["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("[s1e2]"), "{text}");
        // A rolled-back guest restarts the agent's snapshot ids: s1e2 could now be anything.
        let rolled = wire
            .request(call(2, "rollback", json!({ "checkpoint": "ready" })))
            .await;
        assert_eq!(rolled["result"]["isError"], false);
        let act = wire
            .request(call(3, "act", json!({ "ref": "s1e2", "action": "press" })))
            .await;
        assert_eq!(act["result"]["isError"], true, "{act}");
        let refused = act["result"]["content"][0]["text"].as_str().unwrap();
        assert!(refused.contains("snapshot"), "{refused}");
    }

    #[tokio::test]
    async fn consoles_are_told_the_refs_the_model_was_shown() {
        let runner = Scripted::default();
        let named = runner.0.clone();
        let mut wire = Wire::initialized(runner).await;
        let listing = wire
            .request(json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":"snapshot","arguments":{}}}))
            .await;
        let text = listing["result"]["content"][0]["text"].as_str().unwrap();
        let named = named.lock().unwrap();
        let [(machine, refs)] = named.as_slice() else {
            panic!("{named:?}");
        };
        assert_eq!(machine.as_str(), "dev");
        assert_eq!(refs.snapshot().get(), 1);
        // Every ref the model was shown, and only those.
        let shown: Vec<String> = refs.refs().map(|(_, r)| r.to_string()).collect();
        assert!(!shown.is_empty());
        for r in &shown {
            assert!(text.contains(&format!("[{r}]")), "{r} in {text}");
        }
        assert_eq!(text.matches("[s1e").count(), shown.len(), "{text}");
    }

    #[tokio::test]
    async fn after_a_person_had_the_machine_the_result_says_so_and_old_refs_go() {
        let mut wire = Wire::initialized(Scripted::default()).await;
        let call = |id: u32, name: &str, arguments: Value| {
            json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
                "params":{"name":name,"arguments":arguments}})
        };
        wire.request(call(1, "snapshot", json!({}))).await;
        let status = wire.request(call(2, "status", json!({}))).await;
        let content = status["result"]["content"].as_array().unwrap();
        let note = content.last().unwrap()["text"].as_str().unwrap();
        assert!(
            note.starts_with("Note: a person had control of dev for 42 s"),
            "{note}"
        );
        let act = wire
            .request(call(3, "act", json!({ "ref": "s1e2", "action": "press" })))
            .await;
        assert_eq!(act["result"]["isError"], true, "{act}");
    }

    #[tokio::test]
    async fn bad_calls_are_tool_errors_unknown_tools_are_protocol_errors() {
        let runner = Recorder::default();
        let calls = runner.0.clone();
        let mut wire = Wire::initialized(runner).await;
        let bad = wire
            .request(json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":"click","arguments":{"x":1}}}))
            .await;
        assert_eq!(bad["result"]["isError"], true);
        let unknown = wire
            .request(
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"teleport"}}),
            )
            .await;
        assert_eq!(unknown["error"]["code"], -32602);
        assert!(
            calls.lock().unwrap().is_empty(),
            "nothing reached the runner"
        );
    }

    /// Keeps the names it's introduced by, and answers introductions with
    /// `answers` in turn, then with success.
    #[derive(Debug, Default)]
    struct Introduced {
        names: std::sync::Arc<Mutex<Vec<Option<ClientName>>>>,
        answers: Mutex<std::collections::VecDeque<Result<(), &'static str>>>,
    }

    impl Introduced {
        fn answering(answers: impl IntoIterator<Item = Result<(), &'static str>>) -> Self {
            Self {
                answers: Mutex::new(answers.into_iter().collect()),
                ..Self::default()
            }
        }
    }

    impl Runner for Introduced {
        type Error = &'static str;

        async fn introduce(&self, client: Option<ClientName>) -> Result<(), Self::Error> {
            self.names.lock().unwrap().push(client);
            self.answers.lock().unwrap().pop_front().unwrap_or(Ok(()))
        }

        async fn run(&self, _: ToolCall, _: Cancellation) -> Ran<Self::Error> {
            ToolOutcome::Failed(Unconfirmed::Failed("ran")).into()
        }

        async fn name(&self, _: MachineName, _: SnapshotRefs) {}
    }

    fn hello(name: &str) -> Value {
        json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{},
            "clientInfo":{"name":name,"version":"0"}}})
    }

    #[tokio::test]
    async fn the_runner_hears_the_clients_name_before_the_handshake_is_answered() {
        let runner = Introduced::default();
        let names = std::sync::Arc::clone(&runner.names);
        let mut wire = Wire::start(runner).await;
        let init = wire.request(hello("  agent one\n")).await;
        assert_eq!(init["result"]["serverInfo"]["name"], "doppel");
        assert_eq!(*names.lock().unwrap(), vec![ClientName::parse("agent one")]);
        wire.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await;
        let call = wire
            .request(json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":"status","arguments":{}}}))
            .await;
        assert_eq!(call["result"]["isError"], true, "{call}");
        assert_eq!(names.lock().unwrap().len(), 1, "introduced once");
    }

    #[tokio::test]
    async fn a_failed_introduction_fails_the_handshake_with_its_reason() {
        let runner = Introduced::answering([Err("no daemon")]);
        let mut wire = Wire::start(runner).await;
        let init = wire.request(hello("agent")).await;
        assert!(init["result"].is_null());
        assert_eq!(init["error"]["message"], "no daemon");
    }

    /// A call from a stateless client (MCP 2026-07-28): no `initialize`;
    /// the protocol version and the client ride in each request's metadata.
    fn stateless_call(id: u32, client: &str) -> Value {
        json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
            "name":"status","arguments":{},
            "_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28",
                "io.modelcontextprotocol/clientInfo":{"name":client,"version":"0"},
                "io.modelcontextprotocol/clientCapabilities":{}}}})
    }

    #[tokio::test]
    async fn a_stateless_client_is_introduced_once_by_the_name_its_calls_carry() {
        let runner = Introduced::default();
        let names = std::sync::Arc::clone(&runner.names);
        let mut wire = Wire::start(runner).await;
        let first = wire.request(stateless_call(1, "claude-code")).await;
        let second = wire.request(stateless_call(2, "claude-code")).await;
        for answer in [&first, &second] {
            assert_eq!(answer["result"]["content"][0]["text"], "ran", "{answer}");
        }
        assert_eq!(
            *names.lock().unwrap(),
            vec![ClientName::parse("claude-code")]
        );
    }

    #[tokio::test]
    async fn a_failed_introduction_fails_the_call_and_the_next_call_tries_again() {
        let runner = Introduced::answering([Err("no daemon")]);
        let names = std::sync::Arc::clone(&runner.names);
        let mut wire = Wire::start(runner).await;
        let refused = wire.request(stateless_call(1, "agent")).await;
        assert_eq!(refused["error"]["message"], "no daemon", "{refused}");
        let ran = wire.request(stateless_call(2, "agent")).await;
        assert_eq!(ran["result"]["content"][0]["text"], "ran", "{ran}");
        assert_eq!(names.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_client_name_is_one_trimmed_line_of_at_most_64_characters() {
        assert_eq!(
            ClientName::parse("claude-code").unwrap().as_str(),
            "claude-code"
        );
        assert_eq!(ClientName::parse("  a\nb\tc  ").unwrap().as_str(), "a b c");
        assert_eq!(
            ClientName::parse(&"x".repeat(80)).unwrap().as_str().len(),
            64
        );
        assert_eq!(ClientName::parse(""), None);
        assert_eq!(ClientName::parse(" \n\t "), None);
    }
}
