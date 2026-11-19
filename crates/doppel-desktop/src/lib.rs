//! What is seen and done on a Mac's desktop, wherever that desktop runs: the
//! screen and its text, the accessibility tree and its refs, input, files
//! and programs, waits, and what the agent in the session reports.
//!
//! Pure: no filesystem, no clock, no processes, no async runtime. It decides
//! what a snapshot shows, what a ref points at and when a wait is over; the
//! shell talks to the agent and the display.

pub mod agent;
pub mod ax;
pub mod deadline;
pub mod exec;
pub mod files;
pub mod geometry;
pub mod guest_path;
pub mod guest_user;
pub mod input;
pub mod mark;
pub mod outcome;
pub mod screen;
pub mod screen_text;
pub mod wait;
