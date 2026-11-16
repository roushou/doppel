//! Everything in this crate but `transport` is sans-IO (design principles
//! §7: a pure module inside a crate that does I/O keeps a source test).
//! The manifest can't say so, since `transport` needs `nix`; this test does.

use std::fs;
use std::path::{Path, PathBuf};

/// What a sans-IO module never names: sockets, files, processes, threads,
/// clocks, the async runtime, or the transport itself.
const FORBIDDEN: &[&str] = &[
    "nix::",
    "std::net",
    "std::os::",
    "std::fs",
    "std::io::",
    "std::thread",
    "std::process",
    "std::time::Instant",
    "SystemTime",
    "tokio",
    "transport::",
];

/// The modules that do I/O, relative to `src/`.
const SHELL: &[&str] = &["transport"];

/// Whether `code` names `token` as a path of its own, not the tail of a
/// longer name (`unix::` doesn't name `nix::`).
fn names(code: &str, token: &str) -> bool {
    code.match_indices(token).any(|(at, _)| {
        !code[..at]
            .chars()
            .next_back()
            .is_some_and(|before| before.is_alphanumeric() || before == '_')
    })
}

fn sources(dir: &Path, into: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            sources(&path, into)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            into.push(path);
        }
    }
    Ok(())
}

#[test]
fn only_the_transport_does_io() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&src, &mut files).unwrap();
    let shell: Vec<PathBuf> = SHELL.iter().map(|module| src.join(module)).collect();
    let mut found = Vec::new();
    let mut checked = 0;
    for file in files
        .iter()
        .filter(|file| !shell.iter().any(|module| file.starts_with(module)))
    {
        checked += 1;
        let text = fs::read_to_string(file).unwrap();
        for (number, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or_default();
            for forbidden in FORBIDDEN.iter().filter(|forbidden| names(code, forbidden)) {
                found.push(format!(
                    "{}:{}: names `{forbidden}`",
                    file.strip_prefix(&src).unwrap().display(),
                    number + 1
                ));
            }
        }
    }
    assert!(
        checked >= 4,
        "found only {checked} pure modules under {}",
        src.display()
    );
    assert!(
        found.is_empty(),
        "I/O in a sans-IO module:\n{}",
        found.join("\n")
    );
}
