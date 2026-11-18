//! A Doppel machine: what it is (its spec and lineage), where its files
//! live, the procedures that change it (starting, stopping, checkpointing,
//! logging in) as state machines, and what is seen and done inside its
//! guest (screen and text, accessibility tree, input, files, programs,
//! waits), as it happens and as a session's recordings keep it.
//!
//! Pure: no filesystem, no clock, no processes, no async runtime. It computes
//! what should happen (paths, plans, state transitions) and the shell
//! (`doppel-host`) performs it.

pub mod activity;
pub mod agent;
pub mod ax;
pub mod bundle;
pub mod checkpoint;
pub mod deadline;
pub mod exec;
pub mod files;
pub mod geometry;
pub mod guest_path;
pub mod input;
pub mod lineage;
pub mod login;
pub mod machine;
pub mod mark;
pub mod outcome;
pub mod recording;
pub mod screen;
pub mod screen_probe;
pub mod screen_text;
pub mod startup;
pub mod stop;
pub mod vm;
pub mod wait;
