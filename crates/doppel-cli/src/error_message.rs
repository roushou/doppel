//! How a failed command is said on stderr: one line, the error and each
//! cause it doesn't already say.
//!
//! Doppel's errors write their cause into their own message (`the daemon
//! refused: {0}`, `{path}: {source}`) and also keep it as their source, so
//! a chain printed in full says each cause twice. Context added on the way
//! out (`making a machine from golden-27`) doesn't, so its cause follows it.

use std::fmt;

/// A failed command's message, each cause said once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorMessage(String);

impl ErrorMessage {
    /// The error, then each cause in its chain that the message so far
    /// doesn't already contain, joined by `: `.
    #[must_use]
    pub fn of(error: &anyhow::Error) -> Self {
        let mut message = String::new();
        for cause in error.chain() {
            let said = cause.to_string();
            if message.is_empty() {
                message = said;
            } else if !message.contains(&said) {
                message.push_str(": ");
                message.push_str(&said);
            }
        }
        Self(message)
    }
}

impl fmt::Display for ErrorMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("invalid argument `golden`: machine 'live' isn't a golden image")]
    struct Refusal;

    /// Says its cause and keeps it as its source, as Doppel's errors do.
    #[derive(Debug, thiserror::Error)]
    #[error("the daemon refused: {0}")]
    struct Refused(#[from] Refusal);

    /// Keeps its cause without saying it.
    #[derive(Debug, thiserror::Error)]
    #[error("the daemon closed the connection")]
    struct Closed(#[source] std::io::Error);

    #[test]
    fn a_cause_the_message_already_says_is_said_once() {
        let error =
            anyhow::Error::from(Refused(Refusal)).context("making a machine from golden-27");
        assert_eq!(
            ErrorMessage::of(&error).to_string(),
            "making a machine from golden-27: the daemon refused: invalid argument `golden`: machine 'live' isn't a golden image"
        );
    }

    #[test]
    fn a_cause_the_message_doesn_t_say_follows_it() {
        let error = anyhow::Error::from(Closed(std::io::Error::other("broken pipe")));
        assert_eq!(
            ErrorMessage::of(&error).to_string(),
            "the daemon closed the connection: broken pipe"
        );
    }

    #[test]
    fn a_lone_error_is_its_own_message() {
        let error = anyhow::anyhow!("give the command to run on each fork after `--`");
        assert_eq!(
            ErrorMessage::of(&error).to_string(),
            "give the command to run on each fork after `--`"
        );
    }
}
