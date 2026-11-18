//! What a machine *is*: its persisted specification.
//!
//! Restore requires a byte-identical VZ configuration, so every
//! input VZ would otherwise randomise, notably the MAC address, lives here.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use doppel_desktop::geometry::{Scale, Size};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpecError {
    #[error(
        "machine names are 1–32 characters: lowercase letters, digits and '-', starting with a letter or digit (got {0:?})"
    )]
    InvalidName(String),
    #[error("invalid MAC address {0:?}; expected aa:bb:cc:dd:ee:ff")]
    InvalidMac(String),
    #[error("MAC address {0} is multicast; a NIC needs a unicast address")]
    MulticastMac(MacAddress),
    #[error("{field} must be greater than zero")]
    Zero { field: &'static str },
    #[error("memory must be a whole number of MiB (got {0} bytes)")]
    MemoryNotMibAligned(u64),
    #[error(
        "shared directory names are 1–64 characters of letters, digits, '-' or '_' (got {0:?})"
    )]
    InvalidShareName(String),
    #[error("shared directory names must be unique ({0:?} appears twice)")]
    DuplicateShareName(String),
    #[error("host path must be absolute (got {0:?})")]
    RelativePath(PathBuf),
    #[error("hardware model and machine identifier must be both set or both empty")]
    PartialPlatform,
    #[error("machine is already installed")]
    AlreadyInstalled,
}

// --- Identity ---------------------------------------------------------------

/// A machine's name: also its directory name, so it's restricted to a portable,
/// shell-safe subset and short enough for unix socket paths.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MachineName(String);

impl MachineName {
    pub const MAX_LEN: usize = 32;

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for MachineName {
    type Err = SpecError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let valid = !s.is_empty()
            && s.len() <= Self::MAX_LEN
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && s.bytes().next().is_some_and(|b| b != b'-');
        if valid {
            Ok(Self(s.to_owned()))
        } else {
            Err(SpecError::InvalidName(s.to_owned()))
        }
    }
}

impl fmt::Display for MachineName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Source of random bytes. A capability: the core never reaches for randomness.
pub trait Entropy {
    type Error;
    /// # Errors
    /// The source could not produce bytes.
    fn fill(&mut self, bytes: &mut [u8]) -> Result<(), Self::Error>;
}

/// A unicast Ethernet address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MacAddress([u8; 6]);

impl MacAddress {
    const MULTICAST: u8 = 0b01;
    const LOCALLY_ADMINISTERED: u8 = 0b10;

    /// A random locally administered unicast address, as VZ itself generates.
    ///
    /// # Errors
    /// The entropy source failed.
    pub fn generate<E: Entropy>(entropy: &mut E) -> Result<Self, E::Error> {
        let mut bytes = [0; 6];
        entropy.fill(&mut bytes)?;
        bytes[0] = (bytes[0] | Self::LOCALLY_ADMINISTERED) & !Self::MULTICAST;
        Ok(Self(bytes))
    }

    #[must_use]
    pub const fn octets(self) -> [u8; 6] {
        self.0
    }
}

impl FromStr for MacAddress {
    type Err = SpecError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || SpecError::InvalidMac(s.to_owned());
        let mut bytes = [0; 6];
        let mut parts = s.split(':');
        for byte in &mut bytes {
            let part = parts.next().ok_or_else(invalid)?;
            if part.len() != 2 {
                return Err(invalid());
            }
            *byte = u8::from_str_radix(part, 16).map_err(|_| invalid())?;
        }
        if parts.next().is_some() {
            return Err(invalid());
        }
        let mac = Self(bytes);
        if bytes[0] & Self::MULTICAST != 0 {
            Err(SpecError::MulticastMac(mac))
        } else {
            Ok(mac)
        }
    }
}

impl fmt::Display for MacAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d, e, g] = self.0;
        write!(f, "{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{g:02x}")
    }
}

/// VZ's opaque `VZMacHardwareModel` blob.
#[derive(Clone, PartialEq, Eq)]
pub struct HardwareModel(Vec<u8>);

/// VZ's opaque `VZMacMachineIdentifier` blob. Forks share it: restoring a
/// saved state with a different identifier fails.
#[derive(Clone, PartialEq, Eq)]
pub struct MachineIdentifier(Vec<u8>);

macro_rules! opaque_blob {
    ($ty:ident) => {
        impl $ty {
            #[must_use]
            pub fn new(bytes: Vec<u8>) -> Option<Self> {
                (!bytes.is_empty()).then_some(Self(bytes))
            }
            #[must_use]
            pub fn as_bytes(&self) -> &[u8] {
                &self.0
            }
        }
        impl fmt::Debug for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($ty), "({} bytes)"), self.0.len())
            }
        }
    };
}
opaque_blob!(HardwareModel);
opaque_blob!(MachineIdentifier);

// --- Resources --------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuCount(u32);

impl CpuCount {
    /// # Errors
    /// [`SpecError::Zero`].
    pub fn new(count: u32) -> Result<Self, SpecError> {
        if count == 0 {
            Err(SpecError::Zero { field: "cpu count" })
        } else {
            Ok(Self(count))
        }
    }
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemorySize(u64);

impl MemorySize {
    const MIB: u64 = 1 << 20;

    /// # Errors
    /// Zero, or not a whole number of MiB (VZ's requirement).
    pub fn from_bytes(bytes: u64) -> Result<Self, SpecError> {
        match bytes {
            0 => Err(SpecError::Zero { field: "memory" }),
            b if b % Self::MIB != 0 => Err(SpecError::MemoryNotMibAligned(b)),
            b => Ok(Self(b)),
        }
    }
    /// # Errors
    /// Zero, or overflow.
    pub fn from_gib(gib: u32) -> Result<Self, SpecError> {
        Self::from_bytes(u64::from(gib) << 30)
    }
    #[must_use]
    pub const fn bytes(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskSize(u64);

impl DiskSize {
    /// # Errors
    /// [`SpecError::Zero`].
    pub fn from_bytes(bytes: u64) -> Result<Self, SpecError> {
        if bytes == 0 {
            Err(SpecError::Zero { field: "disk size" })
        } else {
            Ok(Self(bytes))
        }
    }
    /// # Errors
    /// [`SpecError::Zero`].
    pub fn from_gib(gib: u32) -> Result<Self, SpecError> {
        Self::from_bytes(u64::from(gib) << 30)
    }
    #[must_use]
    pub const fn bytes(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplaySpec {
    width_pixels: u32,
    height_pixels: u32,
    pixels_per_inch: u32,
}

impl DisplaySpec {
    /// # Errors
    /// Any dimension is zero.
    pub fn new(
        width_pixels: u32,
        height_pixels: u32,
        pixels_per_inch: u32,
    ) -> Result<Self, SpecError> {
        for (value, field) in [
            (width_pixels, "display width"),
            (height_pixels, "display height"),
            (pixels_per_inch, "pixels per inch"),
        ] {
            if value == 0 {
                return Err(SpecError::Zero { field });
            }
        }
        Ok(Self {
            width_pixels,
            height_pixels,
            pixels_per_inch,
        })
    }

    /// A Retina display of the given size in points: 2× pixels at 220 ppi, so
    /// macOS picks a HiDPI mode and text stays sharp.
    ///
    /// # Errors
    /// Zero, or overflow.
    pub fn retina(width_points: u32, height_points: u32) -> Result<Self, SpecError> {
        Self::new(
            width_points.saturating_mul(2),
            height_points.saturating_mul(2),
            220,
        )
    }

    #[must_use]
    pub const fn width_pixels(self) -> u32 {
        self.width_pixels
    }
    #[must_use]
    pub const fn height_pixels(self) -> u32 {
        self.height_pixels
    }
    #[must_use]
    pub const fn pixels_per_inch(self) -> u32 {
        self.pixels_per_inch
    }

    /// The display's size in points, the input coordinate space. macOS uses a
    /// HiDPI (2×) mode at Retina densities; the VMM applies the same rule.
    #[must_use]
    pub fn points(self) -> Size {
        let scale = if self.pixels_per_inch >= 200 {
            Scale::Retina
        } else {
            Scale::Standard
        };
        Size::of_pixels(self.width_pixels, self.height_pixels, scale)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkSpec {
    Nat { mac: MacAddress },
    None,
}

// --- Shared directories -------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShareName(String);

impl ShareName {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ShareName {
    type Err = SpecError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let valid = !s.is_empty()
            && s.len() <= 64
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if valid {
            Ok(Self(s.to_owned()))
        } else {
            Err(SpecError::InvalidShareName(s.to_owned()))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedDirectory {
    name: ShareName,
    host_path: PathBuf,
    access: Access,
}

impl SharedDirectory {
    /// # Errors
    /// [`SpecError::RelativePath`].
    pub fn new(name: ShareName, host_path: PathBuf, access: Access) -> Result<Self, SpecError> {
        if host_path.is_absolute() {
            Ok(Self {
                name,
                host_path,
                access,
            })
        } else {
            Err(SpecError::RelativePath(host_path))
        }
    }
    #[must_use]
    pub fn name(&self) -> &ShareName {
        &self.name
    }
    #[must_use]
    pub fn host_path(&self) -> &Path {
        &self.host_path
    }
    #[must_use]
    pub const fn access(&self) -> Access {
        self.access
    }
}

// --- The spec ---------------------------------------------------------------

/// Whether macOS has been installed. The VZ platform blobs only exist together,
/// after installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Platform {
    NotInstalled,
    Installed {
        hardware: HardwareModel,
        identifier: MachineIdentifier,
    },
}

impl Platform {
    /// Parses the two wire fields, which are valid only together.
    ///
    /// # Errors
    /// [`SpecError::PartialPlatform`] when exactly one is present.
    pub fn from_parts(
        hardware: Option<HardwareModel>,
        identifier: Option<MachineIdentifier>,
    ) -> Result<Self, SpecError> {
        match (hardware, identifier) {
            (None, None) => Ok(Self::NotInstalled),
            (Some(hardware), Some(identifier)) => Ok(Self::Installed {
                hardware,
                identifier,
            }),
            (Some(_), None) | (None, Some(_)) => Err(SpecError::PartialPlatform),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineSpec {
    cpus: CpuCount,
    memory: MemorySize,
    disk: DiskSize,
    display: DisplaySpec,
    network: NetworkSpec,
    shares: Vec<SharedDirectory>,
    platform: Platform,
}

/// Everything about a machine except its install state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hardware {
    pub cpus: CpuCount,
    pub memory: MemorySize,
    pub disk: DiskSize,
    pub display: DisplaySpec,
    pub network: NetworkSpec,
    pub shares: Vec<SharedDirectory>,
}

impl MachineSpec {
    /// # Errors
    /// [`SpecError::DuplicateShareName`]: each name maps to one guest folder.
    pub fn new(hardware: Hardware, platform: Platform) -> Result<Self, SpecError> {
        let Hardware {
            cpus,
            memory,
            disk,
            display,
            network,
            shares,
        } = hardware;
        for (i, share) in shares.iter().enumerate() {
            if shares[..i].iter().any(|s| s.name == share.name) {
                return Err(SpecError::DuplicateShareName(share.name.0.clone()));
            }
        }
        Ok(Self {
            cpus,
            memory,
            disk,
            display,
            network,
            shares,
            platform,
        })
    }

    /// Records the platform blobs produced by installing macOS.
    ///
    /// # Errors
    /// [`SpecError::AlreadyInstalled`]: identity is fixed once chosen.
    pub fn installed(
        self,
        hardware: HardwareModel,
        identifier: MachineIdentifier,
    ) -> Result<Self, SpecError> {
        match self.platform {
            Platform::NotInstalled => Ok(Self {
                platform: Platform::Installed {
                    hardware,
                    identifier,
                },
                ..self
            }),
            Platform::Installed { .. } => Err(SpecError::AlreadyInstalled),
        }
    }

    /// The same machine at another MAC address, as a fresh fork runs: it
    /// keeps the identity (a new one makes macOS treat it as a new Mac and
    /// ask for an Apple Account before the agent starts), and the MAC keeps
    /// it apart on the NAT from the machine it came from. Without a network
    /// it stays without.
    #[must_use]
    pub fn with_mac(self, mac: MacAddress) -> Self {
        let network = match self.network {
            NetworkSpec::Nat { .. } => NetworkSpec::Nat { mac },
            NetworkSpec::None => NetworkSpec::None,
        };
        Self { network, ..self }
    }

    #[must_use]
    pub const fn cpus(&self) -> CpuCount {
        self.cpus
    }
    #[must_use]
    pub const fn memory(&self) -> MemorySize {
        self.memory
    }
    #[must_use]
    pub const fn disk(&self) -> DiskSize {
        self.disk
    }
    #[must_use]
    pub const fn display(&self) -> DisplaySpec {
        self.display
    }
    #[must_use]
    pub const fn network(&self) -> NetworkSpec {
        self.network
    }
    #[must_use]
    pub fn shares(&self) -> &[SharedDirectory] {
        &self.shares
    }
    #[must_use]
    pub const fn platform(&self) -> &Platform {
        &self.platform
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_points_follow_density() {
        let retina = DisplaySpec::retina(1280, 800).unwrap().points();
        assert_eq!((retina.width(), retina.height()), (1280.0, 800.0));
        let plain = DisplaySpec::new(1920, 1080, 96).unwrap().points();
        assert_eq!((plain.width(), plain.height()), (1920.0, 1080.0));
    }

    struct Fixed(u8);
    impl Entropy for Fixed {
        type Error = std::convert::Infallible;
        fn fill(&mut self, bytes: &mut [u8]) -> Result<(), Self::Error> {
            bytes.fill(self.0);
            Ok(())
        }
    }

    #[test]
    fn a_readdressed_machine_keeps_its_identity_and_hardware() {
        let mac = |byte| MacAddress::generate(&mut Fixed(byte)).unwrap_or_else(|e| match e {});
        let installed = |network| {
            MachineSpec::new(
                Hardware {
                    cpus: CpuCount::new(4).unwrap(),
                    memory: MemorySize::from_gib(4).unwrap(),
                    disk: DiskSize::from_gib(64).unwrap(),
                    display: DisplaySpec::retina(1280, 800).unwrap(),
                    network,
                    shares: Vec::new(),
                },
                Platform::Installed {
                    hardware: HardwareModel::new(vec![1]).unwrap(),
                    identifier: MachineIdentifier::new(vec![2]).unwrap(),
                },
            )
            .unwrap()
        };
        let source = installed(NetworkSpec::Nat { mac: mac(0x02) });
        let fork = source.clone().with_mac(mac(0x0a));
        assert_eq!(fork.network(), NetworkSpec::Nat { mac: mac(0x0a) });
        assert_eq!(fork.platform(), source.platform());
        assert_eq!(
            (fork.cpus(), fork.memory(), fork.disk(), fork.display()),
            (
                source.cpus(),
                source.memory(),
                source.disk(),
                source.display()
            )
        );
        let offline = installed(NetworkSpec::None).with_mac(mac(0x0a));
        assert_eq!(offline.network(), NetworkSpec::None);
    }

    #[test]
    fn machine_names() {
        for ok in ["default", "a", "work-2", "0abc"] {
            assert!(ok.parse::<MachineName>().is_ok(), "{ok}");
        }
        for bad in ["", "-x", "Work", "a b", "a/b", "..", &"x".repeat(33)] {
            assert!(bad.parse::<MachineName>().is_err(), "{bad}");
        }
    }

    #[test]
    fn generated_mac_is_local_unicast_whatever_the_entropy() {
        for byte in [0x00, 0xff, 0x01, 0xaa] {
            let Ok(mac) = MacAddress::generate(&mut Fixed(byte));
            assert_eq!(mac.octets()[0] & 0b11, 0b10, "{mac}");
        }
    }

    #[test]
    fn mac_parse_display_round_trip() {
        let mac: MacAddress = "aa:d9:39:38:2f:8a".parse().unwrap();
        assert_eq!(mac.to_string(), "aa:d9:39:38:2f:8a");
        assert_eq!("AA:D9:39:38:2F:8A".parse::<MacAddress>().unwrap(), mac);
        for bad in [
            "",
            "aa:d9:39:38:2f",
            "aa:d9:39:38:2f:8a:00",
            "aad9:39:38:2f:8a",
            "zz:d9:39:38:2f:8a",
            "a:d9:39:38:2f:8a",
        ] {
            assert!(
                matches!(bad.parse::<MacAddress>(), Err(SpecError::InvalidMac(_))),
                "{bad}"
            );
        }
        assert!(matches!(
            "01:00:5e:00:00:01".parse::<MacAddress>(),
            Err(SpecError::MulticastMac(_))
        ));
    }

    #[test]
    fn memory_must_be_mib_aligned() {
        assert!(MemorySize::from_gib(4).is_ok());
        assert_eq!(
            MemorySize::from_bytes(0),
            Err(SpecError::Zero { field: "memory" })
        );
        assert_eq!(
            MemorySize::from_bytes((1 << 20) + 1),
            Err(SpecError::MemoryNotMibAligned((1 << 20) + 1))
        );
    }

    #[test]
    fn platform_parts_are_both_or_neither() {
        let hw = || HardwareModel::new(vec![1]);
        let id = || MachineIdentifier::new(vec![2]);
        assert_eq!(Platform::from_parts(None, None), Ok(Platform::NotInstalled));
        assert!(matches!(
            Platform::from_parts(hw(), id()),
            Ok(Platform::Installed { .. })
        ));
        assert_eq!(
            Platform::from_parts(hw(), None),
            Err(SpecError::PartialPlatform)
        );
        assert_eq!(
            Platform::from_parts(None, id()),
            Err(SpecError::PartialPlatform)
        );
        assert!(HardwareModel::new(Vec::new()).is_none());
    }

    #[test]
    fn identity_is_fixed_once_installed() {
        let hardware = Hardware {
            cpus: CpuCount::new(4).unwrap(),
            memory: MemorySize::from_gib(4).unwrap(),
            disk: DiskSize::from_gib(64).unwrap(),
            display: DisplaySpec::retina(1280, 800).unwrap(),
            network: NetworkSpec::None,
            shares: vec![],
        };
        let spec = MachineSpec::new(hardware, Platform::NotInstalled).unwrap();
        let installed = spec
            .installed(
                HardwareModel::new(vec![1]).unwrap(),
                MachineIdentifier::new(vec![2]).unwrap(),
            )
            .unwrap();
        assert_eq!(
            installed.installed(
                HardwareModel::new(vec![3]).unwrap(),
                MachineIdentifier::new(vec![4]).unwrap()
            ),
            Err(SpecError::AlreadyInstalled)
        );
    }

    #[test]
    fn share_names_unique_and_paths_absolute() {
        let share = |name: &str, path: &str| {
            SharedDirectory::new(name.parse().unwrap(), path.into(), Access::ReadOnly)
        };
        assert!(matches!(
            share("tools", "relative/dir"),
            Err(SpecError::RelativePath(_))
        ));
        let hardware = Hardware {
            cpus: CpuCount::new(1).unwrap(),
            memory: MemorySize::from_gib(4).unwrap(),
            disk: DiskSize::from_gib(64).unwrap(),
            display: DisplaySpec::retina(1280, 800).unwrap(),
            network: NetworkSpec::None,
            shares: vec![share("tools", "/a").unwrap(), share("tools", "/b").unwrap()],
        };
        assert_eq!(
            MachineSpec::new(hardware, Platform::NotInstalled),
            Err(SpecError::DuplicateShareName("tools".into()))
        );
    }
}
