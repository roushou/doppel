//! What a viewer shows of machines and the agents driving them, decided
//! without drawing: each agent's timeline of calls and what they pointed
//! at, the element tree, recordings, the machine list, and how a screen's
//! stream and a person's steering of it go. The desktop viewer draws it.
//!
//! Pure: no window toolkit, no threads, no clock; time comes in as values.

pub mod elements;
pub mod encoded;
pub mod film;
pub mod model;
pub mod panel;
pub mod recordings;
pub mod report;
pub mod resync;
pub mod stage;
pub mod steering;
pub mod timeline;
pub mod transport;
