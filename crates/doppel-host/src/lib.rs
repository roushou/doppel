//! Doppel's imperative shell on the host.
//!
//! Thin by design: it parses inputs, asks the pure crates what to do, performs
//! the resulting I/O (files, processes, sockets) and feeds results back.

pub mod agent;
pub mod agent_install;
pub mod bootstrap;
pub mod capabilities;
pub mod dhcp_leases;
pub mod display;
pub mod files;
pub mod golden;
pub mod launcher;
pub mod login;
pub mod machines;
pub mod provision;
#[cfg(test)]
mod pure_modules;
mod shared_memory;
pub mod store;
pub mod vmm;
