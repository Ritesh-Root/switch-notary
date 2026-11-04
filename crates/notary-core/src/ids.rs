//! Content addressing. A record's id is `<prefix>_` + SHA-256 of its canonical JSON, so a
//! changed record is caught the moment it is read back, and anyone can check that an id an
//! agent cites really names that record, with no model involved.
//!
//! Canonical JSON = `serde_json`'s `Value` (keys sorted, since `preserve_order` is off),
//! compact, with no floats and no integer above 2^53. For this domain (ASCII keys) that is
//! byte-identical to RFC 8785. We hash the `Value`, not a struct, so reordering struct
//! fields can never change an id.

use std::fmt::Write as _;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::errors::{Code, NotaryError};

#[must_use]
pub fn sha_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().fold(String::with_capacity(64), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    })
}

/// The canonical bytes of `v`.
///
/// # Errors
/// `integrity_mismatch` if `v` holds a float or an integer JSON can't carry exactly.
pub fn canonical<T: Serialize>(v: &T) -> Result<Vec<u8>, NotaryError> {
    let v = serde_json::to_value(v).map_err(|e| NotaryError::new(Code::IntegrityMismatch, e.to_string()))?;
    if !plain_numbers(&v) {
        return Err(NotaryError::new(Code::IntegrityMismatch, "floats and integers above 2^53 have no canonical form"));
    }
    serde_json::to_vec(&v).map_err(|e| NotaryError::new(Code::IntegrityMismatch, e.to_string()))
}

fn plain_numbers(v: &Value) -> bool {
    const MAX: u64 = 1 << 53;
    match v {
        Value::Number(n) => n.as_u64().is_some_and(|x| x <= MAX) || n.as_i64().is_some_and(|x| x.unsigned_abs() <= MAX),
        Value::Array(a) => a.iter().all(plain_numbers),
        Value::Object(o) => o.values().all(plain_numbers),
        _ => true,
    }
}

/// `prefix_` + SHA-256 of `v`'s canonical bytes.
///
/// # Errors
/// As [`canonical`].
pub fn content_id<T: Serialize>(prefix: &str, v: &T) -> Result<String, NotaryError> {
    Ok(format!("{prefix}_{}", sha_hex(&canonical(v)?)))
}

/// Whether `s` is `prefix_` followed by 64 lowercase hex digits.
#[must_use]
pub fn is_id(prefix: &str, s: &str) -> bool {
    s.strip_prefix(prefix).and_then(|r| r.strip_prefix('_')).is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

/// Whether `s` is a full git commit id (40 lowercase hex). Short ids are ambiguous, so refused.
#[must_use]
pub fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_are_sorted_and_output_is_compact() {
        assert_eq!(canonical(&json!({"b": 1, "a": [2, {"d": 3, "c": "x"}]})).unwrap(), br#"{"a":[2,{"c":"x","d":3}],"b":1}"#);
    }

    #[test]
    fn floats_and_huge_integers_are_refused() {
        assert!(canonical(&json!({"x": 1.5})).is_err());
        assert!(canonical(&json!({"x": u64::MAX})).is_err());
        assert!(canonical(&json!({"x": -42})).is_ok());
    }

    #[test]
    fn ids_are_stable_and_validated() {
        let id = content_id("ev", &json!({"a": 1})).unwrap();
        assert_eq!(id, content_id("ev", &json!({"a": 1})).unwrap());
        assert!(is_id("ev", &id));
        assert!(!is_id("au", &id));
        assert!(!is_id("ev", "ev_123"));
        assert!(!is_id("ev", &id.to_uppercase()));
        assert!(is_sha(&"a".repeat(40)));
        assert!(!is_sha("3f9a2c1"));
    }
}
