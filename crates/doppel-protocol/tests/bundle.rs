//! Storage contract: a bundle's `spec.pb` and `lineage.pb`, each one bare
//! `doppel.bundle.v1` message, no framing, committed under
//! `fixtures/bundle/`. Every bundle already written holds these bytes, so
//! a change here is a storage migration: the files only ever gain fields.
//! The Swift VMM reads the same spec file (`apple/Tests/DoppelWireTests`).
//!
//! Regenerate after an intentional schema change:
//! `DOPPEL_UPDATE_GOLDEN=1 cargo test -p doppel-protocol --test bundle`

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration test helpers: a failed unwrap is a failed test"
)]

use std::path::PathBuf;

use doppel_machine::lineage::{Lineage, Origin, Role};
use doppel_machine::machine::MachineSpec;
use doppel_protocol::pb::bundle::v1 as bundle;
use prost::Message;

/// Checks `stored` encodes to the committed file, or writes it when asked
/// to, and returns the file's bytes.
fn pinned(name: &str, stored: &impl Message) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/bundle")
        .join(name);
    let encoded = stored.encode_to_vec();
    #[expect(
        clippy::disallowed_methods,
        reason = "test-only switch to regenerate fixtures"
    )]
    let update = std::env::var("DOPPEL_UPDATE_GOLDEN").is_ok();
    if update {
        std::fs::write(&path, &encoded).unwrap();
    }
    let committed = std::fs::read(&path).unwrap();
    assert_eq!(committed, encoded, "{} changed", path.display());
    committed
}

/// An installed machine's spec, with a shared directory.
fn stored_spec() -> bundle::MachineSpec {
    bundle::MachineSpec {
        cpu_count: 4,
        memory_bytes: 4 << 30,
        disk_bytes: 64 << 30,
        display: Some(bundle::Display {
            width_pixels: 2560,
            height_pixels: 1600,
            pixels_per_inch: 220,
        }),
        network: Some(bundle::Network {
            attachment: Some(bundle::network::Attachment::Nat(bundle::Nat {
                mac_address: "aa:d9:39:38:2f:8a".into(),
            })),
        }),
        hardware_model: vec![1, 2, 3, 4].into(),
        machine_identifier: vec![9, 8, 7].into(),
        shared_directories: vec![bundle::SharedDirectory {
            name: "tools".into(),
            host_path: "/Users/someone/doppel/tools".into(),
            access: bundle::Access::ReadOnly as i32,
        }],
    }
}

/// `spec.pb` reads back as the spec it was written from.
#[test]
fn the_spec_file_is_pinned() {
    let committed = pinned("spec.v1.bin", &stored_spec());
    let stored = bundle::MachineSpec::decode(committed.as_slice()).unwrap();
    assert_eq!(stored, stored_spec());
    let spec = MachineSpec::try_from(stored).unwrap();
    assert_eq!(bundle::MachineSpec::from(&spec), stored_spec());
}

/// `lineage.pb` of a golden image installed from a restore image, its
/// template `ready`.
#[test]
fn the_lineage_file_is_pinned() {
    let lineage = Lineage {
        role: Role::Golden {
            template: "ready".parse().unwrap(),
        },
        origin: Origin::Installed,
    };
    let committed = pinned("lineage.v1.bin", &bundle::Lineage::from(&lineage));
    let stored = bundle::Lineage::decode(committed.as_slice()).unwrap();
    assert_eq!(Lineage::try_from(&stored), Ok(lineage));
}
