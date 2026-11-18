//! What granting the guest agent its permissions writes into a stopped
//! machine's Data volume, so the guest boots with them and no person
//! answers a prompt.
//!
//! Two records hold them, both Apple's own and undocumented, so each is
//! checked before it's written and an unknown shape is refused:
//!
//! - **TCC**: the system privacy database's `access` table gets a row per
//!   service (Accessibility, Screen Recording) for the agent's bundle id,
//!   allowed, held to the agent's code requirement, as System Settings
//!   writes it.
//! - **Screen capture approvals**: replayd's record of when the user last
//!   saw the "bypass the system private window picker" alert. Without an
//!   entry the alert comes up once the agent captures; with a hint date far
//!   ahead it never does.
//!
//! The shell attaches the disk image, runs `sqlite3` with [`TccSchema`]'s
//! and [`TccGrant`]'s SQL, and reads and writes the approvals plist.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::agent_install::plan::LABEL;
use crate::golden::plan::GuestUserName;

/// The system TCC database, from the Data volume's root.
pub const TCC_DB: &str = "Library/Application Support/com.apple.TCC/TCC.db";

/// The services the agent needs.
const SERVICES: [&str; 2] = ["kTCCServiceAccessibility", "kTCCServiceScreenCapture"];

/// Columns a grant row sets: the rest keep their defaults.
const WRITTEN: [&str; 10] = [
    "service",
    "client",
    "client_type",
    "auth_value",
    "auth_reason",
    "auth_version",
    "csreq",
    "flags",
    "last_modified",
    "last_reminded",
];

/// How long until replayd would remind the user of the agent's capture:
/// far beyond any machine's life.
const NO_REMINDER: Duration = Duration::from_secs(100 * 365 * 24 * 60 * 60);
/// The reminder interval replayd itself records (30 days).
const HINT_POLICY: i64 = 30 * 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GrantPlanError {
    #[error("not a code requirement (a csreq blob starts with fade0c00)")]
    Requirement,
    #[error(
        "the guest's TCC database has a layout this Doppel doesn't know ({0}); nothing was written"
    )]
    Schema(String),
    #[error("the guest's screen capture approvals aren't a dictionary; nothing was written")]
    Approvals,
    #[error("the disk image attached without one Data volume ({0})")]
    NoDataVolume(String),
    #[error("diskutil's answer couldn't be read: {0}")]
    Attach(String),
    #[error("{0} is before 1970")]
    Time(String),
}

/// A code requirement in its binary form (`csreq -b`): what TCC holds the
/// agent's signature to. Its bytes are only checked to be one.
#[derive(Clone, PartialEq, Eq)]
pub struct CodeRequirement(Vec<u8>);

impl CodeRequirement {
    /// # Errors
    /// [`GrantPlanError::Requirement`] unless it starts with the magic of a
    /// single requirement.
    pub fn parse(bytes: Vec<u8>) -> Result<Self, GrantPlanError> {
        if bytes.starts_with(&[0xfa, 0xde, 0x0c, 0x00]) {
            Ok(Self(bytes))
        } else {
            Err(GrantPlanError::Requirement)
        }
    }

    fn hex(&self) -> String {
        use std::fmt::Write as _;
        self.0.iter().fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
    }
}

impl std::fmt::Debug for CodeRequirement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CodeRequirement({} bytes)", self.0.len())
    }
}

/// The `access` table's layout as `sqlite3` lists it, known to take a grant
/// row: every column a row sets exists, and every column a row leaves out
/// may be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TccSchema(());

impl TccSchema {
    /// What `sqlite3 -list -noheader` prints for this, one column a line:
    /// `name|notnull|has a default`.
    pub const QUERY: &str =
        "select name, \"notnull\", dflt_value is not null from pragma_table_info('access');";

    /// # Errors
    /// [`GrantPlanError::Schema`], saying what doesn't fit.
    pub fn parse(listing: &str) -> Result<Self, GrantPlanError> {
        let mut columns = BTreeSet::new();
        let mut required = Vec::new();
        for line in listing.lines().filter(|line| !line.is_empty()) {
            let fields: Vec<&str> = line.split('|').collect();
            let [name, not_null, has_default] = fields[..] else {
                return Err(GrantPlanError::Schema(format!(
                    "unreadable column {line:?}"
                )));
            };
            columns.insert(name);
            if not_null == "1" && has_default == "0" {
                required.push(name);
            }
        }
        if columns.is_empty() {
            return Err(GrantPlanError::Schema("no access table".to_owned()));
        }
        let missing: Vec<_> = WRITTEN.iter().filter(|c| !columns.contains(*c)).collect();
        if !missing.is_empty() {
            return Err(GrantPlanError::Schema(format!("no column {missing:?}")));
        }
        let unset: Vec<_> = required.iter().filter(|c| !WRITTEN.contains(c)).collect();
        if !unset.is_empty() {
            return Err(GrantPlanError::Schema(format!("also requires {unset:?}")));
        }
        Ok(Self(()))
    }
}

/// The rows that grant the agent its services, replacing any the guest
/// wrote (a denial after a prompt, an older grant).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TccGrant {
    sql: String,
}

impl TccGrant {
    /// Only for a database whose layout was checked.
    ///
    /// # Errors
    /// [`GrantPlanError::Time`] for a time before 1970.
    pub fn new(
        _checked: &TccSchema,
        requirement: &CodeRequirement,
        at: SystemTime,
    ) -> Result<Self, GrantPlanError> {
        let at = unix_seconds(at)?;
        let csreq = requirement.hex();
        let mut sql = String::from("BEGIN;\n");
        for service in SERVICES {
            // client_type 0: a bundle id. auth_value 2: allowed.
            // auth_reason 4: set in System Settings. auth_version 1.
            sql.push_str(&format!(
                "INSERT OR REPLACE INTO access ({}) VALUES ('{service}', '{LABEL}', 0, 2, 4, 1, X'{csreq}', 0, {at}, {at});\n",
                WRITTEN.join(", ")
            ));
        }
        sql.push_str("COMMIT;\n");
        Ok(Self { sql })
    }

    /// For `sqlite3`'s standard input.
    #[must_use]
    pub fn sql(&self) -> &str {
        &self.sql
    }
}

/// replayd's approvals of the agent's screen capture, for one guest user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureApprovals {
    user: GuestUserName,
}

impl CaptureApprovals {
    #[must_use]
    pub const fn new(user: GuestUserName) -> Self {
        Self { user }
    }

    /// The user's home, from the Data volume's root.
    #[must_use]
    pub fn home(&self) -> PathBuf {
        PathBuf::from("Users").join(self.user.as_str())
    }

    /// The directory of the plist, from the Data volume's root.
    #[must_use]
    pub fn dir(&self) -> PathBuf {
        self.home()
            .join("Library/Group Containers/group.com.apple.replayd")
    }

    /// The plist, from the Data volume's root.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.dir().join("ScreenCaptureApprovals.plist")
    }

    /// The agent app as replayd names it: where `agent install` puts it.
    fn app_url(&self) -> String {
        format!(
            "file:///Users/{}/Applications/Doppel%20Agent.app/",
            self.user.as_str()
        )
    }

    /// The approvals with the agent's set: seen and used `at`, and never to
    /// be reminded. Every other app's entry stays.
    ///
    /// # Errors
    /// [`GrantPlanError::Approvals`] when what's there isn't a dictionary.
    pub fn merged(
        &self,
        existing: Option<plist::Value>,
        at: SystemTime,
    ) -> Result<plist::Value, GrantPlanError> {
        let mut approvals = match existing {
            None => plist::Dictionary::new(),
            Some(plist::Value::Dictionary(approvals)) => approvals,
            Some(_) => return Err(GrantPlanError::Approvals),
        };
        let date = |time: SystemTime| plist::Value::Date(time.into());
        let mut entry = plist::Dictionary::new();
        entry.insert("kScreenCaptureAlertableUsageCount".into(), 1.into());
        entry.insert("kScreenCaptureApprovalLastAlerted".into(), date(at));
        entry.insert("kScreenCaptureApprovalLastUsed".into(), date(at));
        entry.insert(
            "kScreenCapturePrivacyHintDate".into(),
            date(at + NO_REMINDER),
        );
        entry.insert("kScreenCapturePrivacyHintPolicy".into(), HINT_POLICY.into());
        approvals.insert(self.app_url(), plist::Value::Dictionary(entry));
        Ok(plist::Value::Dictionary(approvals))
    }
}

/// An attached disk image, as `diskutil image attach --plist` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedImage {
    /// The whole disk: what is ejected.
    pub disk: String,
    /// The macOS Data volume: what is mounted.
    pub data: String,
}

#[derive(serde::Deserialize)]
struct Attached {
    #[serde(rename = "system-entities")]
    entities: Vec<Entity>,
}

#[derive(serde::Deserialize)]
struct Entity {
    #[serde(rename = "dev-entry")]
    dev: String,
    #[serde(rename = "content-hint")]
    hint: Option<String>,
    role: Option<String>,
}

impl AttachedImage {
    /// # Errors
    /// [`GrantPlanError::Attach`] for an answer that isn't one,
    /// [`GrantPlanError::NoDataVolume`] unless exactly one volume is the
    /// Data volume.
    pub fn parse(plist: &[u8]) -> Result<Self, GrantPlanError> {
        let attached: Attached =
            plist::from_bytes(plist).map_err(|error| GrantPlanError::Attach(error.to_string()))?;
        let disk = attached
            .entities
            .iter()
            .find(|entity| entity.hint.as_deref() == Some("GUID_partition_scheme"))
            .map(|entity| entity.dev.clone())
            .ok_or_else(|| GrantPlanError::Attach("no partitioned disk".to_owned()))?;
        let data: Vec<_> = attached
            .entities
            .iter()
            .filter(|entity| entity.role.as_deref() == Some("Data"))
            .map(|entity| entity.dev.clone())
            .collect();
        match &data[..] {
            [data] => Ok(Self {
                disk,
                data: data.clone(),
            }),
            [] => Err(GrantPlanError::NoDataVolume("none".to_owned())),
            several => Err(GrantPlanError::NoDataVolume(several.join(", "))),
        }
    }
}

fn unix_seconds(at: SystemTime) -> Result<u64, GrantPlanError> {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .map_err(|_| GrantPlanError::Time(format!("{at:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As macOS 27 lists its `access` table.
    const MACOS_27: &str = "service|1|0\nclient|1|0\nclient_type|1|0\nauth_value|1|0\nauth_reason|1|0\nauth_version|1|0\ncsreq|0|0\npolicy_id|0|0\nindirect_object_identifier_type|0|0\nindirect_object_identifier|1|1\nindirect_object_code_identity|0|0\nflags|0|0\nlast_modified|1|1\npid|0|0\npid_version|0|0\nboot_uuid|1|1\nlast_reminded|1|1\none_time_reprompt_eligible|0|0\nreminder_count|1|1\n";

    fn requirement() -> CodeRequirement {
        CodeRequirement::parse(vec![0xfa, 0xde, 0x0c, 0x00, 0x01]).unwrap()
    }

    fn at() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    fn user() -> GuestUserName {
        "doppel".parse().unwrap()
    }

    #[test]
    fn a_requirement_is_a_csreq_blob() {
        assert!(CodeRequirement::parse(b"identifier x".to_vec()).is_err());
        assert_eq!(requirement().hex(), "fade0c0001");
    }

    #[test]
    fn macos_27_s_access_table_takes_a_grant() {
        assert!(TccSchema::parse(MACOS_27).is_ok());
    }

    #[test]
    fn an_unknown_layout_is_refused() {
        // A new column every row must set.
        let more = format!("{MACOS_27}consent_scope|1|0\n");
        assert!(matches!(
            TccSchema::parse(&more),
            Err(GrantPlanError::Schema(why)) if why.contains("consent_scope")
        ));
        // A column a grant sets, gone.
        let less = MACOS_27.replace("csreq|0|0\n", "");
        assert!(TccSchema::parse(&less).is_err());
        assert!(TccSchema::parse("").is_err());
        assert!(TccSchema::parse("service|1").is_err());
    }

    #[test]
    fn the_grant_replaces_both_services_rows_in_one_transaction() {
        let schema = TccSchema::parse(MACOS_27).unwrap();
        let sql = TccGrant::new(&schema, &requirement(), at()).unwrap();
        let sql = sql.sql();
        assert!(sql.starts_with("BEGIN;\n") && sql.ends_with("COMMIT;\n"));
        for service in SERVICES {
            assert!(
                sql.contains(&format!(
                    "VALUES ('{service}', 'dev.doppel.agent', 0, 2, 4, 1, X'fade0c0001', 0, 1800000000, 1800000000);"
                )),
                "{sql}"
            );
        }
        assert_eq!(sql.matches("INSERT OR REPLACE INTO access").count(), 2);
    }

    #[test]
    fn approvals_keep_other_apps_and_never_remind_of_the_agent() {
        let approvals = CaptureApprovals::new(user());
        assert_eq!(
            approvals.path(),
            PathBuf::from(
                "Users/doppel/Library/Group Containers/group.com.apple.replayd/ScreenCaptureApprovals.plist"
            )
        );
        let mut other = plist::Dictionary::new();
        other.insert("file:///Applications/Other.app/".into(), 7.into());
        let merged = approvals
            .merged(Some(plist::Value::Dictionary(other)), at())
            .unwrap();
        let merged = merged.as_dictionary().unwrap();
        assert!(merged.contains_key("file:///Applications/Other.app/"));
        let agent = merged
            .get("file:///Users/doppel/Applications/Doppel%20Agent.app/")
            .and_then(plist::Value::as_dictionary)
            .unwrap();
        let hint = agent
            .get("kScreenCapturePrivacyHintDate")
            .and_then(plist::Value::as_date)
            .unwrap();
        assert!(SystemTime::from(hint) > at() + Duration::from_secs(50 * 365 * 24 * 3600));
    }

    #[test]
    fn approvals_that_are_not_a_dictionary_are_left_alone() {
        let approvals = CaptureApprovals::new(user());
        assert_eq!(
            approvals.merged(Some(plist::Value::Integer(1.into())), at()),
            Err(GrantPlanError::Approvals)
        );
        assert!(approvals.merged(None, at()).is_ok());
    }

    #[test]
    fn the_data_volume_and_its_disk_are_found_in_diskutil_s_answer() {
        let answer = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>system-entities</key><array>
<dict><key>content-hint</key><string>GUID_partition_scheme</string><key>dev-entry</key><string>disk6</string></dict>
<dict><key>content-hint</key><string>Apple_APFS_Volume</string><key>role</key><string>Preboot</string><key>dev-entry</key><string>disk7s1</string></dict>
<dict><key>content-hint</key><string>Apple_APFS_Volume</string><key>role</key><string>System</string><key>dev-entry</key><string>disk11s1</string></dict>
<dict><key>content-hint</key><string>Apple_APFS_Volume</string><key>role</key><string>Data</string><key>dev-entry</key><string>disk11s5</string></dict>
</array></dict></plist>"#;
        assert_eq!(
            AttachedImage::parse(answer).unwrap(),
            AttachedImage {
                disk: "disk6".to_owned(),
                data: "disk11s5".to_owned()
            }
        );
        let no_data = String::from_utf8_lossy(answer).replace(">Data<", ">VM<");
        assert!(matches!(
            AttachedImage::parse(no_data.as_bytes()),
            Err(GrantPlanError::NoDataVolume(_))
        ));
    }
}
