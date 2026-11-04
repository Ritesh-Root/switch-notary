//! A receipt: one self-contained JSON file holding a work order's signed head, its entries
//! and every record they name. Anyone can check it offline with `agora-verify`: every
//! record re-hashes to its id, the entries form one chain ending at the signed head, and
//! the signature is the Notary's.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids;
use crate::ledger::{Entry, RecordKind, verify_head};
use crate::signing::SignedHead;

pub const PROTOCOL: &str = "agora-notary-receipt/1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bundle {
    pub protocol: String,
    pub head: SignedHead,
    pub entries: Vec<Entry>,
    pub records: BTreeMap<String, Value>,
}

/// What a valid receipt proves, for printing.
#[derive(Debug, PartialEq, Eq)]
pub struct Verified {
    pub run: String,
    pub entries: usize,
    pub head: String,
    pub key: String,
}

/// Check `bundle` completely. With `pinned_key`, also require that key.
///
/// # Errors
/// The first thing that doesn't hold, in words.
pub fn verify(bundle: &Bundle, pinned_key: Option<&str>) -> Result<Verified, String> {
    if bundle.protocol != PROTOCOL {
        return Err(format!("unknown receipt protocol {:?}", bundle.protocol));
    }
    for (id, value) in &bundle.records {
        let kind = RecordKind::of_id(id).ok_or_else(|| format!("{id} isn't a record id"))?;
        let bytes = ids::canonical(value).map_err(|e| format!("{id}: {e}"))?;
        if format!("{}_{}", kind.prefix(), ids::sha_hex(&bytes)) != *id {
            return Err(format!(
                "record {id} was changed: it no longer hashes to its id"
            ));
        }
    }
    for e in &bundle.entries {
        if !bundle.records.contains_key(&e.record) {
            return Err(format!(
                "entry {} names record {} which isn't in the receipt",
                e.seq, e.record
            ));
        }
    }
    let n = verify_head(&bundle.head.run, &bundle.entries, &bundle.head.head)
        .map_err(|b| format!("chain broken at entry {}: {}", b.seq, b.reason))?;
    if n != bundle.head.entries
        || bundle.entries.last().map(|e| e.hash.as_str()) != Some(bundle.head.head.as_str())
    {
        return Err(format!(
            "the signed head covers {} entries but the receipt holds {n}",
            bundle.head.entries
        ));
    }
    bundle.head.verify()?;
    if let Some(k) = pinned_key
        && !k.eq_ignore_ascii_case(&bundle.head.key)
    {
        return Err("signed by a different key than the one you pinned".into());
    }
    Ok(Verified {
        run: bundle.head.run.clone(),
        entries: n,
        head: bundle.head.head.clone(),
        key: bundle.head.key.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Ledger;
    use crate::signing::Signer;
    use serde_json::json;

    fn bundle() -> (Bundle, Signer) {
        let ledger = Ledger::in_memory().unwrap();
        for v in [json!({"approvers": ["priya"]}), json!({"x": 1})] {
            ledger.append("WO-17", RecordKind::Evidence, &v).unwrap();
        }
        ledger
            .append(
                "WO-17",
                RecordKind::Decision,
                &json!({"answer": "approve", "by": "priya"}),
            )
            .unwrap();
        let (entries, records) = ledger.export("WO-17").unwrap();
        let signer = Signer::ephemeral();
        let last = entries.last().unwrap();
        let head = signer.sign("WO-17", entries.len(), &last.hash, last.at_ms);
        (
            Bundle {
                protocol: PROTOCOL.into(),
                head,
                entries,
                records,
            },
            signer,
        )
    }

    #[test]
    fn a_fresh_receipt_verifies() {
        let (b, signer) = bundle();
        let v = verify(&b, Some(&signer.public_hex())).unwrap();
        assert_eq!((v.run.as_str(), v.entries), ("WO-17", 3));
    }

    #[test]
    fn a_one_byte_edit_anywhere_fails() {
        let (b, _) = bundle();
        let text = serde_json::to_string(&b).unwrap();
        let edited = text.replacen("priya", "priyb", 1);
        let tampered: Bundle = serde_json::from_str(&edited).unwrap();
        assert!(verify(&tampered, None).unwrap_err().contains("was changed"));
    }

    #[test]
    fn dropped_entries_and_foreign_keys_fail() {
        let (mut b, _) = bundle();
        b.entries.pop();
        assert!(verify(&b, None).is_err());
        let (b, _) = bundle();
        assert!(
            verify(&b, Some(&Signer::ephemeral().public_hex()))
                .unwrap_err()
                .contains("different key")
        );
    }
}
