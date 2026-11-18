//! One module per family of commands. Each command is its arguments
//! (a clap `Args` struct, or a `Subcommand` enum for a group) and a `run`
//! that does it with the [`Context`](crate::context::Context).

pub(crate) mod agent;
pub(crate) mod checkpoints;
pub(crate) mod daemon;
pub(crate) mod debug;
pub(crate) mod doctor;
pub(crate) mod golden;
pub(crate) mod guest;
pub(crate) mod lifecycle;
pub(crate) mod machines;
pub(crate) mod mcp;
pub(crate) mod record;
pub(crate) mod sessions;
pub(crate) mod view;
