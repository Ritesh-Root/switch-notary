//! A read-only window onto the desk, with no login: both rooms' messages (read through Switch's
//! history API with the Notary's own key), the work orders, each work order's ledger, and the
//! receipts, each checked with the same code as `notary-verify`. `/api/wo/{wo}` returns one work order's
//! whole case file: its row, every ledger entry and record, and what each code in it means.
//! `/api/evidence/{id}` returns one `ev_` record by id. It binds to localhost only and
//! changes nothing; acting still happens in chat.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
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

fn router(view: Arc<View>) -> Router {
    Router::new()
        .route("/", get(|| async { Html(PAGE) }))
        .route("/api/state", get(state))
        .route("/api/ledger/{wo}", get(ledger))
        .route("/api/wo/{wo}", get(case_file))
        .route("/api/evidence/{id}", get(evidence))
        .route("/receipts/{file}", get(receipt_file))
        .layer(middleware::from_fn(local_host_only))
        .with_state(view)
}

/// Serve the viewer on `addr` until the process exits.
pub fn spawn(addr: SocketAddr, view: View) {
    let app = router(Arc::new(view));
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

/// Answer only requests addressed to this machine by name, so a web page can't read the desk
/// through DNS rebinding (a hostile name that resolves to 127.0.0.1).
async fn local_host_only(req: Request, next: Next) -> Response {
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    if local_host(host) {
        next.run(req).await
    } else {
        (StatusCode::MISDIRECTED_REQUEST, "open the viewer at http://127.0.0.1 or http://localhost").into_response()
    }
}

/// `localhost:8090`, `127.0.0.1`, `[::1]:8090` → true; anything else → false.
fn local_host(host: &str) -> bool {
    let name = match host.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => host.rsplit_once(':').map_or(host, |(name, _)| name),
    };
    matches!(name, "127.0.0.1" | "localhost" | "::1")
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

/// One stored `ev_` record. The same JSON `/api/wo/{wo}` puts under `records`.
async fn evidence(State(view): State<Arc<View>>, Path(id): Path<String>) -> Response {
    if !notary_core::ids::is_id("ev", &id) {
        return (StatusCode::BAD_REQUEST, "not evidence").into_response();
    }
    let ledger = match Ledger::open(&view.data_dir.join("ledger.db")) {
        Ok(l) => l,
        Err(e) => return (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response(),
    };
    match ledger.record(&id) {
        Ok(Some(record)) => Json(json!({"id": id, "record": record})).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "no such evidence").into_response(),
        // The row is there; its bytes no longer hash to the id, so it is not served.
        Err(e) if e.code == Code::IntegrityMismatch => (StatusCode::CONFLICT, e.to_string()).into_response(),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response(),
    }
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
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use notary_core::ledger::{Ledger, RecordKind};
    use rusqlite::Connection;
    use serde_json::{Value, json};

    use super::*;

    struct Tmp(PathBuf);

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Aborts the viewer task. Dropping the handle alone would detach it.
    struct Server(tokio::task::JoinHandle<()>);

    impl Drop for Server {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    /// One approved order and one policy entry. Switch is a closed port, so a room read is the
    /// viewer's own error line.
    fn sample_desk() -> (Tmp, View) {
        let dir = std::env::temp_dir().join(format!("notary-view-{}", notary_core::signing::random_hex(6)));
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let db = Connection::open(dir.join("desk.db")).expect("desk.db");
        db.execute_batch(
            "CREATE TABLE work_orders (
                 n INTEGER PRIMARY KEY AUTOINCREMENT, wo TEXT UNIQUE, requester TEXT NOT NULL, title TEXT NOT NULL,
                 base TEXT NOT NULL, branch TEXT NOT NULL, policy TEXT NOT NULL, state TEXT NOT NULL,
                 attempts INTEGER NOT NULL DEFAULT 0, builder_audit TEXT, approval TEXT, created_ms INTEGER NOT NULL);
             INSERT INTO work_orders (wo, requester, title, base, branch, policy, state, attempts, created_ms)
             VALUES ('WO-1', 'sam', 'Ship the invoice export', 'abc', 'wo-1', '{}', 'approved', 1, 1700000000000);",
        )
        .expect("seed desk");
        drop(db);
        let ledger = Ledger::open(&dir.join("ledger.db")).expect("ledger");
        ledger.append("WO-1", RecordKind::Policy, &json!({"protocol": "notary-intake/1"})).expect("policy entry");
        drop(ledger);
        let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("closed port");
        let port = closed.local_addr().expect("addr").port();
        drop(closed);
        let view = View {
            agent: Agent::new(&format!("http://127.0.0.1:{port}"), "notary-test", "test-key"),
            hub: "team-desk".into(),
            work: "team-work".into(),
            data_dir: dir.clone(),
            key: "dd".into(),
        };
        (Tmp(dir), view)
    }

    async fn serve(view: View) -> (std::net::SocketAddr, Server) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("viewer bind");
        let addr = listener.local_addr().expect("viewer addr");
        let app = router(Arc::new(view));
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (addr, Server(handle))
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder().timeout(Duration::from_secs(5)).build().expect("client")
    }

    #[tokio::test]
    async fn get_root_serves_the_viewer_html() {
        let (_tmp, view) = sample_desk();
        let (addr, _server) = serve(view).await;
        let resp = client().get(format!("http://{addr}/")).send().await.expect("GET /");
        assert_eq!(resp.status().as_u16(), 200);
        assert_eq!(resp.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()), Some("text/html; charset=utf-8"));
        let body = resp.text().await.expect("html");
        assert!(body.starts_with("<!doctype html>"), "page does not start with the viewer doctype");
        assert!(body.contains("<title>Notary</title>"));
        assert!(body.contains("id=\"case\""));
    }

    #[tokio::test]
    async fn get_api_state_returns_the_desk_and_refuses_a_foreign_host() {
        let (_tmp, view) = sample_desk();
        let (addr, _server) = serve(view).await;
        let resp = client().get(format!("http://{addr}/api/state")).send().await.expect("GET /api/state");
        assert_eq!(resp.status().as_u16(), 200);
        let body: Value = resp.json().await.expect("state json");
        assert_eq!(body["key"], "dd");
        assert_eq!(body["receipts"], json!([]));
        assert_eq!(body["work_orders"].as_array().expect("orders").len(), 1);
        assert_eq!(body["work_orders"][0]["wo"], "WO-1");
        assert_eq!(body["work_orders"][0]["requester"], "sam");
        assert_eq!(body["work_orders"][0]["state"], "approved");
        assert_eq!(body["work_orders"][0]["attempts"], 1);
        for room in ["desk", "work"] {
            assert_eq!(body[room][0]["sender_name"], "viewer", "{room}");
            assert_eq!(body[room][0]["timestamp"], Value::Null, "{room}");
            let text = body[room][0]["body"].as_str().expect("error text");
            assert!(text.contains("couldn't read this room from Switch"), "{room}: {text}");
        }

        let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        stream.write_all(b"GET /api/state HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n\r\n").await.expect("write");
        let mut buf = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut buf)).await.expect("foreign host timed out").expect("read");
        let text = String::from_utf8_lossy(&buf);
        assert!(text.starts_with("HTTP/1.1 421"), "{text}");
        assert!(text.contains("open the viewer at http://127.0.0.1 or http://localhost"), "{text}");
    }

    /// The host gate is middleware on the whole router. `/api/state` is checked above; these are the other routes.
    #[tokio::test]
    async fn foreign_host_is_misdirected_on_every_other_route() {
        let (_tmp, view) = sample_desk();
        let (addr, _server) = serve(view).await;
        let evidence = format!("/api/evidence/ev_{}", "ab".repeat(32));
        for path in ["/", "/api/wo/WO-1", "/api/ledger/WO-1", evidence.as_str(), "/receipts/receipt-wo-1.json"] {
            let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
            let req = format!("GET {path} HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n\r\n");
            stream.write_all(req.as_bytes()).await.expect("write");
            let mut buf = Vec::new();
            tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut buf)).await.expect("foreign host timed out").expect("read");
            let text = String::from_utf8_lossy(&buf);
            assert!(text.starts_with("HTTP/1.1 421"), "{path}: {text}");
            assert!(text.contains("open the viewer at http://127.0.0.1 or http://localhost"), "{path}: {text}");
        }
    }

    #[tokio::test]
    async fn get_api_wo_returns_the_case_file_or_a_plain_error() {
        let (_tmp, view) = sample_desk();
        let (addr, _server) = serve(view).await;
        let http = client();

        let resp = http.get(format!("http://{addr}/api/wo/wo-1")).send().await.expect("GET /api/wo/wo-1");
        assert_eq!(resp.status().as_u16(), 200);
        let body: Value = resp.json().await.expect("case json");
        assert_eq!(body["wo"], "WO-1");
        assert_eq!(body["order"]["title"], "Ship the invoice export");
        assert_eq!(body["order"]["branch"], "wo-1");
        assert_eq!(body["order"]["created_ms"], json!(1_700_000_000_000_i64));
        assert_eq!(body["chain"], json!({"ok": true, "entries": 1}));
        assert_eq!(body["entries"][0]["seq"], 1);
        assert_eq!(body["entries"][0]["kind"], "policy");
        let records = body["records"].as_object().expect("records");
        assert_eq!(records.len(), 1);
        assert_eq!(records.values().next().expect("policy")["protocol"], "notary-intake/1");
        assert_eq!(body["codes"]["revision_limit"]["retryable"], false);
        assert_eq!(body["codes"]["not_an_approver"]["fixer"], "human");

        let missing = http.get(format!("http://{addr}/api/wo/WO-9")).send().await.expect("GET missing");
        assert_eq!(missing.status().as_u16(), 404);
        assert_eq!(missing.text().await.expect("404 body"), "no such work order");

        let bad = http.get(format!("http://{addr}/api/wo/WO-1a")).send().await.expect("GET bad id");
        assert_eq!(bad.status().as_u16(), 400);
        assert_eq!(bad.text().await.expect("400 body"), "not a work order");
    }

    #[tokio::test]
    async fn get_api_evidence_returns_the_record_or_a_plain_error() {
        let (_tmp, view) = sample_desk();
        let ledger_path = view.data_dir.join("ledger.db");
        let ledger = Ledger::open(&ledger_path).expect("ledger");
        let (id, _) = ledger.append("WO-1", RecordKind::Evidence, &json!({"result": {"failed": 1}})).expect("evidence");
        let policy = ledger.entries("WO-1").expect("entries").into_iter().find(|e| e.kind == RecordKind::Policy).expect("policy").record;
        drop(ledger);
        let (addr, _server) = serve(view).await;
        let http = client();

        let resp = http.get(format!("http://{addr}/api/evidence/{id}")).send().await.expect("GET /api/evidence");
        assert_eq!(resp.status().as_u16(), 200);
        let body: Value = resp.json().await.expect("evidence json");
        assert_eq!(body["id"], id);
        assert_eq!(body["record"], json!({"result": {"failed": 1}}));

        let unknown = format!("ev_{}", "ab".repeat(32));
        let missing = http.get(format!("http://{addr}/api/evidence/{unknown}")).send().await.expect("GET missing evidence");
        assert_eq!(missing.status().as_u16(), 404);
        assert_eq!(missing.text().await.expect("404 body"), "no such evidence");

        let bad = http.get(format!("http://{addr}/api/evidence/ev_123")).send().await.expect("GET short id");
        assert_eq!(bad.status().as_u16(), 400);
        assert_eq!(bad.text().await.expect("400 body"), "not evidence");

        let other = http.get(format!("http://{addr}/api/evidence/{policy}")).send().await.expect("GET policy id");
        assert_eq!(other.status().as_u16(), 400);
        assert_eq!(other.text().await.expect("policy body"), "not evidence");

        let db = Connection::open(&ledger_path).expect("ledger db");
        db.execute("UPDATE records SET bytes = ?1 WHERE id = ?2", (br#"{"result":{"failed":0}}"#.to_vec(), id.as_str())).expect("tamper");
        drop(db);
        let tampered = http.get(format!("http://{addr}/api/evidence/{id}")).send().await.expect("GET tampered evidence");
        assert_eq!(tampered.status().as_u16(), 409);
        assert_eq!(tampered.text().await.expect("409 body"), format!("integrity_mismatch: record {id} changed after it was written"));

        let (_tmp, mut view) = sample_desk();
        view.data_dir.push("missing");
        let (addr, _server) = serve(view).await;
        let down = http.get(format!("http://{addr}/api/evidence/{unknown}")).send().await.expect("GET unreadable ledger");
        assert_eq!(down.status().as_u16(), 503);
        let text = down.text().await.expect("503 body");
        assert!(text.starts_with("upstream_unavailable: sqlite:"), "{text}");
    }

    #[tokio::test]
    async fn get_api_ledger_returns_the_chain_or_a_plain_error() {
        let (_tmp, view) = sample_desk();
        let (addr, _server) = serve(view).await;
        let http = client();

        let resp = http.get(format!("http://{addr}/api/ledger/wo-1")).send().await.expect("GET /api/ledger/wo-1");
        assert_eq!(resp.status().as_u16(), 200);
        let body: Value = resp.json().await.expect("ledger json");
        assert_eq!(body["wo"], "WO-1");
        assert_eq!(body["chain"], "1 entries, chain intact");
        let line = body["lines"][0].as_str().expect("policy line");
        assert!(line.starts_with("1. `po_") && line.ends_with("` intake + Desk Policy"), "{line}");

        let empty = http.get(format!("http://{addr}/api/ledger/WO-9")).send().await.expect("GET empty ledger");
        assert_eq!(empty.status().as_u16(), 200);
        let body: Value = empty.json().await.expect("empty ledger json");
        assert_eq!(body["wo"], "WO-9");
        assert_eq!(body["chain"], "0 entries, chain intact");
        assert_eq!(body["lines"], json!([]));

        let bad = http.get(format!("http://{addr}/api/ledger/WO-1a")).send().await.expect("GET bad id");
        assert_eq!(bad.status().as_u16(), 400);
        assert_eq!(bad.text().await.expect("400 body"), "not a work order");

        let (_tmp, mut view) = sample_desk();
        view.data_dir.push("missing");
        let (addr, _server) = serve(view).await;
        let down = http.get(format!("http://{addr}/api/ledger/WO-1")).send().await.expect("GET unreadable ledger");
        assert_eq!(down.status().as_u16(), 503);
        let text = down.text().await.expect("503 body");
        assert!(text.starts_with("upstream_unavailable: sqlite:"), "{text}");
    }

    #[tokio::test]
    async fn get_receipt_file_downloads_the_bundle_or_404() {
        let payload = br#"{"protocol":"notary-receipt/1"}"#;
        let (_tmp, view) = sample_desk();
        let dir = view.data_dir.join("receipts");
        std::fs::create_dir_all(&dir).expect("receipts dir");
        std::fs::write(dir.join("receipt-wo-1.json"), payload).expect("receipt file");
        let (addr, _server) = serve(view).await;
        let http = client();

        let resp = http.get(format!("http://{addr}/receipts/receipt-wo-1.json")).send().await.expect("GET receipt");
        assert_eq!(resp.status().as_u16(), 200);
        assert_eq!(resp.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()), Some("application/json"));
        assert_eq!(resp.headers().get(header::CONTENT_DISPOSITION).and_then(|v| v.to_str().ok()), Some("attachment; filename=\"receipt-wo-1.json\""));
        assert_eq!(resp.bytes().await.expect("receipt bytes").as_ref(), payload);

        let missing = http.get(format!("http://{addr}/receipts/receipt-wo-9.json")).send().await.expect("GET missing receipt");
        assert_eq!(missing.status().as_u16(), 404);
        assert!(missing.bytes().await.expect("404 body").is_empty());

        let bad = http.get(format!("http://{addr}/receipts/receipt-wo-1.txt")).send().await.expect("GET bad name");
        assert_eq!(bad.status().as_u16(), 404);
        assert!(bad.bytes().await.expect("bad name body").is_empty());
    }

    #[test]
    fn only_local_hosts_are_answered() {
        for ok in ["127.0.0.1:8090", "localhost:8090", "localhost", "[::1]:8090", "127.0.0.1"] {
            assert!(super::local_host(ok), "{ok}");
        }
        for bad in ["evil.example", "evil.example:8090", "127.0.0.1.evil.example", "localhost.evil:8090", "", "[::2]:8090"] {
            assert!(!super::local_host(bad), "{bad}");
        }
    }

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
