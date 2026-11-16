//! Tokens: who may use the browser viewer.
//!
//! A token is 32 random bytes, shown to the person once when it's issued.
//! The book keeps only its SHA-256 digest, with when it was issued and when
//! it expires, so a book that leaks lets no one in. Pure: the caller passes
//! the time and reads and writes the book's bytes.

use std::fmt;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// A token as its holder presents it: `dpl_` then its 32 bytes in
/// unpadded base64url. Its `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct TokenSecret(String);

impl TokenSecret {
    const PREFIX: &'static str = "dpl_";
    /// How many random bytes a token holds.
    pub const BYTES: usize = 32;

    /// The token made of these random bytes.
    #[must_use]
    pub fn from_bytes(bytes: [u8; Self::BYTES]) -> Self {
        Self(format!("{}{}", Self::PREFIX, URL_SAFE_NO_PAD.encode(bytes)))
    }

    /// The token as its holder presents it: show it once, when issued.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// What the book keeps of it.
    #[must_use]
    pub fn digest(&self) -> TokenDigest {
        TokenDigest(Sha256::digest(self.0.as_bytes()).into())
    }
}

impl fmt::Debug for TokenSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TokenSecret(..)")
    }
}

impl FromStr for TokenSecret {
    type Err = TokenError;

    /// Accepts only the form [`Self::from_bytes`] makes, so one token has
    /// one spelling and one digest.
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let encoded = raw
            .strip_prefix(Self::PREFIX)
            .ok_or(TokenError::Malformed)?;
        let bytes: [u8; Self::BYTES] = URL_SAFE_NO_PAD
            .decode(encoded)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(TokenError::Malformed)?;
        let token = Self::from_bytes(bytes);
        if token.0 == raw {
            Ok(token)
        } else {
            Err(TokenError::Malformed)
        }
    }
}

/// A token's SHA-256: all the book keeps of it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TokenDigest([u8; 32]);

impl TokenDigest {
    /// # Errors
    /// The bytes aren't 32 long.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TokenError> {
        bytes.try_into().map(Self).map_err(|_| TokenError::Digest)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The id the token is known by: its digest's first four bytes.
    #[must_use]
    pub fn id(&self) -> TokenId {
        TokenId(hex(&self.0[..TokenId::BYTES]))
    }
}

impl fmt::Debug for TokenDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TokenDigest({})", self.id())
    }
}

impl fmt::Display for TokenDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex(&self.0))
    }
}

impl FromStr for TokenDigest {
    type Err = TokenError;

    /// 64 lowercase hex digits.
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let bytes = unhex(raw).ok_or(TokenError::Digest)?;
        Self::from_bytes(&bytes)
    }
}

/// How a token is named when listed or revoked: 8 lowercase hex digits,
/// the start of its digest. It tells nothing about the token itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TokenId(String);

impl TokenId {
    const BYTES: usize = 4;

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TokenId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for TokenId {
    type Err = TokenError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match unhex(raw) {
            Some(bytes) if bytes.len() == Self::BYTES => Ok(Self(raw.to_owned())),
            Some(_) | None => Err(TokenError::Id(raw.to_owned())),
        }
    }
}

/// What a person calls a token ("phone"), to tell tokens apart: 1 to 64
/// characters, none of them control characters, not blank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenLabel(String);

impl TokenLabel {
    const MAX_CHARS: usize = 64;

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TokenLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for TokenLabel {
    type Err = TokenError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let fits = !raw.trim().is_empty()
            && raw.chars().count() <= Self::MAX_CHARS
            && !raw.chars().any(char::is_control);
        if fits {
            Ok(Self(raw.to_owned()))
        } else {
            Err(TokenError::Label)
        }
    }
}

/// How long a token lasts: a whole number of seconds from a minute to 30
/// days.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ttl(Duration);

impl Ttl {
    pub const MIN: Duration = Duration::from_secs(60);
    pub const MAX: Duration = Duration::from_secs(30 * 86_400);
    /// A working day.
    pub const DEFAULT: Self = Self(Duration::from_secs(12 * 3_600));

    /// # Errors
    /// Shorter than a minute or longer than 30 days.
    pub fn from_secs(seconds: u64) -> Result<Self, TokenError> {
        let ttl = Duration::from_secs(seconds);
        if (Self::MIN..=Self::MAX).contains(&ttl) {
            Ok(Self(ttl))
        } else {
            Err(TokenError::Ttl)
        }
    }

    #[must_use]
    pub const fn as_duration(self) -> Duration {
        self.0
    }
}

impl FromStr for Ttl {
    type Err = TokenError;

    /// A number and a unit: `90m`, `12h`, `7d` (or `s`).
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let split = raw.len().saturating_sub(1);
        let (number, unit) = (raw.get(..split), raw.get(split..));
        let scale = match unit {
            Some("s") => 1,
            Some("m") => 60,
            Some("h") => 3_600,
            Some("d") => 86_400,
            Some(_) | None => return Err(TokenError::Ttl),
        };
        let number: u64 = number
            .filter(|digits| digits.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|digits| digits.parse().ok())
            .ok_or(TokenError::Ttl)?;
        Self::from_secs(number.checked_mul(scale).ok_or(TokenError::Ttl)?)
    }
}

/// An issued token, as the book knows it. It expires after it was issued,
/// both in whole seconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    id: TokenId,
    label: Option<TokenLabel>,
    issued: SystemTime,
    expires: SystemTime,
}

impl Token {
    #[must_use]
    pub const fn id(&self) -> &TokenId {
        &self.id
    }

    #[must_use]
    pub const fn label(&self) -> Option<&TokenLabel> {
        self.label.as_ref()
    }

    #[must_use]
    pub const fn issued(&self) -> SystemTime {
        self.issued
    }

    #[must_use]
    pub const fn expires(&self) -> SystemTime {
        self.expires
    }

    /// Whether it has expired at `now`.
    #[must_use]
    pub fn expired(&self, now: SystemTime) -> bool {
        now >= self.expires
    }
}

/// Why a presented token doesn't let its holder in.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Denied {
    /// Never issued, or revoked.
    #[error("not a token this daemon issued, or one since revoked")]
    Unknown,
    #[error("token {0} has expired")]
    Expired(TokenId),
}

/// A value that isn't a valid part of a token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    #[error("not a doppel token: one starts with `dpl_` and has 43 characters after it")]
    Malformed,
    #[error("a token's digest is 32 bytes (64 hex digits)")]
    Digest,
    #[error("{0:?} isn't a token id: one is 8 hex digits, as `doppel daemon tokens` lists")]
    Id(String),
    #[error("a label is 1 to 64 characters, without control characters")]
    Label,
    #[error("a token lasts from a minute to 30 days, written like `90m`, `12h` or `7d`")]
    Ttl,
}

/// Why the book can't change as asked.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChangeError {
    #[error("token {0} is already in the book")]
    Duplicate(TokenId),
    #[error("no token {0}: it was revoked, has expired, or was never issued")]
    Unknown(TokenId),
}

/// Why a book's bytes can't be read.
#[derive(Debug, thiserror::Error)]
pub enum BookError {
    #[error("not a token book: {0}")]
    Json(#[from] serde_json::Error),
    #[error("token book version {0} is newer than this doppel")]
    Version(u32),
    #[error("a token in the book is invalid: {0}")]
    Token(#[from] TokenError),
    #[error("a token in the book expires before it was issued")]
    Times,
    #[error("token {0} is in the book twice")]
    Duplicate(TokenId),
}

/// The tokens a daemon has issued and not revoked, by digest. Each digest,
/// and so each id, is in it once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TokenBook {
    entries: Vec<(TokenDigest, Token)>,
}

impl TokenBook {
    const VERSION: u32 = 1;

    /// Adds a token issued at `now`. Expired tokens are dropped first.
    ///
    /// # Errors
    /// A token with its id is in the book.
    pub fn add(
        &mut self,
        digest: TokenDigest,
        label: Option<TokenLabel>,
        ttl: Ttl,
        now: SystemTime,
    ) -> Result<Token, ChangeError> {
        self.entries.retain(|(_, token)| !token.expired(now));
        let id = digest.id();
        if self.entries.iter().any(|(_, token)| token.id == id) {
            return Err(ChangeError::Duplicate(id));
        }
        let issued = from_unix(unix(now));
        let token = Token {
            id,
            label,
            issued,
            expires: issued + ttl.as_duration(),
        };
        self.entries.push((digest, token.clone()));
        Ok(token)
    }

    /// The token a holder presents, if it lets them in at `now`.
    ///
    /// # Errors
    /// See [`Denied`].
    pub fn check(&self, secret: &TokenSecret, now: SystemTime) -> Result<&Token, Denied> {
        // Comparing digests, not secrets: how long it takes says nothing
        // about a secret.
        let digest = secret.digest();
        let (_, token) = self
            .entries
            .iter()
            .find(|(kept, _)| *kept == digest)
            .ok_or(Denied::Unknown)?;
        if token.expired(now) {
            Err(Denied::Expired(token.id.clone()))
        } else {
            Ok(token)
        }
    }

    /// Removes a token: it lets no one in from now on.
    ///
    /// # Errors
    /// No token has this id.
    pub fn revoke(&mut self, id: &TokenId) -> Result<Token, ChangeError> {
        let at = self
            .entries
            .iter()
            .position(|(_, token)| token.id == *id)
            .ok_or_else(|| ChangeError::Unknown(id.clone()))?;
        Ok(self.entries.remove(at).1)
    }

    /// The tokens that let their holders in at `now`, oldest first.
    pub fn live(&self, now: SystemTime) -> impl Iterator<Item = &Token> {
        self.entries
            .iter()
            .map(|(_, token)| token)
            .filter(move |token| !token.expired(now))
    }

    /// A book from its file's bytes.
    ///
    /// # Errors
    /// See [`BookError`].
    pub fn parse(bytes: &[u8]) -> Result<Self, BookError> {
        let file: BookFile = serde_json::from_slice(bytes)?;
        if file.version != Self::VERSION {
            return Err(BookError::Version(file.version));
        }
        let mut book = Self::default();
        for entry in file.tokens {
            let digest: TokenDigest = entry.sha256.parse()?;
            let token = Token {
                id: digest.id(),
                label: entry.label.as_deref().map(str::parse).transpose()?,
                issued: from_unix(entry.issued),
                expires: from_unix(entry.expires),
            };
            if token.expires <= token.issued {
                return Err(BookError::Times);
            }
            if book.entries.iter().any(|(_, kept)| kept.id == token.id) {
                return Err(BookError::Duplicate(token.id));
            }
            book.entries.push((digest, token));
        }
        Ok(book)
    }

    /// The book's file: JSON, as [`Self::parse`] reads it.
    #[must_use]
    pub fn to_json(&self) -> String {
        let file = BookFile {
            version: Self::VERSION,
            tokens: self
                .entries
                .iter()
                .map(|(digest, token)| BookEntry {
                    sha256: digest.to_string(),
                    label: token.label.as_ref().map(|label| label.0.clone()),
                    issued: unix(token.issued),
                    expires: unix(token.expires),
                })
                .collect(),
        };
        // Strings and numbers only: serializing can't fail.
        let mut json = serde_json::to_string_pretty(&file).unwrap_or_default();
        json.push('\n');
        json
    }
}

/// `tokens.json`, version 1.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BookFile {
    version: u32,
    tokens: Vec<BookEntry>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BookEntry {
    sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    /// Seconds since 1970, UTC.
    issued: u64,
    expires: u64,
}

fn unix(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn from_unix(seconds: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(seconds)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Lowercase hex only, so each value has one spelling.
fn unhex(raw: &str) -> Option<Vec<u8>> {
    if !raw.len().is_multiple_of(2) || !raw.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    (0..raw.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(raw.get(at..at + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_794_603_845;

    fn at(seconds: u64) -> SystemTime {
        from_unix(seconds)
    }

    fn secret(fill: u8) -> TokenSecret {
        TokenSecret::from_bytes([fill; 32])
    }

    #[test]
    fn a_token_reads_back_only_in_the_form_it_was_made() {
        let token = secret(7);
        assert_eq!(token.expose().len(), 4 + 43);
        assert_eq!(token.expose().parse::<TokenSecret>(), Ok(token.clone()));
        for wrong in [
            "",
            "dpl_",
            &token.expose()[4..],
            &token.expose()[..46],
            &format!("{}A", token.expose()),
            &token.expose().replace("dpl_", "DPL_"),
        ] {
            assert_eq!(
                wrong.parse::<TokenSecret>(),
                Err(TokenError::Malformed),
                "{wrong}"
            );
        }
        assert_eq!(format!("{token:?}"), "TokenSecret(..)");
    }

    #[test]
    fn an_issued_token_lets_its_holder_in_until_it_expires() {
        let mut book = TokenBook::default();
        let ttl = Ttl::from_secs(3_600).unwrap();
        let issued = book
            .add(
                secret(1).digest(),
                Some("phone".parse().unwrap()),
                ttl,
                at(T0),
            )
            .unwrap();
        assert_eq!(issued.expires(), at(T0 + 3_600));
        assert_eq!(book.check(&secret(1), at(T0 + 3_599)), Ok(&issued));
        assert_eq!(
            book.check(&secret(1), at(T0 + 3_600)),
            Err(Denied::Expired(issued.id().clone()))
        );
        assert_eq!(book.check(&secret(2), at(T0)), Err(Denied::Unknown));
    }

    #[test]
    fn a_revoked_token_lets_no_one_in() {
        let mut book = TokenBook::default();
        let token = book
            .add(secret(1).digest(), None, Ttl::DEFAULT, at(T0))
            .unwrap();
        book.add(secret(2).digest(), None, Ttl::DEFAULT, at(T0))
            .unwrap();
        assert_eq!(book.revoke(token.id()), Ok(token.clone()));
        assert_eq!(book.check(&secret(1), at(T0)), Err(Denied::Unknown));
        assert!(book.check(&secret(2), at(T0)).is_ok());
        assert_eq!(
            book.revoke(token.id()),
            Err(ChangeError::Unknown(token.id().clone()))
        );
    }

    #[test]
    fn a_token_is_in_the_book_once_and_expired_ones_leave_it() {
        let mut book = TokenBook::default();
        let minute = Ttl::from_secs(60).unwrap();
        let first = book.add(secret(1).digest(), None, minute, at(T0)).unwrap();
        assert_eq!(
            book.add(secret(1).digest(), None, minute, at(T0)),
            Err(ChangeError::Duplicate(first.id().clone()))
        );
        book.add(secret(2).digest(), None, Ttl::DEFAULT, at(T0))
            .unwrap();
        assert_eq!(book.live(at(T0 + 60)).count(), 1);
        // Adding drops the expired one, which may then be issued again.
        assert!(
            book.add(secret(1).digest(), None, minute, at(T0 + 60))
                .is_ok()
        );
        assert_eq!(book.live(at(T0 + 60)).count(), 2);
    }

    #[test]
    fn a_ttl_is_a_minute_to_30_days() {
        assert_eq!("90m".parse(), Ok(Ttl(Duration::from_secs(5_400))));
        assert_eq!("12h".parse(), Ok(Ttl::DEFAULT));
        assert_eq!("30d".parse::<Ttl>().map(Ttl::as_duration), Ok(Ttl::MAX));
        assert_eq!("60s".parse::<Ttl>().map(Ttl::as_duration), Ok(Ttl::MIN));
        for wrong in [
            "59s",
            "31d",
            "12",
            "h",
            "-1h",
            "+1h",
            "1.5h",
            "99999999999999999999d",
        ] {
            assert_eq!(wrong.parse::<Ttl>(), Err(TokenError::Ttl), "{wrong}");
        }
    }

    #[test]
    fn labels_ids_and_digests_have_one_spelling() {
        assert!("phone".parse::<TokenLabel>().is_ok());
        for wrong in ["", "  ", "a\nb", &"x".repeat(65)] {
            assert_eq!(
                wrong.parse::<TokenLabel>(),
                Err(TokenError::Label),
                "{wrong:?}"
            );
        }
        let digest = secret(3).digest();
        assert_eq!(digest.to_string().parse(), Ok(digest));
        assert!(
            digest
                .to_string()
                .to_uppercase()
                .parse::<TokenDigest>()
                .is_err()
        );
        assert_eq!(digest.id().as_str().parse(), Ok(digest.id()));
        for wrong in ["1234567", "123456789", "ABCDEF12", "abcdefgh"] {
            assert!(wrong.parse::<TokenId>().is_err(), "{wrong}");
        }
    }

    #[test]
    fn a_book_reads_back_what_it_wrote_and_refuses_what_it_didnt() {
        let mut book = TokenBook::default();
        book.add(
            secret(1).digest(),
            Some("phone".parse().unwrap()),
            Ttl::DEFAULT,
            at(T0),
        )
        .unwrap();
        book.add(
            secret(2).digest(),
            None,
            Ttl::from_secs(30 * 86_400).unwrap(),
            at(T0),
        )
        .unwrap();
        assert_eq!(TokenBook::parse(book.to_json().as_bytes()).unwrap(), book);

        let entry = |sha: &str, issued: u64, expires: u64| {
            format!(
                r#"{{"version":1,"tokens":[{{"sha256":"{sha}","issued":{issued},"expires":{expires}}}]}}"#
            )
        };
        let sha = secret(1).digest().to_string();
        assert!(matches!(
            TokenBook::parse(br#"{"version":2,"tokens":[]}"#),
            Err(BookError::Version(2))
        ));
        assert!(matches!(
            TokenBook::parse(entry(&sha, T0, T0).as_bytes()),
            Err(BookError::Times)
        ));
        assert!(matches!(
            TokenBook::parse(entry("abc", T0, T0 + 1).as_bytes()),
            Err(BookError::Token(TokenError::Digest))
        ));
        let twice = format!(
            r#"{{"version":1,"tokens":[{e},{e}]}}"#,
            e = format!(r#"{{"sha256":"{sha}","issued":{T0},"expires":{}}}"#, T0 + 1)
        );
        assert!(matches!(
            TokenBook::parse(twice.as_bytes()),
            Err(BookError::Duplicate(_))
        ));
        assert!(matches!(
            TokenBook::parse(br#"{"version":1,"tokens":[],"extra":1}"#),
            Err(BookError::Json(_))
        ));
    }
}
