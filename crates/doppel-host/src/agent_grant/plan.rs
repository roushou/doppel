//! What granting the guest agent its permissions writes into a stopped
//! machine's Data volume, so the guest boots with them and no person
//! answers a prompt.
//!
//! Three records hold them, all Apple's own and undocumented, so each is
//! checked before any is written, and an unknown shape is refused:
//!
//! - **System TCC**: the system privacy database's `access` table gets a
//!   row per service the agent works through (Accessibility, Screen
//!   Recording).
//! - **User TCC**: the guest user's privacy database gets a row per folder
//!   an agent works in (Downloads, Desktop, Documents) and per app it
//!   scripts (Finder, System Events), so the agent's first `ls ~/Downloads`
//!   or AppleScript to Finder doesn't stop at a prompt.
//! - **Screen capture approvals**: replayd's record of when the user last
//!   saw the "bypass the system private window picker" alert. Without an
//!   entry the alert comes up once the agent captures; with a hint date far
//!   ahead it never does.
//!
//! Every TCC row is for the agent's bundle id, allowed, held to the agent's
//! code requirement, as System Settings writes it.
//!
//! An [`AgentGrant`] is the request. Given what the guest has (its
//! [`TccSchema`]s and its approvals), [`AgentGrant::check`] gives the
//! [`CheckedGrant`]: the only thing the shell writes.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::agent_install::plan::LABEL;
use doppel_desktop::guest_user::GuestUserName;

/// A TCC database, from the directory it belongs to: the Data volume's
/// root for the system's; the user's home, or tccd's data container on
/// macOS 27, for a user's.
pub const TCC_DB: &str = "Library/Application Support/com.apple.TCC/TCC.db";

/// Where macOS 27 keeps system daemons' data containers, from the Data
/// volume's root: tccd's holds the user's TCC database under `Data/`.
pub const PROTECTED_CONTAINERS: &str = "private/var/containers/Data/ProtectedSystem";

/// The file in each container that says whose it is.
pub const CONTAINER_METADATA: &str = ".com.apple.containermanagerd.metadata.plist";

/// What the agent is allowed in the system database.
const SYSTEM: [Allowance; 2] = [
    Allowance::Service("kTCCServiceAccessibility"),
    Allowance::Service("kTCCServiceScreenCapture"),
];

/// What the agent is allowed in the user's database.
const USER: [Allowance; 5] = [
    Allowance::Service("kTCCServiceSystemPolicyDownloadsFolder"),
    Allowance::Service("kTCCServiceSystemPolicyDesktopFolder"),
    Allowance::Service("kTCCServiceSystemPolicyDocumentsFolder"),
    Allowance::AppleEvents(ScriptedApp {
        bundle_id: "com.apple.finder",
        // identifier "com.apple.finder" and anchor apple
        requirement: &[
            0xfa, 0xde, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
            0x00, 0x06, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x10, b'c', b'o', b'm', b'.',
            b'a', b'p', b'p', b'l', b'e', b'.', b'f', b'i', b'n', b'd', b'e', b'r', 0x00, 0x00,
            0x00, 0x03,
        ],
    }),
    Allowance::AppleEvents(ScriptedApp {
        bundle_id: "com.apple.systemevents",
        // identifier "com.apple.systemevents" and anchor apple
        requirement: &[
            0xfa, 0xde, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x34, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
            0x00, 0x06, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x16, b'c', b'o', b'm', b'.',
            b'a', b'p', b'p', b'l', b'e', b'.', b's', b'y', b's', b't', b'e', b'm', b'e', b'v',
            b'e', b'n', b't', b's', 0x00, 0x00, 0x00, 0x00, 0x00, 0x03,
        ],
    }),
];

/// One thing the agent is allowed: a row of a TCC database's `access`
/// table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Allowance {
    /// A service with no object: a permission, or a folder.
    Service(&'static str),
    /// Sending Apple events to an app (Automation).
    AppleEvents(ScriptedApp),
}

/// An app the agent may script, held to its code requirement (`csreq -b`
/// of its designated requirement), so only Apple's own app answers to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScriptedApp {
    bundle_id: &'static str,
    requirement: &'static [u8],
}

impl Allowance {
    /// Its service, then the object columns: the type, the identifier, and
    /// the code identity. A plain service has none, as tccd writes it.
    fn columns(self) -> (&'static str, String) {
        match self {
            Self::Service(service) => (service, "NULL, 'UNUSED', NULL".to_owned()),
            // Type 0: a bundle id.
            Self::AppleEvents(app) => (
                "kTCCServiceAppleEvents",
                format!("0, '{}', X'{}'", app.bundle_id, hex(app.requirement)),
            ),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

/// Columns a grant row sets: the rest keep their defaults.
const WRITTEN: [&str; 13] = [
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
    "indirect_object_identifier_type",
    "indirect_object_identifier",
    "indirect_object_code_identity",
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
    #[error(
        "the guest user has no TCC database yet (log in to the machine once, then grant again); nothing was written"
    )]
    NoUserTcc,
    #[error(
        "the guest has several users' TCC databases ({0}) and which is this user's isn't known; nothing was written"
    )]
    SeveralUserTcc(String),
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
        hex(&self.0)
    }
}

impl std::fmt::Debug for CodeRequirement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CodeRequirement({} bytes)", self.0.len())
    }
}

/// Proof that a TCC database's `access` table takes a grant row: every
/// column a row sets exists, and every column a row leaves out may be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TccSchema(());

impl TccSchema {
    /// What `sqlite3 -list -noheader` prints for [`Self::parse`], one
    /// column a line: `name|notnull|has a default`.
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

/// Granting the agent, run as `user`, its services from `at`, held to
/// `requirement`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentGrant {
    user: GuestUserName,
    requirement: CodeRequirement,
    at: SystemTime,
}

impl AgentGrant {
    #[must_use]
    pub const fn new(user: GuestUserName, requirement: CodeRequirement, at: SystemTime) -> Self {
        Self {
            user,
            requirement,
            at,
        }
    }

    #[must_use]
    pub const fn user(&self) -> &GuestUserName {
        &self.user
    }

    /// The user's home, from the Data volume's root.
    #[must_use]
    pub fn home(&self) -> PathBuf {
        PathBuf::from("Users").join(self.user.as_str())
    }

    /// replayd's group container, from the Data volume's root.
    #[must_use]
    pub fn approvals_dir(&self) -> PathBuf {
        self.home()
            .join("Library/Group Containers/group.com.apple.replayd")
    }

    /// The approvals plist, from the Data volume's root.
    #[must_use]
    pub fn approvals_file(&self) -> PathBuf {
        self.approvals_dir().join("ScreenCaptureApprovals.plist")
    }

    /// What to write, given the system's and the user's TCC databases,
    /// each known to take it, and the approvals the guest has (if any).
    ///
    /// # Errors
    /// [`GrantPlanError::Approvals`] when the approvals aren't a
    /// dictionary, [`GrantPlanError::Time`] for a time before 1970.
    pub fn check(
        &self,
        _system: &TccSchema,
        _user: &TccSchema,
        approvals: Option<plist::Value>,
    ) -> Result<CheckedGrant, GrantPlanError> {
        Ok(CheckedGrant {
            system_sql: self.sql(&SYSTEM)?,
            user_sql: self.sql(&USER)?,
            approvals: self.approvals(approvals)?,
        })
    }

    /// The rows for `allowed` in one transaction, replacing any the guest
    /// wrote (a denial after a prompt, an older grant).
    fn sql(&self, allowed: &[Allowance]) -> Result<String, GrantPlanError> {
        let at = self
            .at
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| GrantPlanError::Time(format!("{:?}", self.at)))?
            .as_secs();
        let csreq = self.requirement.hex();
        let mut sql = String::from("BEGIN;\n");
        for allowance in allowed {
            let (service, object) = allowance.columns();
            // client_type 0: a bundle id. auth_value 2: allowed.
            // auth_reason 4: set in System Settings. auth_version 1.
            sql.push_str(&format!(
                "INSERT OR REPLACE INTO access ({}) VALUES ('{service}', '{LABEL}', 0, 2, 4, 1, X'{csreq}', 0, {at}, {at}, {object});\n",
                WRITTEN.join(", ")
            ));
        }
        sql.push_str("COMMIT;\n");
        Ok(sql)
    }

    /// The approvals with the agent's set: seen and used now, and never to
    /// be reminded. Every other app's entry stays.
    fn approvals(&self, existing: Option<plist::Value>) -> Result<plist::Value, GrantPlanError> {
        let mut approvals = match existing {
            None => plist::Dictionary::new(),
            Some(plist::Value::Dictionary(approvals)) => approvals,
            Some(_) => return Err(GrantPlanError::Approvals),
        };
        let date = |time: SystemTime| plist::Value::Date(time.into());
        let mut entry = plist::Dictionary::new();
        entry.insert("kScreenCaptureAlertableUsageCount".into(), 1.into());
        entry.insert("kScreenCaptureApprovalLastAlerted".into(), date(self.at));
        entry.insert("kScreenCaptureApprovalLastUsed".into(), date(self.at));
        entry.insert(
            "kScreenCapturePrivacyHintDate".into(),
            date(self.at + NO_REMINDER),
        );
        entry.insert("kScreenCapturePrivacyHintPolicy".into(), HINT_POLICY.into());
        // The app as replayd names it: where `agent install` puts it.
        let app = format!(
            "file:///Users/{}/Applications/Doppel%20Agent.app/",
            self.user.as_str()
        );
        approvals.insert(app, plist::Value::Dictionary(entry));
        Ok(plist::Value::Dictionary(approvals))
    }
}

/// A grant checked against what the guest has: every record's new
/// contents. Only [`AgentGrant::check`] makes one, so nothing is written
/// unless every record was known.
#[derive(Debug, Clone, PartialEq)]
#[must_use]
pub struct CheckedGrant {
    system_sql: String,
    user_sql: String,
    approvals: plist::Value,
}

impl CheckedGrant {
    /// For `sqlite3`'s standard input, on the system's TCC database.
    #[must_use]
    pub fn system_sql(&self) -> &str {
        &self.system_sql
    }

    /// For `sqlite3`'s standard input, on the user's TCC database.
    #[must_use]
    pub fn user_sql(&self) -> &str {
        &self.user_sql
    }

    /// The approvals plist's new contents.
    #[must_use]
    pub const fn approvals(&self) -> &plist::Value {
        &self.approvals
    }
}

/// Where the guest user's TCC database is, from the Data volume's root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserTcc(PathBuf);

impl UserTcc {
    /// Whether a container's metadata plist names it tccd's.
    #[must_use]
    pub fn is_tccd_container(metadata: &[u8]) -> bool {
        plist::from_bytes::<plist::Dictionary>(metadata).is_ok_and(|metadata| {
            metadata
                .get("MCMMetadataIdentifier")
                .and_then(plist::Value::as_string)
                == Some("com.apple.tccd")
        })
    }

    /// The database, given the protected containers that are tccd's (by
    /// directory name) and whether `grant`'s home has one of its own. macOS
    /// 27 keeps it in tccd's container; earlier releases, in the home.
    ///
    /// # Errors
    /// [`GrantPlanError::NoUserTcc`] when neither has it,
    /// [`GrantPlanError::SeveralUserTcc`] when tccd has more than one
    /// container (one per user: which is this user's isn't known).
    pub fn find(
        grant: &AgentGrant,
        tccd_containers: &[String],
        in_home: bool,
    ) -> Result<Self, GrantPlanError> {
        match tccd_containers {
            [container] => Ok(Self(
                PathBuf::from(PROTECTED_CONTAINERS)
                    .join(container)
                    .join("Data")
                    .join(TCC_DB),
            )),
            [] if in_home => Ok(Self(grant.home().join(TCC_DB))),
            [] => Err(GrantPlanError::NoUserTcc),
            several => Err(GrantPlanError::SeveralUserTcc(several.join(", "))),
        }
    }

    /// From the Data volume's root.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.0
    }
}

/// The devices of an attached disk image, as `diskutil image attach
/// --plist` describes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedDevices {
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

impl AttachedDevices {
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

#[cfg(test)]
mod tests {
    use std::path::Path;

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

    fn grant() -> AgentGrant {
        AgentGrant::new(user(), requirement(), at())
    }

    fn checked(approvals: Option<plist::Value>) -> Result<CheckedGrant, GrantPlanError> {
        let schema = TccSchema::parse(MACOS_27).unwrap();
        grant().check(&schema, &schema, approvals)
    }

    const ALLOWED: &str =
        "'dev.doppel.agent', 0, 2, 4, 1, X'fade0c0001', 0, 1800000000, 1800000000";

    #[test]
    fn the_system_database_gets_the_agent_s_two_services_in_one_transaction() {
        let checked = checked(None).unwrap();
        let sql = checked.system_sql();
        assert!(sql.starts_with("BEGIN;\n") && sql.ends_with("COMMIT;\n"));
        for service in ["kTCCServiceAccessibility", "kTCCServiceScreenCapture"] {
            assert!(
                sql.contains(&format!(
                    "VALUES ('{service}', {ALLOWED}, NULL, 'UNUSED', NULL);"
                )),
                "{sql}"
            );
        }
        assert_eq!(sql.matches("INSERT OR REPLACE INTO access").count(), 2);
    }

    #[test]
    fn the_user_database_gets_the_folders_and_the_scripted_apps() {
        let checked = checked(None).unwrap();
        let sql = checked.user_sql();
        assert!(sql.starts_with("BEGIN;\n") && sql.ends_with("COMMIT;\n"));
        for folder in ["Downloads", "Desktop", "Documents"] {
            assert!(
                sql.contains(&format!(
                    "VALUES ('kTCCServiceSystemPolicy{folder}Folder', {ALLOWED}, NULL, 'UNUSED', NULL);"
                )),
                "{sql}"
            );
        }
        // As `csreq -b` compiles each app's designated requirement.
        for (app, requirement) in [
            (
                "com.apple.finder",
                "fade0c000000002c00000001000000060000000200000010636f6d2e6170706c652e66696e64657200000003",
            ),
            (
                "com.apple.systemevents",
                "fade0c000000003400000001000000060000000200000016636f6d2e6170706c652e73797374656d6576656e7473000000000003",
            ),
        ] {
            assert!(
                sql.contains(&format!(
                    "VALUES ('kTCCServiceAppleEvents', {ALLOWED}, 0, '{app}', X'{requirement}');"
                )),
                "{sql}"
            );
        }
        assert_eq!(sql.matches("INSERT OR REPLACE INTO access").count(), 5);
        assert!(!sql.contains("kTCCServiceAccessibility"));
    }

    #[test]
    fn the_user_database_is_tccd_s_container_or_else_the_home_s() {
        let grant = grant();
        let container = UserTcc::find(&grant, &["ABC".to_owned()], true).unwrap();
        assert_eq!(
            container.path(),
            Path::new(
                "private/var/containers/Data/ProtectedSystem/ABC/Data/Library/Application Support/com.apple.TCC/TCC.db"
            )
        );
        let home = UserTcc::find(&grant, &[], true).unwrap();
        assert_eq!(
            home.path(),
            Path::new("Users/doppel/Library/Application Support/com.apple.TCC/TCC.db")
        );
        assert_eq!(
            UserTcc::find(&grant, &[], false),
            Err(GrantPlanError::NoUserTcc)
        );
        assert!(matches!(
            UserTcc::find(&grant, &["A".to_owned(), "B".to_owned()], false),
            Err(GrantPlanError::SeveralUserTcc(which)) if which == "A, B"
        ));
    }

    #[test]
    fn a_container_is_tccd_s_by_its_metadata() {
        let metadata = |identifier: &str| {
            let mut dict = plist::Dictionary::new();
            dict.insert("MCMMetadataIdentifier".into(), identifier.into());
            let mut bytes = Vec::new();
            plist::to_writer_binary(&mut bytes, &plist::Value::Dictionary(dict)).unwrap();
            bytes
        };
        assert!(UserTcc::is_tccd_container(&metadata("com.apple.tccd")));
        assert!(!UserTcc::is_tccd_container(&metadata(
            "com.apple.deviceconfigurationd"
        )));
        assert!(!UserTcc::is_tccd_container(b"not a plist"));
    }

    #[test]
    fn approvals_keep_other_apps_and_never_remind_of_the_agent() {
        assert_eq!(
            grant().approvals_file(),
            PathBuf::from(
                "Users/doppel/Library/Group Containers/group.com.apple.replayd/ScreenCaptureApprovals.plist"
            )
        );
        let mut other = plist::Dictionary::new();
        other.insert("file:///Applications/Other.app/".into(), 7.into());
        let checked = checked(Some(plist::Value::Dictionary(other))).unwrap();
        let merged = checked.approvals().as_dictionary().unwrap();
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
    fn approvals_that_are_not_a_dictionary_stop_the_whole_grant() {
        assert_eq!(
            checked(Some(plist::Value::Integer(1.into()))),
            Err(GrantPlanError::Approvals)
        );
    }

    #[test]
    fn a_grant_before_1970_is_refused() {
        let early = AgentGrant::new(
            user(),
            requirement(),
            SystemTime::UNIX_EPOCH - Duration::from_secs(1),
        );
        assert!(matches!(
            early.check(
                &TccSchema::parse(MACOS_27).unwrap(),
                &TccSchema::parse(MACOS_27).unwrap(),
                None
            ),
            Err(GrantPlanError::Time(_))
        ));
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
            AttachedDevices::parse(answer).unwrap(),
            AttachedDevices {
                disk: "disk6".to_owned(),
                data: "disk11s5".to_owned()
            }
        );
        let no_data = String::from_utf8_lossy(answer).replace(">Data<", ">VM<");
        assert!(matches!(
            AttachedDevices::parse(no_data.as_bytes()),
            Err(GrantPlanError::NoDataVolume(_))
        ));
    }
}
