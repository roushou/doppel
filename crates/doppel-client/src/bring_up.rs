//! Bringing a machine up: start it, then wait for an agent-ready desktop.
//! Two requests, and a failure of the second still leaves the machine
//! running, so their result says which one failed.

use doppel_desktop::deadline::Clock;
use doppel_machine::login::Desktop;
use doppel_machine::machine::MachineName;
use doppel_machine::vm::{NextStart, Window};

use crate::{Call, Client, ClientError};

/// A start, and what came of the desktop after it.
#[derive(Debug)]
#[must_use]
pub enum BringUp {
    /// The start failed: the desktop wasn't waited for.
    NotStarted(ClientError),
    /// It runs, from `from`. The desktop wait's failure doesn't undo that.
    Started {
        from: NextStart,
        desktop: Result<Desktop, ClientError>,
    },
}

impl<C: Clock> Client<C> {
    /// Starts `name` in `window`, then waits for its desktop, both within
    /// `call`.
    pub fn bring_up<'a>(
        &self,
        name: &MachineName,
        window: Window,
        call: impl Into<Call<'a>>,
    ) -> BringUp {
        let call = call.into();
        match self.start(name, window, call) {
            Err(error) => BringUp::NotStarted(error),
            Ok((_, from)) => BringUp::Started {
                from,
                desktop: self.await_desktop(name, call),
            },
        }
    }
}
