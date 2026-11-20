//! MCP (Model Context Protocol) server for Doppel, on [`rmcp`].
//!
//! [`Server`] is an rmcp handler: it parses each `tools/call` into a typed
//! [`ToolCall`], hands it to the [`Runner`] the shell provides, and renders
//! the [`ToolOutcome`]. rmcp owns JSON-RPC, the session and the transport;
//! the shell (`doppel mcp`) owns the machines.

mod refs;
mod server;
mod tools;

pub use server::{Cancellation, ClientName, Notice, Ran, Runner, Server};
pub use tools::{
    Afterwards, SnapshotListing, StatusReport, ToolAction, ToolCall, ToolOutcome, Why,
};
