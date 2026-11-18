//! A running machine, through one connection to its VMM: what the host
//! sees and does without the guest's help (its screen, its text, input
//! through the VM's devices, recording), and connections to the agent in
//! the guest.

use std::os::unix::net::UnixStream;
use std::path::Path;

use doppel_machine::deadline::Deadline;
use doppel_machine::geometry::Point;
use doppel_machine::input::Input;
use doppel_machine::machine::MachineName;
use doppel_machine::screen::Screenshot;
use doppel_machine::screen_probe::Rgb;
use doppel_machine::screen_text::ScreenText;
use doppel_machine::vm::VsockPort;

use super::MachineError;
use crate::agent::AgentClient;
use crate::capabilities::Clock;
use crate::vmm::{RecordingStart, RecordingSummary, VmmClient};

/// A running machine, connected. The connection is its own; dropping it
/// closes it.
#[derive(Debug)]
pub struct RunningMachine<'m, C> {
    name: MachineName,
    client: VmmClient<'m, C>,
    clock: &'m C,
}

impl<'m, C: Clock> RunningMachine<'m, C> {
    pub(super) const fn new(name: MachineName, client: VmmClient<'m, C>, clock: &'m C) -> Self {
        Self {
            name,
            client,
            clock,
        }
    }

    #[must_use]
    pub const fn name(&self) -> &MachineName {
        &self.name
    }

    /// The VM's display, captured by the host: works at the login screen,
    /// and without the guest agent. `max_dimension_pixels` 0 keeps its size.
    ///
    /// # Errors
    /// See [`MachineError`].
    pub fn screenshot(
        &mut self,
        max_dimension_pixels: u32,
        by: Deadline,
    ) -> Result<Screenshot, MachineError> {
        Ok(self
            .client
            .capture_display_png(max_dimension_pixels, by)?
            .try_into()?)
    }

    /// Text on the VM's display, recognized on the host: like
    /// [`Self::screenshot`], works at the login screen without an agent.
    ///
    /// # Errors
    /// See [`MachineError`].
    pub fn recognize_text(&mut self, by: Deadline) -> Result<ScreenText, MachineError> {
        Ok(self.client.recognize_text(by)?.try_into()?)
    }

    /// The colour of the VM's display at `at`, as [`Self::screenshot`]
    /// would show it.
    ///
    /// # Errors
    /// See [`MachineError`].
    pub fn sample_display(&mut self, at: Point, by: Deadline) -> Result<Rgb, MachineError> {
        Ok(self.client.sample_display(at, by)?)
    }

    /// Input through the VM's own keyboard and pointer: reaches the login
    /// screen, and needs no agent.
    ///
    /// # Errors
    /// See [`MachineError`].
    pub fn host_input(&mut self, input: &Input, by: Deadline) -> Result<(), MachineError> {
        Ok(self.client.host_input(input.into(), by)?)
    }

    /// Starts recording the display to `path` (absolute, not existing yet).
    /// The VMM records until [`Self::stop_recording`] or the machine stops;
    /// it needs no claim, as it changes nothing.
    ///
    /// # Errors
    /// See [`MachineError`].
    pub fn start_recording(
        &mut self,
        path: &Path,
        by: Deadline,
    ) -> Result<RecordingStart, MachineError> {
        Ok(self.client.start_recording(path, by)?)
    }

    /// Stops the recording to `path` (whichever runs, if `None`) once its
    /// file is complete: give it [`super::FINISH_RECORDING_TIMEOUT`].
    ///
    /// # Errors
    /// See [`MachineError`].
    pub fn stop_recording(
        &mut self,
        path: Option<&Path>,
        by: Deadline,
    ) -> Result<RecordingSummary, MachineError> {
        Ok(self.client.stop_recording(path, by)?)
    }

    /// A guest vsock connection on `port`; the VMM hands over the socket.
    ///
    /// # Errors
    /// See [`MachineError`].
    pub fn open_agent_channel(
        &mut self,
        port: VsockPort,
        by: Deadline,
    ) -> Result<UnixStream, MachineError> {
        let fd = self.client.open_agent_channel(port.get(), by)?;
        Ok(UnixStream::from(fd))
    }

    /// A fresh connection to `doppel-agent` in the guest, made by `by`.
    /// Each request on it then takes its own deadline.
    ///
    /// # Errors
    /// Nothing listening on the agent port, or a failed handshake.
    pub fn agent(&mut self, by: Deadline) -> Result<AgentClient<&'m C>, MachineError> {
        self.open_agent(self.clock, by)
    }

    /// A connection to the agent to keep between calls (it owns a copy of
    /// the clock), subscribed to its events when it can push them.
    ///
    /// # Errors
    /// As [`Self::agent`], or a failed subscription.
    pub fn connect_agent(&mut self, by: Deadline) -> Result<AgentClient<C>, MachineError>
    where
        C: Clone,
    {
        let mut client = self.open_agent(self.clock.clone(), by)?;
        client.subscribe(by)?;
        Ok(client)
    }

    fn open_agent<K: Clock>(
        &mut self,
        clock: K,
        by: Deadline,
    ) -> Result<AgentClient<K>, MachineError> {
        let fd = self
            .client
            .open_agent_channel(VsockPort::AGENT.get(), by)
            .map_err(|source| MachineError::AgentUnreachable {
                machine: self.name.clone(),
                source,
            })?;
        AgentClient::connect(UnixStream::from(fd), clock, by).map_err(|source| {
            MachineError::AgentHandshake {
                machine: self.name.clone(),
                source,
            }
        })
    }
}
