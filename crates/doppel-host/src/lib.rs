//! Doppel's imperative shell on the host.
//!
//! Thin by design: it parses inputs, asks the pure crates what to do, performs
//! the resulting I/O (files, processes, sockets) and feeds results back.

pub mod agent;
pub mod agent_grant;
pub mod agent_install;
pub mod bootstrap;
pub mod capabilities;
mod dhcp_leases;
pub mod files;
pub mod golden;
pub mod guest_step;
pub mod guest_user;
pub mod login;
pub mod machines;
pub mod provision;
#[cfg(test)]
mod pure_modules;
pub mod restore_image;
mod screen_probe;
pub mod store;
pub mod vmm;

pub use vmm::{display, launcher};
