//! What a machine bundle keeps on disk (`doppel.bundle.v1`) ⇄ core domain
//! types: `spec.pb` and `lineage.pb`. Its own schema, apart from the wire
//! packages, so a protocol change is never a storage migration.
//!
//! Reading is `TryFrom` (parse, don't validate); writing is `From`, since a
//! valid domain value always has a stored form.

use std::path::PathBuf;

use doppel_machine::bundle::CheckpointLabel;
use doppel_machine::lineage::{Lineage, Origin, Role};
use doppel_machine::machine::{
    Access, CpuCount, DiskSize, DisplaySpec, Hardware, HardwareModel, MachineIdentifier,
    MachineSpec, MemorySize, NetworkSpec, Platform, SharedDirectory, SpecError,
};

use crate::convert::ParseError;
use crate::pb::bundle::v1 as bundle;

// --- spec.pb --------------------------------------------------------------

impl TryFrom<bundle::MachineSpec> for MachineSpec {
    type Error = ParseError;

    fn try_from(spec: bundle::MachineSpec) -> Result<Self, Self::Error> {
        let display = spec.display.ok_or(ParseError::Missing("display"))?;
        let network = match spec
            .network
            .and_then(|n| n.attachment)
            .ok_or(ParseError::Missing("network"))?
        {
            bundle::network::Attachment::Nat(nat) => NetworkSpec::Nat {
                mac: nat.mac_address.parse()?,
            },
            bundle::network::Attachment::Disconnected(_) => NetworkSpec::None,
        };
        let shares = spec
            .shared_directories
            .into_iter()
            .map(|share| {
                let access = match bundle::Access::try_from(share.access) {
                    Ok(bundle::Access::ReadOnly) => Access::ReadOnly,
                    Ok(bundle::Access::ReadWrite) => Access::ReadWrite,
                    Ok(bundle::Access::Unspecified) | Err(_) => {
                        return Err(ParseError::UnknownEnum {
                            field: "shared_directories.access",
                            value: share.access,
                        });
                    }
                };
                Ok(SharedDirectory::new(
                    share.name.parse()?,
                    PathBuf::from(share.host_path),
                    access,
                )?)
            })
            .collect::<Result<_, ParseError>>()?;
        let hardware = Hardware {
            cpus: CpuCount::new(spec.cpu_count)?,
            memory: MemorySize::from_bytes(spec.memory_bytes)?,
            disk: DiskSize::from_bytes(spec.disk_bytes)?,
            display: DisplaySpec::new(
                display.width_pixels,
                display.height_pixels,
                display.pixels_per_inch,
            )?,
            network,
            shares,
        };
        let platform = Platform::from_parts(
            HardwareModel::new(spec.hardware_model.to_vec()),
            MachineIdentifier::new(spec.machine_identifier.to_vec()),
        )?;
        Ok(Self::new(hardware, platform)?)
    }
}

impl From<&MachineSpec> for bundle::MachineSpec {
    fn from(spec: &MachineSpec) -> Self {
        let display = spec.display();
        let attachment = match spec.network() {
            NetworkSpec::Nat { mac } => bundle::network::Attachment::Nat(bundle::Nat {
                mac_address: mac.to_string(),
            }),
            NetworkSpec::None => bundle::network::Attachment::Disconnected(bundle::Disconnected {}),
        };
        let (hardware_model, machine_identifier) = match spec.platform() {
            Platform::NotInstalled => (Vec::new(), Vec::new()),
            Platform::Installed {
                hardware,
                identifier,
            } => (hardware.as_bytes().to_vec(), identifier.as_bytes().to_vec()),
        };
        Self {
            cpu_count: spec.cpus().get(),
            memory_bytes: spec.memory().bytes(),
            disk_bytes: spec.disk().bytes(),
            display: Some(bundle::Display {
                width_pixels: display.width_pixels(),
                height_pixels: display.height_pixels(),
                pixels_per_inch: display.pixels_per_inch(),
            }),
            network: Some(bundle::Network {
                attachment: Some(attachment),
            }),
            hardware_model: hardware_model.into(),
            machine_identifier: machine_identifier.into(),
            shared_directories: spec
                .shares()
                .iter()
                .map(|share| bundle::SharedDirectory {
                    name: share.name().as_str().to_owned(),
                    host_path: share.host_path().to_string_lossy().into_owned(),
                    access: match share.access() {
                        Access::ReadOnly => bundle::Access::ReadOnly,
                        Access::ReadWrite => bundle::Access::ReadWrite,
                    } as i32,
                })
                .collect(),
        }
    }
}

// --- lineage.pb --------------------------------------------------------------

impl From<&Lineage> for bundle::Lineage {
    fn from(lineage: &Lineage) -> Self {
        use bundle::lineage::{Origin as StoredOrigin, Role as StoredRole};
        let role = match &lineage.role {
            Role::Working => StoredRole::Working(bundle::Working {}),
            Role::Golden { template } => StoredRole::Golden(bundle::GoldenRole {
                checkpoint: template.to_string(),
            }),
        };
        let origin = match &lineage.origin {
            Origin::Unrecorded => StoredOrigin::Unrecorded(bundle::Unrecorded {}),
            Origin::Installed => StoredOrigin::Installed(bundle::Installed {}),
            Origin::Imported => StoredOrigin::Imported(bundle::Imported {}),
            Origin::Forked { from, checkpoint } => StoredOrigin::Forked(bundle::ForkedFrom {
                machine: from.to_string(),
                checkpoint: checkpoint.to_string(),
            }),
        };
        Self {
            role: Some(role),
            origin: Some(origin),
        }
    }
}

impl TryFrom<&bundle::Lineage> for Lineage {
    type Error = ParseError;

    /// A role or origin this build doesn't know (a newer writer's) is an
    /// error, never read as working or unrecorded.
    fn try_from(stored: &bundle::Lineage) -> Result<Self, ParseError> {
        use bundle::lineage::{Origin as StoredOrigin, Role as StoredRole};
        let label = |field: &'static str, raw: &str| {
            raw.parse::<CheckpointLabel>()
                .map_err(|error| ParseError::Invalid {
                    field,
                    reason: error.to_string(),
                })
        };
        let role = match &stored.role {
            Some(StoredRole::Working(_)) => Role::Working,
            Some(StoredRole::Golden(golden)) => Role::Golden {
                template: label("lineage.golden.checkpoint", &golden.checkpoint)?,
            },
            None => return Err(ParseError::Missing("lineage.role")),
        };
        let origin = match &stored.origin {
            Some(StoredOrigin::Unrecorded(_)) => Origin::Unrecorded,
            Some(StoredOrigin::Installed(_)) => Origin::Installed,
            Some(StoredOrigin::Imported(_)) => Origin::Imported,
            Some(StoredOrigin::Forked(forked)) => Origin::Forked {
                from: forked
                    .machine
                    .parse()
                    .map_err(|error: SpecError| ParseError::Invalid {
                        field: "lineage.forked.machine",
                        reason: error.to_string(),
                    })?,
                checkpoint: label("lineage.forked.checkpoint", &forked.checkpoint)?,
            },
            None => return Err(ParseError::Missing("lineage.origin")),
        };
        Ok(Self { role, origin })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doppel_machine::machine::MacAddress;

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
            hardware_model: vec![1].into(),
            machine_identifier: vec![2].into(),
            shared_directories: vec![bundle::SharedDirectory {
                name: "tools".into(),
                host_path: "/opt/tools".into(),
                access: bundle::Access::ReadOnly as i32,
            }],
        }
    }

    #[test]
    fn spec_round_trips_through_the_domain() {
        let spec = MachineSpec::try_from(stored_spec()).unwrap();
        assert_eq!(
            spec.network(),
            NetworkSpec::Nat {
                mac: "aa:d9:39:38:2f:8a".parse::<MacAddress>().unwrap()
            }
        );
        assert_eq!(bundle::MachineSpec::from(&spec), stored_spec());
    }

    #[test]
    fn invalid_stored_specs_are_rejected_when_read() {
        let mut no_network = stored_spec();
        no_network.network = None;
        assert_eq!(
            MachineSpec::try_from(no_network),
            Err(ParseError::Missing("network"))
        );

        let mut half_installed = stored_spec();
        half_installed.machine_identifier = Default::default();
        assert_eq!(
            MachineSpec::try_from(half_installed),
            Err(ParseError::Spec(SpecError::PartialPlatform))
        );

        let mut bad_access = stored_spec();
        bad_access.shared_directories[0].access = 99;
        assert!(matches!(
            MachineSpec::try_from(bad_access),
            Err(ParseError::UnknownEnum { value: 99, .. })
        ));

        let mut bad_mac = stored_spec();
        bad_mac.network = Some(bundle::Network {
            attachment: Some(bundle::network::Attachment::Nat(bundle::Nat {
                mac_address: "nope".into(),
            })),
        });
        assert!(matches!(
            MachineSpec::try_from(bad_mac),
            Err(ParseError::Spec(SpecError::InvalidMac(_)))
        ));
    }

    #[test]
    fn lineage_round_trips() {
        let golden = Role::Golden {
            template: "ready".parse().unwrap(),
        };
        let forked = Origin::Forked {
            from: "golden-27".parse().unwrap(),
            checkpoint: "ready".parse().unwrap(),
        };
        for role in [Role::Working, golden] {
            for origin in [
                Origin::Unrecorded,
                Origin::Installed,
                Origin::Imported,
                forked.clone(),
            ] {
                let lineage = Lineage {
                    role: role.clone(),
                    origin,
                };
                let stored = bundle::Lineage::from(&lineage);
                assert_eq!(Lineage::try_from(&stored), Ok(lineage));
            }
        }
    }

    #[test]
    fn a_lineage_this_build_cannot_read_is_never_a_default() {
        use bundle::lineage::{Origin as StoredOrigin, Role as StoredRole};
        let good = bundle::Lineage::from(&Lineage::default());
        let refused = |change: fn(&mut bundle::Lineage)| {
            let mut stored = good.clone();
            change(&mut stored);
            Lineage::try_from(&stored).is_err()
        };
        // A newer writer's role or origin reads as unset.
        assert!(refused(|stored| stored.role = None));
        assert!(refused(|stored| stored.origin = None));
        assert!(refused(|stored| {
            stored.role = Some(StoredRole::Golden(bundle::GoldenRole {
                checkpoint: "Not A Label".into(),
            }));
        }));
        assert!(refused(|stored| {
            stored.origin = Some(StoredOrigin::Forked(bundle::ForkedFrom {
                machine: String::new(),
                checkpoint: "ready".into(),
            }));
        }));
    }
}
