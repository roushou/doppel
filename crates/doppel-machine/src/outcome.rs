//! How a request to another party ended: done, failed, or unknown.
//!
//! A request that timed out or lost its connection may still have taken
//! effect: the VMM may have paused the guest, the agent may have clicked.
//! Such an outcome is unknown. It is never rounded up to success, and never
//! down to a failure that is safe to retry as if nothing happened.
//!
//! [`Outcome`] is a `Result` whose error says which of the two it was, so
//! `?` works as usual. An error type that can tell on its own implements
//! [`Failure`], and `?` then classifies it on the way out; code that knows
//! better (a VMM process seen to exit) builds [`Unconfirmed`] directly.

use std::fmt;

/// Done (`Ok`), or [`Unconfirmed`]: definitely not done, or unknown.
pub type Outcome<T, E> = Result<T, Unconfirmed<E>>;

/// Whether a request that ended in an error may have taken effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Certainty {
    /// Refused, or never delivered: nothing changed.
    NotDone,
    /// It may or may not have taken effect.
    MaybeDone,
}

/// A request that didn't end in a confirmed success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unconfirmed<E> {
    /// It definitely didn't take effect.
    Failed(E),
    /// It may have taken effect: check before relying on either answer.
    Unknown(E),
}

/// An error that knows whether the request it ended may have taken effect.
pub trait Failure {
    fn certainty(&self) -> Certainty;
}

impl<E> Unconfirmed<E> {
    /// The error with the given certainty.
    pub const fn new(certainty: Certainty, error: E) -> Self {
        match certainty {
            Certainty::NotDone => Self::Failed(error),
            Certainty::MaybeDone => Self::Unknown(error),
        }
    }

    #[must_use]
    pub const fn certainty(&self) -> Certainty {
        match self {
            Self::Failed(_) => Certainty::NotDone,
            Self::Unknown(_) => Certainty::MaybeDone,
        }
    }

    #[must_use]
    pub const fn error(&self) -> &E {
        match self {
            Self::Failed(error) | Self::Unknown(error) => error,
        }
    }

    #[must_use]
    pub fn into_error(self) -> E {
        match self {
            Self::Failed(error) | Self::Unknown(error) => error,
        }
    }

    /// The same certainty about another error, such as a wider one.
    pub fn map<F>(self, f: impl FnOnce(E) -> F) -> Unconfirmed<F> {
        match self {
            Self::Failed(error) => Unconfirmed::Failed(f(error)),
            Self::Unknown(error) => Unconfirmed::Unknown(f(error)),
        }
    }
}

/// Classifies an error by what it says about itself, so `?` on a
/// `Result<T, E>` works in a function returning `Outcome<T, E>`.
impl<E: Failure> From<E> for Unconfirmed<E> {
    fn from(error: E) -> Self {
        Self::new(error.certainty(), error)
    }
}

/// The error's own message: whether it was done is for the reader to say.
impl<E: fmt::Display> fmt::Display for Unconfirmed<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error().fmt(f)
    }
}

/// Transparent, as its message is: the error's causes are its own. Naming
/// the error as its cause would print its message twice in a chain.
impl<E: std::error::Error + 'static> std::error::Error for Unconfirmed<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error().source()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    enum Wire {
        Refused,
        Dropped,
    }

    impl Failure for Wire {
        fn certainty(&self) -> Certainty {
            match self {
                Self::Refused => Certainty::NotDone,
                Self::Dropped => Certainty::MaybeDone,
            }
        }
    }

    fn send(answer: Result<u8, Wire>) -> Outcome<u8, Wire> {
        Ok(answer? + 1)
    }

    #[test]
    fn question_mark_classifies_an_error_by_what_it_says() {
        assert_eq!(send(Ok(1)), Ok(2));
        assert_eq!(
            send(Err(Wire::Refused)),
            Err(Unconfirmed::Failed(Wire::Refused))
        );
        assert_eq!(
            send(Err(Wire::Dropped)),
            Err(Unconfirmed::Unknown(Wire::Dropped))
        );
    }

    #[test]
    fn widening_the_error_keeps_the_certainty() {
        let unknown = Unconfirmed::Unknown(Wire::Dropped).map(|w| format!("{w:?}"));
        assert_eq!(unknown, Unconfirmed::Unknown("Dropped".to_owned()));
        assert_eq!(unknown.certainty(), Certainty::MaybeDone);
        assert_eq!(unknown.to_string(), "Dropped");
    }

    #[test]
    fn an_unconfirmed_error_s_causes_are_its_error_s() {
        use std::error::Error;
        #[derive(Debug)]
        struct Outer(std::fmt::Error);
        impl fmt::Display for Outer {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("outer")
            }
        }
        impl Error for Outer {
            fn source(&self) -> Option<&(dyn Error + 'static)> {
                Some(&self.0)
            }
        }
        let error = Unconfirmed::Failed(Outer(std::fmt::Error));
        assert_eq!(error.to_string(), "outer");
        assert_eq!(
            error.source().map(ToString::to_string),
            Some(std::fmt::Error.to_string()),
            "the cause is the error's cause, not the error again"
        );
    }
}
