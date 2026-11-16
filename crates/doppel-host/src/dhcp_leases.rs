//! The host's DHCP leases (`/var/db/dhcpd_leases`, written by `bootpd` for
//! Virtualization's NAT network): which address a guest's MAC was given.
//!
//! The file is a list of brace-delimited blocks of `key=value` lines, each
//! indented with a tab:
//!
//! ```text
//! {
//!     name=Mac
//!     ip_address=192.168.64.7
//!     hw_address=1,4e:ef:30:c6:3:91
//!     identifier=1,4e:ef:30:c6:3:91
//!     lease=0x6af8aa4b
//! }
//! ```
//!
//! `hw_address` is `<ARP hardware type>,<address>`, type 1 being Ethernet,
//! with each byte's leading zero dropped (`03` is written `3`). `lease` is
//! the expiry, hex seconds since the epoch. Fields this parser doesn't use
//! are kept; a block it can't make a lease of is kept as [`Entry::Unusable`].

use std::net::Ipv4Addr;

use doppel_machine::machine::MacAddress;

/// The path `bootpd` writes.
pub const PATH: &str = "/var/db/dhcpd_leases";

/// Every block of a leases file, in file order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DhcpLeases {
    pub entries: Vec<Entry>,
    /// Non-blank lines outside any block.
    pub stray: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Lease(Lease),
    /// A block without a usable address or hardware address, or one the
    /// file ended inside (`bootpd` was rewriting it).
    Unusable {
        block: Block,
        problem: Problem,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    MissingIp,
    BadIp(String),
    MissingHardware,
    Unterminated,
}

/// One block's lines, as written.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Block {
    /// `key=value` lines, in order, trimmed.
    pub fields: Vec<(String, String)>,
    /// Lines without `=`.
    pub odd: Vec<String>,
}

impl Block {
    fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// A block with an address and a hardware address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub ip: Ipv4Addr,
    pub hardware: HardwareAddress,
    /// The expiry (seconds since the epoch), when present and readable.
    pub expires: Option<u64>,
    /// Every line of the block, the fields above included.
    pub block: Block,
}

/// A client's hardware address, as `hw_address` gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HardwareAddress {
    /// Type 1 with six bytes.
    Ethernet([u8; 6]),
    /// Any other type, or an address that isn't six hex bytes.
    Unknown(String),
}

impl HardwareAddress {
    fn parse(raw: &str) -> Self {
        let ethernet = raw.split_once(',').and_then(|(kind, address)| {
            (kind.trim() == "1").then_some(())?;
            let mut bytes = [0u8; 6];
            let mut parts = address.trim().split(':');
            for byte in &mut bytes {
                let part = parts.next()?;
                if !(1..=2).contains(&part.len()) {
                    return None;
                }
                *byte = u8::from_str_radix(part, 16).ok()?;
            }
            parts.next().is_none().then_some(bytes)
        });
        ethernet.map_or_else(|| Self::Unknown(raw.to_owned()), Self::Ethernet)
    }
}

impl Entry {
    fn from_block(block: Block) -> Self {
        let unusable = |block, problem| Self::Unusable { block, problem };
        let Some(ip) = block.get("ip_address") else {
            return unusable(block, Problem::MissingIp);
        };
        let ip = match ip.parse::<Ipv4Addr>() {
            Ok(ip) => ip,
            Err(_) => {
                let bad = ip.to_owned();
                return unusable(block, Problem::BadIp(bad));
            }
        };
        let Some(hardware) = block.get("hw_address").map(HardwareAddress::parse) else {
            return unusable(block, Problem::MissingHardware);
        };
        let expires = block
            .get("lease")
            .and_then(|raw| raw.strip_prefix("0x"))
            .and_then(|hex| u64::from_str_radix(hex, 16).ok());
        Self::Lease(Lease {
            ip,
            hardware,
            expires,
            block,
        })
    }
}

impl DhcpLeases {
    /// Never fails: anything it can't use is kept, not dropped.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut leases = Self::default();
        let mut open: Option<Block> = None;
        for line in text.lines().map(str::trim) {
            match (line, open.as_mut()) {
                ("{", None) => open = Some(Block::default()),
                ("{", Some(_)) => {
                    // A new block before the last closed: the last was cut short.
                    if let Some(block) = open.replace(Block::default()) {
                        leases.entries.push(Entry::Unusable {
                            block,
                            problem: Problem::Unterminated,
                        });
                    }
                }
                ("}", Some(_)) => {
                    if let Some(block) = open.take() {
                        leases.entries.push(Entry::from_block(block));
                    }
                }
                ("", _) => {}
                (other, None) => leases.stray.push(other.to_owned()),
                (other, Some(block)) => match other.split_once('=') {
                    Some((key, value)) => block
                        .fields
                        .push((key.trim().to_owned(), value.trim().to_owned())),
                    None => block.odd.push(other.to_owned()),
                },
            }
        }
        if let Some(block) = open {
            leases.entries.push(Entry::Unusable {
                block,
                problem: Problem::Unterminated,
            });
        }
        leases
    }

    /// The address leased to `mac`: of several leases, the one that expires
    /// last (the most recent); on a tie, or without expiries, the first.
    #[must_use]
    pub fn address_of(&self, mac: MacAddress) -> Option<Ipv4Addr> {
        self.entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Lease(lease) => Some(lease),
                Entry::Unusable { .. } => None,
            })
            .filter(|lease| lease.hardware == HardwareAddress::Ethernet(mac.octets()))
            .fold(None::<&Lease>, |best, lease| match best {
                Some(best) if best.expires >= lease.expires => Some(best),
                Some(_) | None => Some(lease),
            })
            .map(|lease| lease.ip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "{\n\tname=Mac\n\tip_address=192.168.64.7\n\thw_address=1,4e:ef:30:c6:3:91\n\tidentifier=1,4e:ef:30:c6:3:91\n\tlease=0x6af8aa4b\n}\n{\n\tname=other\n\tip_address=192.168.64.20\n\thw_address=1,56:a0:a6:2b:fb:6e\n\tlease=0x6af7ebc0\n}\n";

    fn mac(s: &str) -> MacAddress {
        s.parse().unwrap()
    }

    #[test]
    fn finds_a_mac_whose_bytes_lost_their_leading_zeros() {
        let leases = DhcpLeases::parse(FILE);
        assert_eq!(
            leases.address_of(mac("4e:ef:30:c6:03:91")),
            Some(Ipv4Addr::new(192, 168, 64, 7))
        );
        assert_eq!(
            leases.address_of(mac("56:a0:a6:2b:fb:6e")),
            Some(Ipv4Addr::new(192, 168, 64, 20))
        );
        assert_eq!(leases.address_of(mac("aa:d9:39:38:2f:8a")), None);
        assert!(leases.stray.is_empty());
    }

    #[test]
    fn keeps_fields_it_does_not_use() {
        let leases = DhcpLeases::parse(FILE);
        let Entry::Lease(lease) = &leases.entries[0] else {
            panic!("{:?}", leases.entries[0]);
        };
        assert_eq!(lease.expires, Some(0x6af8_aa4b));
        assert_eq!(lease.block.get("name"), Some("Mac"));
        assert_eq!(lease.block.get("identifier"), Some("1,4e:ef:30:c6:3:91"));
        assert_eq!(lease.block.fields.len(), 5);
    }

    #[test]
    fn the_latest_lease_wins() {
        let text = "{\nip_address=10.0.0.2\nhw_address=1,2:0:0:0:0:1\nlease=0x10\n}\n\
                    {\nip_address=10.0.0.3\nhw_address=1,2:0:0:0:0:1\nlease=0x20\n}\n\
                    {\nip_address=10.0.0.4\nhw_address=1,2:0:0:0:0:1\nlease=0x20\n}\n";
        assert_eq!(
            DhcpLeases::parse(text).address_of(mac("02:00:00:00:00:01")),
            Some(Ipv4Addr::new(10, 0, 0, 3))
        );
        // Without expiries, the first.
        let text = "{\nip_address=10.0.0.2\nhw_address=1,2:0:0:0:0:1\n}\n\
                    {\nip_address=10.0.0.3\nhw_address=1,2:0:0:0:0:1\n}\n";
        assert_eq!(
            DhcpLeases::parse(text).address_of(mac("02:00:00:00:00:01")),
            Some(Ipv4Addr::new(10, 0, 0, 2))
        );
    }

    #[test]
    fn odd_entries_are_kept_not_used() {
        let text = "garbage\n\
                    {\nhw_address=1,2:0:0:0:0:1\n}\n\
                    {\nip_address=not-an-ip\nhw_address=1,2:0:0:0:0:1\n}\n\
                    {\nip_address=10.0.0.9\n}\n\
                    {\nip_address=10.0.0.5\nhw_address=6,2:0:0:0:0:1\nstrange line\n}\n\
                    {\nip_address=10.0.0.6\nhw_address=1,2:0:0:0:0\n}\n\
                    {\nip_address=10.0.0.7\nhw_address=1,2:0:0:0:0:1\n{\n\
                    ip_address=10.0.0.8\nhw_address=1,2:0:0:0:0:1\n";
        let leases = DhcpLeases::parse(text);
        assert_eq!(leases.stray, ["garbage"]);
        let problems: Vec<_> = leases
            .entries
            .iter()
            .map(|entry| match entry {
                Entry::Lease(lease) => Err(lease.hardware.clone()),
                Entry::Unusable { problem, .. } => Ok(problem.clone()),
            })
            .collect();
        assert_eq!(
            problems,
            [
                Ok(Problem::MissingIp),
                Ok(Problem::BadIp("not-an-ip".into())),
                Ok(Problem::MissingHardware),
                Err(HardwareAddress::Unknown("6,2:0:0:0:0:1".into())),
                Err(HardwareAddress::Unknown("1,2:0:0:0:0".into())),
                Ok(Problem::Unterminated),
                Ok(Problem::Unterminated),
            ]
        );
        let Entry::Lease(other_type) = &leases.entries[3] else {
            panic!()
        };
        assert_eq!(other_type.block.odd, ["strange line"]);
        // Nothing usable for this MAC: an unknown type or a cut-off block never matches.
        assert_eq!(leases.address_of(mac("02:00:00:00:00:01")), None);
    }

    #[test]
    fn rejects_over_long_bytes() {
        assert_eq!(
            HardwareAddress::parse("1,002:0:0:0:0:1"),
            HardwareAddress::Unknown("1,002:0:0:0:0:1".into())
        );
        assert_eq!(
            HardwareAddress::parse("1,2:0:0:0:0:1"),
            HardwareAddress::Ethernet([2, 0, 0, 0, 0, 1])
        );
    }

    #[test]
    fn empty_input() {
        assert_eq!(DhcpLeases::parse(""), DhcpLeases::default());
    }
}
