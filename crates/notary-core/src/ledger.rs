//! The work-order ledger, adapted from agora-rs (`crates/agora-engine/src/ledger.rs`).
//!
//! Each entry names one content-addressed record (evidence, the builder's claims, the
//! Notary's audit, a person's decision) and is hashed together with the previous entry's
//! hash, so changing, dropping or reordering any past entry breaks every hash after it.
//! Records are stored by id and re-hashed whenever they are read, so a record edited in
//! place is caught too. Only the Notary writes here; agents can't.
//!
//! Dropping the *newest* entries leaves a shorter chain that is still valid, so the head is
//! signed and posted into the Switch room, and [`verify_head`] checks against it.
//!
//! Changed from agora-rs: no salts and no forgetting (there's no personal data here, and an
//! unsalted content hash is what lets agents cite records); entries point at record ids
//! instead of embedding a tool call; the hashed form is canonical JSON, not struct order.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::{Code, NotaryError};
use crate::ids;

/// The hash before the first entry.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    Evidence,
    Policy,
    Report,
    Audit,
    Decision,
}

impl RecordKind {
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Evidence => "ev",
            Self::Policy => "po",
            Self::Report => "rp",
            Self::Audit => "au",
            Self::Decision => "dc",
        }
    }

    #[must_use]
    pub fn of_id(id: &str) -> Option<Self> {
        [Self::Evidence, Self::Policy, Self::Report, Self::Audit, Self::Decision].into_iter().find(|k| ids::is_id(k.prefix(), id))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// 1, 2, 3… within the run.
    pub seq: u64,
    pub at_ms: u64,
    pub kind: RecordKind,
    pub record: String,
    /// The previous entry's hash ([`GENESIS`] for the first).
    pub prev: String,
    pub hash: String,
}

/// Where the chain stops matching what was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broken {
    pub seq: u64,
    pub reason: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Appended {
    Added(Entry),
    /// The record was already in this run's chain, at `seq`.
    Duplicate { seq: u64 },
}

/// The exact fields an entry's hash covers.
#[derive(Serialize)]
struct Hashed<'a> {
    run: &'a str,
    seq: u64,
    at_ms: u64,
    kind: RecordKind,
    record: &'a str,
    prev: &'a str,
}

fn hash_of(h: &Hashed<'_>) -> String {
    // Hashed holds only strings and small integers, so it always has a canonical form.
    ids::canonical(h).map(|b| ids::sha_hex(&b)).unwrap_or_default()
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn to_i64(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn to_u64(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

#[derive(Debug)]
pub struct Ledger {
    db: Mutex<Connection>,
}

impl Ledger {
    /// The ledger database at `path` (created if missing).
    ///
    /// # Errors
    /// If the file can't be opened or the tables created.
    pub fn open(path: &Path) -> Result<Self, NotaryError> {
        Self::with(Connection::open(path)?)
    }

    /// A ledger that lives only as long as the process (tests).
    ///
    /// # Errors
    /// If SQLite can't create the tables.
    pub fn in_memory() -> Result<Self, NotaryError> {
        Self::with(Connection::open_in_memory()?)
    }

    fn with(db: Connection) -> Result<Self, NotaryError> {
        db.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS ledger (
                 run_id TEXT NOT NULL, seq INTEGER NOT NULL, at_ms INTEGER NOT NULL,
                 kind TEXT NOT NULL, record TEXT NOT NULL, prev TEXT NOT NULL, hash TEXT NOT NULL,
                 PRIMARY KEY (run_id, seq), UNIQUE (run_id, record));
             CREATE TABLE IF NOT EXISTS records (id TEXT PRIMARY KEY, bytes BLOB NOT NULL);",
        )?;
        Ok(Self { db: Mutex::new(db) })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Store `value` as a record of `kind` and chain it into `run`. Reads the head and writes
    /// the entry under one lock, so concurrent writers still form a single chain.
    /// Returns the record id (the same value always gets the same id).
    ///
    /// # Errors
    /// If `value` has no canonical form or the database can't be written.
    pub fn append(&self, run: &str, kind: RecordKind, value: &impl Serialize) -> Result<(String, Appended), NotaryError> {
        let bytes = ids::canonical(value)?;
        let id = format!("{}_{}", kind.prefix(), ids::sha_hex(&bytes));
        let db = self.lock();
        if let Some(seq) = db.query_row("SELECT seq FROM ledger WHERE run_id = ?1 AND record = ?2", params![run, id], |r| r.get::<_, i64>(0)).optional()? {
            return Ok((id, Appended::Duplicate { seq: to_u64(seq) }));
        }
        db.execute("INSERT OR IGNORE INTO records (id, bytes) VALUES (?1, ?2)", params![id, bytes])?;
        let head: Option<(i64, String)> = db
            .query_row("SELECT seq, hash FROM ledger WHERE run_id = ?1 ORDER BY seq DESC LIMIT 1", params![run], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        let (seq, prev) = head.map_or((1, GENESIS.to_string()), |(s, h)| (to_u64(s) + 1, h));
        let at_ms = now_ms();
        let hash = hash_of(&Hashed { run, seq, at_ms, kind, record: &id, prev: &prev });
        let kind_name = serde_json::to_value(kind).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
        db.execute(
            "INSERT INTO ledger (run_id, seq, at_ms, kind, record, prev, hash) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![run, to_i64(seq), to_i64(at_ms), kind_name, id, prev, hash],
        )?;
        Ok((id.clone(), Appended::Added(Entry { seq, at_ms, kind, record: id, prev, hash })))
    }

    /// Every entry of `run`, in order.
    ///
    /// # Errors
    /// If the database can't be read or a row is malformed.
    pub fn entries(&self, run: &str) -> Result<Vec<Entry>, NotaryError> {
        let db = self.lock();
        let mut stmt = db.prepare("SELECT seq, at_ms, kind, record, prev, hash FROM ledger WHERE run_id = ?1 ORDER BY seq")?;
        let rows = stmt.query_map(params![run], |r| {
            Ok((to_u64(r.get(0)?), to_u64(r.get(1)?), r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?, r.get::<_, String>(5)?))
        })?;
        rows.map(|row| {
            let (seq, at_ms, kind, record, prev, hash) = row?;
            let kind: RecordKind = serde_json::from_value(Value::String(kind.clone()))
                .map_err(|_| NotaryError::new(Code::IntegrityMismatch, format!("entry {seq}: unknown kind {kind}")))?;
            Ok(Entry { seq, at_ms, kind, record, prev, hash })
        })
        .collect()
    }

    /// The record `id`, re-hashed on the way out: the ledger is evidence, so it is never read leniently.
    ///
    /// # Errors
    /// `integrity_mismatch` if the stored bytes no longer hash to `id`; `None` if there is no such record.
    pub fn record(&self, id: &str) -> Result<Option<Value>, NotaryError> {
        let bytes: Option<Vec<u8>> = self.lock().query_row("SELECT bytes FROM records WHERE id = ?1", params![id], |r| r.get(0)).optional()?;
        let Some(bytes) = bytes else { return Ok(None) };
        let prefix = id.split('_').next().unwrap_or_default();
        if format!("{prefix}_{}", ids::sha_hex(&bytes)) != id {
            return Err(NotaryError::new(Code::IntegrityMismatch, format!("record {id} changed after it was written")));
        }
        serde_json::from_slice(&bytes).map(Some).map_err(|e| NotaryError::new(Code::IntegrityMismatch, format!("record {id}: {e}")))
    }

    /// Ids in `run` that start with `prefix` (for `show` and citations), up to `limit`.
    ///
    /// # Errors
    /// If the database can't be read.
    pub fn find(&self, run: &str, prefix: &str, limit: usize) -> Result<Vec<String>, NotaryError> {
        let db = self.lock();
        let mut stmt = db.prepare("SELECT record FROM ledger WHERE run_id = ?1 AND substr(record, 1, length(?2)) = ?2 ORDER BY seq LIMIT ?3")?;
        let rows = stmt.query_map(params![run, prefix, to_i64(limit as u64)], |r| r.get(0))?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// Recompute `run`'s chain and re-hash every record it names.
    ///
    /// # Errors
    /// [`Broken`] at the first entry that was changed, removed or moved.
    pub fn verify(&self, run: &str) -> Result<usize, Broken> {
        let entries = self.entries(run).map_err(|e| Broken { seq: 0, reason: e.to_string() })?;
        for e in &entries {
            match self.record(&e.record) {
                Ok(Some(_)) => {}
                Ok(None) => return Err(Broken { seq: e.seq, reason: format!("record {} is missing", e.record) }),
                Err(err) => return Err(Broken { seq: e.seq, reason: err.detail }),
            }
        }
        verify_chain(run, &entries)
    }

    /// Everything a receipt needs: the entries and every record they name.
    ///
    /// # Errors
    /// If any record is missing or changed.
    pub fn export(&self, run: &str) -> Result<(Vec<Entry>, BTreeMap<String, Value>), NotaryError> {
        let entries = self.entries(run)?;
        let mut records = BTreeMap::new();
        for e in &entries {
            let v = self.record(&e.record)?.ok_or_else(|| NotaryError::new(Code::IntegrityMismatch, format!("record {} is missing", e.record)))?;
            records.insert(e.record.clone(), v);
        }
        Ok((entries, records))
    }
}

/// Check a sequence of entries forms one unbroken chain for `run`.
///
/// # Errors
/// [`Broken`] at the first entry that doesn't follow from the one before.
pub fn verify_chain(run: &str, entries: &[Entry]) -> Result<usize, Broken> {
    let mut prev = GENESIS.to_string();
    for (i, e) in entries.iter().enumerate() {
        let expected_seq = i as u64 + 1;
        if e.seq != expected_seq {
            return Err(Broken { seq: e.seq, reason: format!("entry {expected_seq} is missing") });
        }
        if e.prev != prev {
            return Err(Broken { seq: e.seq, reason: "does not follow the entry before it".into() });
        }
        if RecordKind::of_id(&e.record) != Some(e.kind) {
            return Err(Broken { seq: e.seq, reason: "its record id doesn't match its kind".into() });
        }
        let hash = hash_of(&Hashed { run, seq: e.seq, at_ms: e.at_ms, kind: e.kind, record: &e.record, prev: &e.prev });
        if hash != e.hash {
            return Err(Broken { seq: e.seq, reason: "it was changed after it was written".into() });
        }
        prev.clone_from(&e.hash);
    }
    Ok(entries.len())
}

/// Verify the chain and that it ends at `head`, a head published earlier, so entries
/// dropped from the end are caught too.
///
/// # Errors
/// [`Broken`] if the chain doesn't verify or doesn't end at `head`.
pub fn verify_head(run: &str, entries: &[Entry], head: &str) -> Result<usize, Broken> {
    let n = verify_chain(run, entries)?;
    match entries.iter().position(|e| e.hash == head) {
        Some(_) => Ok(n),
        None => Err(Broken { seq: entries.last().map_or(0, |e| e.seq) + 1, reason: "the published head is missing: entries were dropped from the end".into() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ledger_with_three() -> Ledger {
        let ledger = Ledger::in_memory().unwrap();
        ledger.append("WO-1", RecordKind::Policy, &json!({"approvers": ["priya"]})).unwrap();
        ledger.append("WO-1", RecordKind::Evidence, &json!({"result": {"failed": 1}})).unwrap();
        ledger.append("WO-1", RecordKind::Audit, &json!({"verdict": "contradicted"})).unwrap();
        ledger
    }

    #[test]
    fn an_untouched_chain_verifies_and_reads_back() {
        let ledger = ledger_with_three();
        assert_eq!(ledger.verify("WO-1"), Ok(3));
        let e = ledger.entries("WO-1").unwrap();
        assert_eq!(e[0].prev, GENESIS);
        assert_eq!(e[1].prev, e[0].hash);
        assert_eq!(ledger.record(&e[1].record).unwrap(), Some(json!({"result": {"failed": 1}})));
    }

    #[test]
    fn appending_the_same_record_twice_is_a_duplicate() {
        let ledger = ledger_with_three();
        let (_, again) = ledger.append("WO-1", RecordKind::Evidence, &json!({"result": {"failed": 1}})).unwrap();
        assert_eq!(again, Appended::Duplicate { seq: 2 });
        assert_eq!(ledger.verify("WO-1"), Ok(3));
    }

    #[test]
    fn editing_a_record_in_place_is_caught() {
        let ledger = ledger_with_three();
        let id = ledger.entries("WO-1").unwrap()[1].record.clone();
        ledger.lock().execute("UPDATE records SET bytes = ?1 WHERE id = ?2", params![br#"{"result":{"failed":0}}"#.to_vec(), id]).unwrap();
        assert_eq!(ledger.record(&id).unwrap_err().code, Code::IntegrityMismatch);
        assert_eq!(ledger.verify("WO-1").unwrap_err().seq, 2);
    }

    #[test]
    fn rewriting_an_entry_is_caught_at_the_entry_that_changed() {
        let ledger = ledger_with_three();
        ledger.lock().execute("UPDATE ledger SET at_ms = at_ms + 1 WHERE seq = 2", []).unwrap();
        assert_eq!(ledger.verify("WO-1"), Err(Broken { seq: 2, reason: "it was changed after it was written".into() }));
    }

    #[test]
    fn deleting_an_entry_is_caught() {
        let ledger = ledger_with_three();
        ledger.lock().execute("DELETE FROM ledger WHERE seq = 2", []).unwrap();
        assert_eq!(ledger.verify("WO-1").unwrap_err().reason, "entry 2 is missing");
    }

    #[test]
    fn a_chain_copied_into_another_run_does_not_verify() {
        let ledger = ledger_with_three();
        let entries = ledger.entries("WO-1").unwrap();
        assert_eq!(verify_chain("WO-1", &entries), Ok(3));
        assert_eq!(verify_chain("WO-2", &entries).unwrap_err().seq, 1);
    }

    #[test]
    fn dropping_the_newest_entry_is_caught_against_a_published_head() {
        let ledger = ledger_with_three();
        let head = ledger.entries("WO-1").unwrap()[2].hash.clone();
        assert_eq!(verify_head("WO-1", &ledger.entries("WO-1").unwrap(), &head), Ok(3));
        ledger.lock().execute("DELETE FROM ledger WHERE seq = 3", []).unwrap();
        assert_eq!(ledger.verify("WO-1"), Ok(2), "a shorter chain is still a valid chain");
        assert!(verify_head("WO-1", &ledger.entries("WO-1").unwrap(), &head).unwrap_err().reason.contains("dropped from the end"));
    }

    #[test]
    fn runs_are_separate_chains_sharing_records() {
        let ledger = ledger_with_three();
        let (id, added) = ledger.append("WO-2", RecordKind::Evidence, &json!({"result": {"failed": 1}})).unwrap();
        assert!(matches!(added, Appended::Added(Entry { seq: 1, .. })));
        assert_eq!(ledger.find("WO-1", &id[..12], 5).unwrap(), vec![id]);
        assert_eq!(ledger.verify("WO-2"), Ok(1));
    }
}
