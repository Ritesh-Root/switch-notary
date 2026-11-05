//! A read-only window onto the desk, with no login: both rooms' messages (read through Switch's
//! history API with the Notary's own key), the work orders, each work order's ledger, and the
//! receipts, each checked with the same code as `notary-verify`. `/api/wo/{wo}` returns one work order's
//! whole case file: its row, every ledger entry and record, and what each code in it means. It binds to localhost only and
//! changes nothing; acting still happens in chat.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use notary_core::errors::Code;
use notary_core::ledger::{self, Ledger};
use notary_core::receipt::{self, Bundle};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};

use crate::switch::Agent;

const PAGE: &str = include_str!("viewer.html");

pub struct View {
    pub agent: Agent,
    pub hub: String,
    pub work: String,
    pub data_dir: PathBuf,
    pub key: String,
}

/// Serve the viewer on `addr` until the process exits.
pub fn spawn(addr: SocketAddr, view: View) {
    let app = Router::new()
        .route("/", get(|| async { Html(PAGE) }))
        .route("/api/state", get(state))
        .route("/api/ledger/{wo}", get(ledger))
        .route("/api/wo/{wo}", get(case_file))
        .route("/receipts/{file}", get(receipt_file))
        .with_state(Arc::new(view));
    tokio::spawn(async move {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                eprintln!("notary: viewer at http://{addr}");
                if let Err(e) = axum::serve(listener, app).await {
                    eprintln!("notary: viewer stopped: {e}");
                }
            }
            Err(e) => eprintln!("notary: viewer couldn't bind {addr}: {e}"),
        }
    });
}

async fn messages(view: &View, room: &str) -> Value {
    match view.agent.history(room, 120).await {
        Ok(v) => v.get("events").cloned().unwrap_or_default(),
        Err(e) => json!([{"sender_name": "viewer", "body": format!("couldn't read this room from Switch: {e}"), "timestamp": null}]),
    }
}

fn work_orders(view: &View) -> Value {
    let Ok(db) = Connection::open_with_flags(view.data_dir.join("desk.db"), OpenFlags::SQLITE_OPEN_READ_ONLY) else { return json!([]) };
    let Ok(mut stmt) = db.prepare("SELECT wo, title, requester, state, attempts FROM work_orders WHERE wo IS NOT NULL ORDER BY n DESC LIMIT 50")
    else {
        return json!([]);
    };
    let rows = stmt.query_map([], |r| {
        Ok(json!({"wo": r.get::<_, String>(0)?, "title": r.get::<_, String>(1)?, "requester": r.get::<_, String>(2)?,
                  "state": r.get::<_, String>(3)?, "attempts": r.get::<_, i64>(4)?}))
    });
    rows.map(|r| Value::Array(r.flatten().collect())).unwrap_or_default()
}

fn receipts(view: &View) -> Value {
    let dir = view.data_dir.join("receipts");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !valid_receipt_name(&name) {
            continue;
        }
        let checked = std::fs::read(entry.path())
            .map_err(|e| e.to_string())
            .and_then(|b| serde_json::from_slice::<Bundle>(&b).map_err(|e| e.to_string()))
            .and_then(|b| receipt::verify(&b, Some(&view.key)));
        out.push(match checked {
            Ok(v) => json!({"file": name, "wo": v.run, "ok": true, "entries": v.entries, "head": v.head}),
            Err(why) => json!({"file": name, "ok": false, "why": why}),
        });
    }
    out.sort_by_key(|r| {
        std::cmp::Reverse(r["file"].as_str().and_then(|f| f.trim_start_matches("receipt-wo-").trim_end_matches(".json").parse::<u64>().ok()))
    });
    Value::Array(out)
}

async fn state(State(view): State<Arc<View>>) -> Json<Value> {
    let (desk, work) = tokio::join!(messages(&view, &view.hub), messages(&view, &view.work));
    Json(json!({"desk": desk, "work": work, "work_orders": work_orders(&view), "receipts": receipts(&view), "key": view.key}))
}

/// `WO-7` (any case) → `Some("WO-7")`; anything else → `None`.
fn wo_id(wo: &str) -> Option<String> {
    let wo = wo.to_uppercase();
    wo.strip_prefix("WO-").is_some_and(|n| !n.is_empty() && n.len() <= 9 && n.bytes().all(|b| b.is_ascii_digit())).then_some(wo)
}

async fn ledger(State(view): State<Arc<View>>, Path(wo): Path<String>) -> Response {
    let Some(wo) = wo_id(&wo) else { return (StatusCode::BAD_REQUEST, "not a work order").into_response() };
    match Ledger::open(&view.data_dir.join("ledger.db")) {
        Ok(l) => {
            let verified = l.verify(&wo).map_err(|b| format!("broken at entry {}: {}", b.seq, b.reason));
            Json(json!({"wo": wo, "lines": crate::desk::ledger_lines(&l, &wo), "chain": verified.map_or_else(|e| e, |n| format!("{n} entries, chain intact"))}))
                .into_response()
        }
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response(),
    }
}

/// One work order's row: `Ok(None)` if there's no such work order, `Err` if the desk can't be read.
fn order(view: &View, wo: &str) -> Result<Option<Value>, String> {
    let db = Connection::open_with_flags(view.data_dir.join("desk.db"), OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
    db.query_row("SELECT wo, title, requester, state, attempts, base, branch, created_ms FROM work_orders WHERE wo = ?1", [wo], |r| {
        Ok(json!({"wo": r.get::<_, String>(0)?, "title": r.get::<_, String>(1)?, "requester": r.get::<_, String>(2)?,
                      "state": r.get::<_, String>(3)?, "attempts": r.get::<_, i64>(4)?, "base": r.get::<_, String>(5)?,
                      "branch": r.get::<_, String>(6)?, "created_ms": r.get::<_, i64>(7)?}))
    })
    .optional()
    .map_err(|e| e.to_string())
}

/// What each code in these records means, plus the two the desk adds outside audits.
fn code_meanings(records: &BTreeMap<String, Value>) -> Value {
    let mut codes: BTreeSet<Code> = BTreeSet::from([Code::RevisionLimit, Code::NotAnApprover]);
    for audit in records.iter().filter(|(id, _)| id.starts_with("au_")).map(|(_, v)| v) {
        for item in ["checked", "findings"].iter().filter_map(|k| audit[*k].as_array()).flatten() {
            if let Ok(code) = serde_json::from_value::<Code>(item["code"].clone()) {
                codes.insert(code);
            }
        }
    }
    codes
        .into_iter()
        .map(|c| {
            let m = c.meta();
            (c.to_string(), json!({"retryable": m.retryable, "fixer": m.fixer, "remediation": m.remediation}))
        })
        .collect::<serde_json::Map<_, _>>()
        .into()
}

async fn case_file(State(view): State<Arc<View>>, Path(wo): Path<String>) -> Response {
    let Some(wo) = wo_id(&wo) else { return (StatusCode::BAD_REQUEST, "not a work order").into_response() };
    let order = match order(&view, &wo) {
        Ok(Some(o)) => o,
        Ok(None) => return (StatusCode::NOT_FOUND, "no such work order").into_response(),
        Err(e) => return (StatusCode::SERVICE_UNAVAILABLE, format!("couldn't read the desk: {e}")).into_response(),
    };
    let ledger = match Ledger::open(&view.data_dir.join("ledger.db")) {
        Ok(l) => l,
        Err(e) => return (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response(),
    };
    // One read: the chain is checked over exactly the entries returned. `export` re-hashes every
    // record, so if one is missing or changed the entries still come back, without records.
    let (entries, records, chain) = match ledger.export(&wo) {
        Ok((entries, records)) => {
            let chain = match ledger::verify_chain(&wo, &entries) {
                Ok(n) => json!({"ok": true, "entries": n}),
                Err(b) => json!({"ok": false, "error": format!("broken at entry {}: {}", b.seq, b.reason)}),
            };
            (entries, records, chain)
        }
        Err(e) => (ledger.entries(&wo).unwrap_or_default(), BTreeMap::new(), json!({"ok": false, "error": e.to_string()})),
    };
    let codes = code_meanings(&records);
    Json(json!({"wo": wo, "order": order, "chain": chain, "entries": entries, "records": records, "codes": codes})).into_response()
}

fn valid_receipt_name(name: &str) -> bool {
    name.strip_prefix("receipt-wo-").and_then(|r| r.strip_suffix(".json")).is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

async fn receipt_file(State(view): State<Arc<View>>, Path(file): Path<String>) -> Response {
    if !valid_receipt_name(&file) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match std::fs::read(view.data_dir.join("receipts").join(&file)) {
        Ok(bytes) => {
            ([(header::CONTENT_TYPE, "application/json"), (header::CONTENT_DISPOSITION, &format!("attachment; filename=\"{file}\""))], bytes)
                .into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn work_order_ids_are_strict() {
        assert_eq!(super::wo_id("wo-12").as_deref(), Some("WO-12"));
        for bad in ["WO-", "WO-1a", "../WO-1", "WO-1/..", "WO-1234567890", "7"] {
            assert_eq!(super::wo_id(bad), None, "{bad}");
        }
    }

    #[test]
    fn code_meanings_cover_audit_codes() {
        let audit = serde_json::json!({"checked": [{"code": "tests_failed"}, {"verdict": "backed"}], "findings": [{"code": "protected_path"}]});
        let records = std::collections::BTreeMap::from([("au_x".to_string(), audit)]);
        let m = super::code_meanings(&records);
        for code in ["tests_failed", "protected_path", "revision_limit", "not_an_approver"] {
            assert!(m[code]["remediation"].is_string(), "{code}");
        }
        assert_eq!(m["tests_failed"]["fixer"], "builder");
    }

    #[test]
    fn only_receipt_files_are_served() {
        assert!(super::valid_receipt_name("receipt-wo-7.json"));
        for bad in ["../ledger.db", "receipt-wo-.json", "receipt-wo-7.json/../../x", "receipt-wo-7.txt", "desk.db"] {
            assert!(!super::valid_receipt_name(bad), "{bad}");
        }
    }
}
