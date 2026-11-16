//! Identifier newtypes: [`SessionId`], [`TaskId`], [`Seq`] and [`BlobRef`].

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use ulid::Ulid;

/// Error returned when a ULID-based identifier fails to parse.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid identifier {input:?}: {reason}")]
pub struct ParseIdError {
    /// The text that failed to parse.
    pub input: String,
    /// Why it failed.
    pub reason: String,
}

macro_rules! ulid_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Ulid);

        impl $name {
            /// Creates a new, time-ordered identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Ulid::generate())
            }

            /// Wraps an existing [`Ulid`].
            #[must_use]
            pub const fn from_ulid(ulid: Ulid) -> Self {
                Self(ulid)
            }

            /// Returns the underlying [`Ulid`].
            #[must_use]
            pub const fn as_ulid(&self) -> Ulid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl FromStr for $name {
            type Err = ParseIdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ulid::from_string(s).map(Self).map_err(|e| ParseIdError {
                    input: s.to_owned(),
                    reason: e.to_string(),
                })
            }
        }
    };
}

ulid_id!(
    /// Identifies one session (one conversation in one project).
    SessionId
);
ulid_id!(
    /// Identifies one task (a pausable unit of work inside a session).
    TaskId
);
ulid_id!(
    /// Identifies one pending approval request shown to the user.
    ApprovalId
);

/// Per-session, gapless event sequence number. The first event of a session is [`Seq::FIRST`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Seq(pub u64);

impl Seq {
    /// The sequence number of the first event in every session.
    pub const FIRST: Seq = Seq(1);

    /// Returns the next sequence number.
    ///
    /// # Panics
    /// Panics if the sequence would overflow `u64`, which cannot happen in practice
    /// (it would take billions of years at a million events per second).
    #[must_use]
    pub fn next(self) -> Seq {
        Seq(self
            .0
            .checked_add(1)
            .expect("event sequence cannot overflow u64"))
    }
}

impl fmt::Display for Seq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Error returned when a [`BlobRef`] fails to parse from hex.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParseBlobRefError {
    /// The input was not exactly 64 characters long.
    #[error("blob reference must be 64 hex characters, got {0}")]
    Length(usize),
    /// The input contained a character that is not a lowercase or uppercase hex digit.
    #[error("blob reference contains a non-hex character at position {0}")]
    NonHex(usize),
}

/// Content address of a blob: the 32-byte BLAKE3 hash of its bytes.
///
/// Serialised as a 64-character lowercase hex string. Events refer to files by
/// `BlobRef` and never embed file bytes (FR-IN-03).
///
/// # Examples
/// ```
/// use harness_types::BlobRef;
/// let r = BlobRef::from_bytes([0xab; 32]);
/// let text = r.to_string();
/// assert_eq!(text.len(), 64);
/// assert_eq!(text.parse::<BlobRef>().unwrap(), r);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlobRef([u8; 32]);

impl BlobRef {
    /// Wraps a raw 32-byte hash.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the raw 32-byte hash.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Returns the lowercase hex form (64 characters).
    #[must_use]
    pub fn to_hex(&self) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            out.push(char::from(DIGITS[usize::from(byte >> 4)]));
            out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
        }
        out
    }
}

impl fmt::Debug for BlobRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BlobRef({})", self.to_hex())
    }
}

impl fmt::Display for BlobRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl FromStr for BlobRef {
    type Err = ParseBlobRefError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = s.as_bytes();
        if bytes.len() != 64 {
            return Err(ParseBlobRefError::Length(bytes.len()));
        }
        let nibble = |i: usize| -> Result<u8, ParseBlobRefError> {
            match bytes[i] {
                c @ b'0'..=b'9' => Ok(c - b'0'),
                c @ b'a'..=b'f' => Ok(c - b'a' + 10),
                c @ b'A'..=b'F' => Ok(c - b'A' + 10),
                _ => Err(ParseBlobRefError::NonHex(i)),
            }
        };
        let mut out = [0u8; 32];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = (nibble(2 * i)? << 4) | nibble(2 * i + 1)?;
        }
        Ok(Self(out))
    }
}

impl Serialize for BlobRef {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for BlobRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn new_session_ids_are_unique_and_time_ordered() {
        let a = SessionId::new();
        let b = SessionId::new();
        assert_ne!(a, b);
        assert!(a.as_ulid().timestamp_ms() <= b.as_ulid().timestamp_ms());
    }

    #[test]
    fn task_id_round_trips_through_display_and_from_str() {
        let id = TaskId::new();
        let parsed: TaskId = id.to_string().parse().unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn approval_ids_are_unique() {
        assert_ne!(ApprovalId::new(), ApprovalId::new());
    }

    #[test]
    fn id_from_str_rejects_garbage_with_input_in_error() {
        let err = "not-a-ulid".parse::<SessionId>().unwrap_err();
        assert_eq!(err.input, "not-a-ulid");
    }

    #[test]
    fn session_id_serialises_as_plain_string() {
        let id = SessionId::new();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id}\""));
        let back: SessionId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn seq_starts_at_one_and_increments() {
        assert_eq!(Seq::FIRST, Seq(1));
        assert_eq!(Seq::FIRST.next(), Seq(2));
    }

    #[test]
    fn seq_displays_with_hash_prefix() {
        assert_eq!(Seq(812).to_string(), "#812");
    }

    #[test]
    fn blob_ref_hex_is_lowercase_and_64_chars() {
        let r = BlobRef::from_bytes([0xAB; 32]);
        assert_eq!(r.to_hex(), "ab".repeat(32));
    }

    #[test]
    fn blob_ref_parse_accepts_uppercase_hex() {
        let r: BlobRef = "AB".repeat(32).parse().unwrap();
        assert_eq!(r, BlobRef::from_bytes([0xAB; 32]));
    }

    #[test]
    fn blob_ref_parse_rejects_wrong_length() {
        assert_eq!("abcd".parse::<BlobRef>(), Err(ParseBlobRefError::Length(4)));
    }

    #[test]
    fn blob_ref_parse_reports_position_of_non_hex_char() {
        let mut s = "0".repeat(64);
        s.replace_range(10..11, "g");
        assert_eq!(s.parse::<BlobRef>(), Err(ParseBlobRefError::NonHex(10)));
    }

    #[test]
    fn blob_ref_serialises_as_hex_string() {
        let r = BlobRef::from_bytes([1; 32]);
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, format!("\"{}\"", "01".repeat(32)));
        assert_eq!(serde_json::from_str::<BlobRef>(&json).unwrap(), r);
    }

    proptest! {
        #[test]
        fn blob_ref_hex_round_trips(bytes in proptest::array::uniform32(any::<u8>())) {
            let r = BlobRef::from_bytes(bytes);
            prop_assert_eq!(r.to_hex().parse::<BlobRef>().unwrap(), r);
        }
    }
}
