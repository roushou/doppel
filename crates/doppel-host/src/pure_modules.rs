//! The pure modules of this I/O crate stay pure: they compute plans and
//! parse text, and the shell modules beside them perform what they say.

/// Each pure module, outside its own tests.
const PURE: [(&str, &str); 8] = [
    ("guest_step/plan.rs", include_str!("guest_step/plan.rs")),
    ("agent_grant/plan.rs", include_str!("agent_grant/plan.rs")),
    ("golden/plan.rs", include_str!("golden/plan.rs")),
    (
        "agent_install/plan.rs",
        include_str!("agent_install/plan.rs"),
    ),
    ("bootstrap/plan.rs", include_str!("bootstrap/plan.rs")),
    ("provision.rs", include_str!("provision.rs")),
    ("dhcp_leases.rs", include_str!("dhcp_leases.rs")),
    (
        "restore_image/plan.rs",
        include_str!("restore_image/plan.rs"),
    ),
];

/// The only modules of this crate a pure module may use.
const PURE_PATHS: [&str; 8] = [
    "crate::guest_step::plan",
    "crate::agent_grant::plan",
    "crate::golden::plan",
    "crate::agent_install::plan",
    "crate::bootstrap::plan",
    "crate::provision",
    "crate::dhcp_leases",
    "crate::restore_image::plan",
];

const FORBIDDEN: [&str; 11] = [
    "std::fs",
    "std::io",
    "std::os",
    "std::process",
    "std::thread",
    "std::sync",
    "std::net::Tcp",
    "std::net::Udp",
    "Instant::now",
    // A time handed in is a value; reading the clock is an effect.
    "SystemTime::now",
    "doppel_protocol",
];

fn code(source: &str) -> &str {
    source.split("#[cfg(test)]").next().unwrap_or(source)
}

#[test]
fn pure_modules_do_no_io() {
    for (file, source) in PURE {
        for forbidden in FORBIDDEN {
            assert!(!code(source).contains(forbidden), "{file} uses {forbidden}");
        }
    }
}

#[test]
fn pure_modules_use_only_pure_modules_of_this_crate() {
    for (file, source) in PURE {
        for (at, _) in code(source).match_indices("crate::") {
            let used = &code(source)[at..];
            assert!(
                PURE_PATHS.iter().any(|pure| used.starts_with(pure)),
                "{file} uses {}",
                used.lines().next().unwrap_or(used)
            );
        }
    }
}
