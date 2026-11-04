//! Evidence: one observation, with who vouches for it, how certain it is and how complete,
//! as separate fields. Its id is the hash of all of that, so it can be cited and re-checked.
//! There is deliberately no timestamp in the body: time lives in the ledger entry, so the
//! same observation made twice gets the same id.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::errors::{Code, NotaryError};
use crate::ids;

/// Who vouches for an observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    /// The Notary's own git mirror, after a successful fetch.
    Git,
    /// The Notary's own sandboxed run.
    NotaryRun,
    /// Switch's tool-activity log, written by the agent's session host with the agent's key.
    SwitchHost,
    /// Something an agent said about itself.
    SelfReported,
    /// A person's answer (an approval card).
    Human,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Observed,
    Derived,
    Inferred,
}

/// How much of the question the observation covers. There is no default: every source
/// has to say, and "nothing found" with partial coverage is never proof of none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Coverage {
    Complete,
    Partial { limitations: Vec<String> },
    Unknown { reason: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    GitCommit,
    GitDiff,
    NotaryTestRun,
    SwitchActivity,
    DeskPolicy,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subject {
    pub repo: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
}

/// Everything the id covers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Body {
    pub authority: Authority,
    pub confidence: Confidence,
    pub coverage: Coverage,
    pub subject: Subject,
    pub operation: Op,
    pub params: Map<String, Value>,
    pub result: Value,
    pub limitations: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Evidence {
    pub id: String,
    pub body: Body,
}

impl Evidence {
    /// Give `body` its id.
    ///
    /// # Errors
    /// If the body has no canonical form (a float somewhere).
    pub fn seal(body: Body) -> Result<Self, NotaryError> {
        Ok(Self { id: ids::content_id("ev", &body)?, body })
    }

    /// Read evidence back from its stored bytes, refusing anything that no longer hashes to `id`.
    ///
    /// # Errors
    /// `integrity_mismatch` if the bytes changed or don't parse.
    pub fn authenticate(id: &str, stored: &[u8]) -> Result<Self, NotaryError> {
        let bad = |why: &str| NotaryError::new(Code::IntegrityMismatch, format!("{id}: {why}"));
        if format!("ev_{}", ids::sha_hex(stored)) != id {
            return Err(bad("stored bytes don't hash to this id"));
        }
        let body: Body = serde_json::from_slice(stored).map_err(|e| bad(&e.to_string()))?;
        Ok(Self { id: id.to_string(), body })
    }

    /// The bytes to store (canonical).
    ///
    /// # Errors
    /// As [`ids::canonical`].
    pub fn bytes(&self) -> Result<Vec<u8>, NotaryError> {
        ids::canonical(&self.body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body() -> Body {
        Body {
            authority: Authority::NotaryRun,
            confidence: Confidence::Observed,
            coverage: Coverage::Complete,
            subject: Subject { repo: "acme-invoices".into(), sha: Some("a".repeat(40)) },
            operation: Op::NotaryTestRun,
            params: json!({"argv": ["pytest", "-q"]}).as_object().cloned().unwrap(),
            result: json!({"passed": 41, "failed": 1}),
            limitations: vec![],
        }
    }

    #[test]
    fn evidence_round_trips_and_tampering_is_caught() {
        let ev = Evidence::seal(body()).unwrap();
        let bytes = ev.bytes().unwrap();
        assert_eq!(Evidence::authenticate(&ev.id, &bytes).unwrap(), ev);
        let forged = String::from_utf8(bytes).unwrap().replace("\"failed\":1", "\"failed\":0");
        assert_eq!(Evidence::authenticate(&ev.id, forged.as_bytes()).unwrap_err().code, Code::IntegrityMismatch);
    }

    #[test]
    fn the_same_observation_has_the_same_id_and_any_field_changes_it() {
        let a = Evidence::seal(body()).unwrap();
        assert_eq!(a.id, Evidence::seal(body()).unwrap().id);
        let mut b = body();
        b.coverage = Coverage::Partial { limitations: vec!["log truncated".into()] };
        assert_ne!(a.id, Evidence::seal(b).unwrap().id);
    }
}
