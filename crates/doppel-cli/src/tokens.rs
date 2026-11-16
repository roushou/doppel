//! `doppel daemon token`: a token for the browser viewer, from this
//! Mac's daemon. A token is made here, from the system's randomness,
//! and shown once; the daemon is told only its digest.

use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use doppel_gateway::{TokenLabel, TokenSecret, Ttl};
use doppel_host::capabilities::{OsEntropy, WallClock};
use doppel_machine::deadline::Deadline;
use doppel_machine::machine::Entropy as _;

use doppel_client::WebStatus;

use crate::daemon_at::DaemonAt;

/// Issues a token, printing it on stdout and what it is on stderr.
pub(crate) fn issue(
    at: &DaemonAt,
    ttl: Ttl,
    label: Option<&TokenLabel>,
    clock: &impl WallClock,
    deadline: Deadline,
) -> Result<()> {
    let mut bytes = [0; TokenSecret::BYTES];
    OsEntropy
        .fill(&mut bytes)
        .context("drawing a token from the system's randomness")?;
    let secret = TokenSecret::from_bytes(bytes);
    let client = at.connect()?;
    let entry = client.issue_token(&secret.digest(), ttl, label, deadline)?;
    println!("{}", secret.expose());
    eprintln!(
        "token {}{}: expires in {}. This is the only time it is shown.",
        entry.id,
        named(entry.label.as_ref()),
        remaining(entry.expires, clock.wall()),
    );
    match client.web_status(deadline)? {
        WebStatus::Listening {
            port,
            fingerprint,
            names,
        } => {
            let host = names.first().map_or("localhost", String::as_str);
            eprintln!("open https://{host}:{port}/#token={}", secret.expose());
            eprintln!("and check that the browser shows this certificate: SHA-256 {fingerprint}");
        }
        WebStatus::Off | WebStatus::Failed { .. } => {
            eprintln!("the browser viewer is off: `doppel daemon web on` turns it on");
        }
    }
    Ok(())
}

/// ` (label)`, or nothing for a token without one.
pub(crate) fn named(label: Option<&TokenLabel>) -> String {
    label.map_or_else(String::new, |label| format!(" ({label})"))
}

/// `12 h 0 min`, `3 d 4 h`, `59 min`: rounded down to the minute.
pub(crate) fn remaining(expires: SystemTime, now: SystemTime) -> String {
    let left = expires.duration_since(now).unwrap_or(Duration::ZERO);
    let minutes = left.as_secs() / 60;
    let (days, hours, minutes) = (minutes / 1_440, minutes / 60 % 24, minutes % 60);
    if days > 0 {
        format!("{days} d {hours} h")
    } else if hours > 0 {
        format!("{hours} h {minutes} min")
    } else {
        format!("{minutes} min")
    }
}
