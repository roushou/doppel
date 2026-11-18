//! A Doppel machine: what it is (its spec and lineage), where its files
//! live, the procedures that change it (starting, stopping, checkpointing,
//! logging in) as state machines, and its activity as a session's
//! recordings keep it. What is seen and done on its desktop is
//! `doppel-desktop`'s.
//!
//! Pure: no filesystem, no clock, no processes, no async runtime. It computes
//! what should happen (paths, plans, state transitions) and the shell
//! (`doppel-host`) performs it.

pub mod activity;
pub mod bundle;
pub mod checkpoint;
pub mod lineage;
pub mod login;
pub mod machine;
pub mod recording;
pub mod screen_probe;
pub mod startup;
pub mod stop;
pub mod vm;
