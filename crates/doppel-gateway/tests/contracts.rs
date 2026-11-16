//! What other code and files depend on: the token book's file format, and
//! the pure modules staying pure.

use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use doppel_gateway::{TokenBook, TokenSecret, Ttl};

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// `tokens.json` as version 1 writes it: a book that reads it back, and
/// writes it byte for byte.
#[test]
fn the_token_book_file_is_pinned() {
    let path = root().join("../../fixtures/gateway/tokens.v1.json");
    let at = |seconds| UNIX_EPOCH + Duration::from_secs(seconds);
    let mut book = TokenBook::default();
    book.add(
        TokenSecret::from_bytes([1; 32]).digest(),
        Some("phone".parse().unwrap()),
        Ttl::DEFAULT,
        at(1_794_603_845),
    )
    .unwrap();
    book.add(
        TokenSecret::from_bytes([2; 32]).digest(),
        None,
        Ttl::from_secs(30 * 86_400).unwrap(),
        at(1_794_603_900),
    )
    .unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "test-only switch to regenerate fixtures"
    )]
    let update = std::env::var("DOPPEL_UPDATE_GOLDEN").is_ok();
    if update {
        std::fs::write(&path, book.to_json()).unwrap();
    }
    let pinned = std::fs::read_to_string(&path).unwrap();
    assert_eq!(book.to_json(), pinned);
    assert_eq!(TokenBook::parse(pinned.as_bytes()).unwrap(), book);
}

/// The pure modules do no I/O and read no clock: the caller does.
#[test]
fn pure_modules_import_no_io() {
    const FORBIDDEN: &[&str] = &[
        "std::fs",
        "std::io",
        "std::net",
        "std::os",
        "std::process",
        "std::thread",
        "std::env",
        "SystemTime::now",
        "Instant",
        "rustls",
        "tungstenite",
        "nix::",
    ];
    for module in ["tokens.rs", "http.rs", "framing.rs", "assets.rs"] {
        let source = std::fs::read_to_string(root().join("src").join(module)).unwrap();
        for forbidden in FORBIDDEN {
            assert!(!source.contains(forbidden), "{module} uses {forbidden}");
        }
    }
}
