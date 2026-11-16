//! The `doppel` executable: answers `ssh`'s password prompt when it is
//! run as `SSH_ASKPASS`, and otherwise runs the command line.

use std::process::ExitCode;

use clap::Parser as _;
use doppel_cli::Cli;

fn main() -> anyhow::Result<ExitCode> {
    if let Some(file) = std::env::var_os(doppel_host::bootstrap::ASKPASS_ENV) {
        return Ok(askpass(&file));
    }
    let report = Cli::parse().run()?;
    print!("{report}");
    match report.exit() {
        // The status a program run in the guest exited with.
        Some(code) => {
            std::io::Write::flush(&mut std::io::stdout())?;
            std::process::exit(code)
        }
        None => Ok(ExitCode::SUCCESS),
    }
}

/// Run by `ssh` as `SSH_ASKPASS` (see `doppel_host::bootstrap`): answers
/// a password prompt from the password file and exits.
fn askpass(password_file: &std::ffi::OsStr) -> ExitCode {
    let prompt = std::env::args().nth(1).unwrap_or_default();
    let mut stdout = std::io::stdout().lock();
    match doppel_host::bootstrap::Askpass.respond(
        &prompt,
        std::path::Path::new(password_file),
        &mut stdout,
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("doppel (askpass): {error}");
            ExitCode::FAILURE
        }
    }
}
