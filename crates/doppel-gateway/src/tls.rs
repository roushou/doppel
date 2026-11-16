//! The listener's certificate: self-signed, made once and kept, so its
//! fingerprint stays the one a person checked in their browser.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use rustls::ServerConfig;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sha2::{Digest as _, Sha256};

/// A certificate's SHA-256, as browsers show it: `AB:CD:…`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint([u8; 32]);

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex: Vec<String> = self.0.iter().map(|byte| format!("{byte:02X}")).collect();
        f.write_str(&hex.join(":"))
    }
}

/// The listener's certificate and key, valid for at most 397 days: Apple's
/// platforms refuse a server certificate valid for longer than 825, even
/// one a person chose to trust.
pub struct Certificate {
    der: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
}

impl fmt::Debug for Certificate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Certificate")
            .field("fingerprint", &self.fingerprint())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("{what} {}: {source}", path.display())]
    Io {
        what: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{} isn't a certificate and key this doppel made: {reason}", dir.display())]
    Unreadable { dir: PathBuf, reason: String },
    #[error("making a certificate: {0}")]
    Make(#[from] rcgen::Error),
    #[error("setting up TLS: {0}")]
    Config(#[from] rustls::Error),
}

impl Certificate {
    const CERT: &'static str = "cert.pem";
    const KEY: &'static str = "key.pem";
    const VALID_DAYS: u64 = 397;
    /// Remade this long before it would expire.
    const RENEW_DAYS: u64 = 30;

    /// The certificate kept in `dir`, made for `names` (host names and
    /// addresses) if there is none or it's about to expire.
    ///
    /// # Errors
    /// See [`TlsError`].
    pub fn load_or_make(dir: &Path, names: &[String], now: SystemTime) -> Result<Self, TlsError> {
        let cert_path = dir.join(Self::CERT);
        match fs::metadata(&cert_path).and_then(|meta| meta.modified()) {
            Ok(made) if !Self::due(made, now) => Self::load(dir),
            Ok(_) => Self::make(dir, names, now),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Self::make(dir, names, now)
            }
            Err(source) => Err(TlsError::Io {
                what: "reading",
                path: cert_path,
                source,
            }),
        }
    }

    fn due(made: SystemTime, now: SystemTime) -> bool {
        let age = now.duration_since(made).unwrap_or(Duration::ZERO);
        age >= Duration::from_secs((Self::VALID_DAYS - Self::RENEW_DAYS) * 86_400)
    }

    fn load(dir: &Path) -> Result<Self, TlsError> {
        let unreadable = |reason: String| TlsError::Unreadable {
            dir: dir.to_path_buf(),
            reason,
        };
        let der = CertificateDer::from_pem_file(dir.join(Self::CERT))
            .map_err(|error| unreadable(error.to_string()))?;
        let key = PrivateKeyDer::from_pem_file(dir.join(Self::KEY))
            .map_err(|error| unreadable(error.to_string()))?;
        Ok(Self { der, key })
    }

    fn make(dir: &Path, names: &[String], now: SystemTime) -> Result<Self, TlsError> {
        let mut params = rcgen::CertificateParams::new(names.to_vec())?;
        let day = |at: SystemTime| {
            let (year, month, day) = civil(at);
            rcgen::date_time_ymd(year, month, day)
        };
        // A day early, for a clock that's a little behind.
        params.not_before = day(now - Duration::from_secs(86_400));
        params.not_after = day(now + Duration::from_secs(Self::VALID_DAYS * 86_400));
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "doppel");
        let key = rcgen::KeyPair::generate()?;
        let cert = params.self_signed(&key)?;

        let io = |what, path: &Path| {
            let path = path.to_path_buf();
            move |source| TlsError::Io { what, path, source }
        };
        fs::create_dir_all(dir).map_err(io("creating", dir))?;
        // The key first, readable by this user alone: a certificate without
        // its key is never left behind.
        let key_path = dir.join(Self::KEY);
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&key_path)
            .map_err(io("writing", &key_path))?;
        file.write_all(key.serialize_pem().as_bytes())
            .map_err(io("writing", &key_path))?;
        let cert_path = dir.join(Self::CERT);
        fs::write(&cert_path, cert.pem()).map_err(io("writing", &cert_path))?;
        Self::load(dir)
    }

    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint(Sha256::digest(self.der.as_ref()).into())
    }

    /// The certificate itself, as a client that trusts it would load it.
    #[must_use]
    pub fn der(&self) -> &CertificateDer<'static> {
        &self.der
    }

    pub(crate) fn server_config(&self) -> Result<Arc<ServerConfig>, TlsError> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(vec![self.der.clone()], self.key.clone_key())?;
        Ok(Arc::new(config))
    }
}

/// The UTC date of `at`: Howard Hinnant's `civil_from_days`.
fn civil(at: SystemTime) -> (i32, u8, u8) {
    let days = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() / 86_400);
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    (
        i32::try_from(year).unwrap_or(i32::MAX),
        u8::try_from(month).unwrap_or(12),
        u8::try_from(day).unwrap_or(28),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_utc_calendar_days() {
        let at = |seconds| SystemTime::UNIX_EPOCH + Duration::from_secs(seconds);
        assert_eq!(civil(at(0)), (1970, 1, 1));
        assert_eq!(civil(at(951_782_400)), (2000, 2, 29));
        assert_eq!(civil(at(1_794_603_845)), (2026, 11, 13));
    }

    #[test]
    fn a_certificate_is_remade_a_month_before_it_expires() {
        let made = SystemTime::UNIX_EPOCH + Duration::from_secs(1_794_603_845);
        let days = |n: u64| made + Duration::from_secs(n * 86_400);
        assert!(!Certificate::due(made, days(366)));
        assert!(Certificate::due(made, days(367)));
    }
}
