//! Who steers a screen's machine, as the console sees it: the person, once
//! they've taken control, or not. Pure: the pane feeds it button presses and
//! the answers to its requests, and does what it says.

/// What the pane shows and allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Steering {
    /// View-only; `refused` says why the last request failed.
    Watching { refused: Option<String> },
    /// Take control was asked; input waits for the answer.
    Asking,
    /// The person has control: input is sent.
    Driving,
    /// Hand back was asked.
    Returning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SteerEvent {
    /// The button was pressed.
    Pressed,
    /// The request in flight succeeded.
    Answered,
    /// It failed, with why.
    Refused(String),
    /// The machine stopped: there's nothing left to steer.
    Ended,
}

/// A request for the input thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteerRequest {
    Take,
    HandBack,
}

impl Steering {
    pub const START: Self = Self::Watching { refused: None };

    /// The next state, and the request to send, if any. A press while a
    /// request is in flight is ignored: the button shows it's busy.
    pub fn on(self, event: SteerEvent) -> (Self, Option<SteerRequest>) {
        match (self, event) {
            (Self::Watching { .. }, SteerEvent::Pressed) => {
                (Self::Asking, Some(SteerRequest::Take))
            }
            (Self::Driving, SteerEvent::Pressed) => (Self::Returning, Some(SteerRequest::HandBack)),
            (state @ (Self::Asking | Self::Returning), SteerEvent::Pressed) => (state, None),
            (Self::Asking, SteerEvent::Answered) => (Self::Driving, None),
            (Self::Returning, SteerEvent::Answered) => (Self::START, None),
            (Self::Asking | Self::Returning, SteerEvent::Refused(why)) => {
                // A failed hand back leaves control with the daemon's
                // choice: closing the console hands it back for sure.
                (Self::Watching { refused: Some(why) }, None)
            }
            // Answers to nothing in flight are stale.
            (
                state @ (Self::Watching { .. } | Self::Driving),
                SteerEvent::Answered | SteerEvent::Refused(_),
            ) => (state, None),
            (
                Self::Watching { .. } | Self::Asking | Self::Driving | Self::Returning,
                SteerEvent::Ended,
            ) => (Self::START, None),
        }
    }

    /// Whether the person's input goes to the machine.
    pub const fn sends_input(&self) -> bool {
        match self {
            Self::Driving => true,
            Self::Watching { .. } | Self::Asking | Self::Returning => false,
        }
    }

    /// The button's label.
    pub const fn button(&self) -> &'static str {
        match self {
            Self::Watching { .. } => "Take control",
            Self::Asking => "Taking control…",
            Self::Driving => "Hand back",
            Self::Returning => "Handing back…",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_person_takes_control_drives_and_hands_back() {
        let (asking, request) = Steering::START.on(SteerEvent::Pressed);
        assert_eq!(
            (&asking, request),
            (&Steering::Asking, Some(SteerRequest::Take))
        );
        assert!(!asking.sends_input());
        let (driving, _) = asking.on(SteerEvent::Answered);
        assert!(driving.sends_input());
        assert_eq!(driving.button(), "Hand back");
        let (returning, request) = driving.on(SteerEvent::Pressed);
        assert_eq!(request, Some(SteerRequest::HandBack));
        assert!(!returning.sends_input());
        assert_eq!(returning.on(SteerEvent::Answered), (Steering::START, None));
    }

    #[test]
    fn a_refusal_is_shown_and_presses_in_flight_are_ignored() {
        assert_eq!(
            Steering::Asking.on(SteerEvent::Pressed),
            (Steering::Asking, None)
        );
        let (refused, _) = Steering::Asking.on(SteerEvent::Refused("held".into()));
        assert_eq!(
            refused,
            Steering::Watching {
                refused: Some("held".into())
            }
        );
        // Pressing again asks again.
        assert_eq!(
            refused.on(SteerEvent::Pressed),
            (Steering::Asking, Some(SteerRequest::Take))
        );
        assert_eq!(
            Steering::Driving.on(SteerEvent::Ended),
            (Steering::START, None)
        );
        assert_eq!(
            Steering::Driving.on(SteerEvent::Answered),
            (Steering::Driving, None),
            "a stale answer changes nothing"
        );
    }
}
