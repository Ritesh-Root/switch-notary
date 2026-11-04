//! The Notary's one WebSocket: every message in every room it belongs to (`scope=all`,
//! `filter=all`), plus approval outcomes (protocol revision 4+).
//!
//! Pings are answered on the reader task itself and events go out over an unbounded
//! channel, so a slow consumer (a 60 s sandboxed test run) can never let the heartbeat lapse.
//! Reconnects reuse the connection id and resume from the furthest position seen.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{Message as WsMessage, client::IntoClientRequest as _, http::HeaderValue};

use super::{Agent, ApprovalOutcome};

/// A message someone posted in one of the Notary's rooms.
#[derive(Debug, Clone)]
pub struct RoomMessage {
    pub room_id: String,
    pub message_id: String,
    pub body: String,
    /// The agent's name for agents, the person's name for people.
    pub sender_name: String,
    /// `user`, `agent`, `platform`, or empty.
    pub sender_kind: String,
    pub addressed: bool,
}

#[derive(Debug, Clone)]
pub enum Event {
    /// (Re)connected.
    Connected,
    Message(RoomMessage),
    Approval(ApprovalOutcome),
    /// Events were missed in these rooms: re-read them.
    Gap {
        rooms: Vec<String>,
    },
    /// Another connection took over, or the server ended this one for good. Stop.
    Stopped {
        reason: String,
    },
}

/// Start the stream. Events arrive on the returned channel until [`Event::Stopped`].
#[must_use]
pub fn spawn(agent: Agent, connection_id: String) -> mpsc::UnboundedReceiver<Event> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut position: BTreeMap<String, u64> = BTreeMap::new();
        let mut generation: Option<u64> = None;
        let mut backoff = Duration::from_secs(1);
        loop {
            let started = std::time::Instant::now();
            match run_once(&agent, &connection_id, &mut position, &mut generation, &tx).await {
                Ok(Some(reason)) => {
                    let _ = tx.send(Event::Stopped { reason });
                    return;
                }
                Ok(None) => eprintln!("notary: stream closed; reconnecting"),
                Err(e) => eprintln!("notary: stream error: {e}; reconnecting in {}s", backoff.as_secs()),
            }
            if tx.is_closed() {
                return;
            }
            if started.elapsed() > Duration::from_secs(30) {
                backoff = Duration::from_secs(1);
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    });
    rx
}

/// One connection, until it closes. `Ok(Some(reason))` means stop for good.
async fn run_once(
    agent: &Agent,
    connection_id: &str,
    position: &mut BTreeMap<String, u64>,
    generation: &mut Option<u64>,
    tx: &mpsc::UnboundedSender<Event>,
) -> Result<Option<String>, String> {
    let start_at = if position.is_empty() { "head".to_string() } else { serde_json::to_string(&position).map_err(|e| e.to_string())? };
    let ws_base = agent.base.replacen("http://", "ws://", 1).replacen("https://", "wss://", 1);
    let mut url = format!(
        "{ws_base}/agents/{}/connection/ws?connection_id={}&scope=all&filter=all&protocol=9&protocol_accepts=4&client=agora-notary&client_version={}&start_at={}",
        agent.id,
        enc(connection_id),
        env!("CARGO_PKG_VERSION"),
        enc(&start_at)
    );
    if let Some(g) = generation {
        let _ = write!(url, "&expected_generation={g}");
    }
    let mut request = url.into_client_request().map_err(|e| e.to_string())?;
    let auth = HeaderValue::from_str(&format!("Bearer {}", agent.key())).map_err(|e| e.to_string())?;
    request.headers_mut().insert("Authorization", auth);
    let (socket, _) = tokio_tungstenite::connect_async(request).await.map_err(|e| e.to_string())?;
    let (mut sink, mut stream) = socket.split();

    while let Some(frame) = stream.next().await {
        let text = match frame.map_err(|e| e.to_string())? {
            WsMessage::Text(t) => t.to_string(),
            WsMessage::Ping(p) => {
                sink.send(WsMessage::Pong(p)).await.map_err(|e| e.to_string())?;
                continue;
            }
            WsMessage::Close(c) => {
                let code = c.as_ref().map(|c| u16::from(c.code)).unwrap_or_default();
                if code == 4401 {
                    return Ok(Some("the agent key was refused (4401); run scripts/register.sh".into()));
                }
                return Ok(None);
            }
            _ => continue,
        };
        let Ok(frame) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let event = frame.get("event").and_then(Value::as_str).unwrap_or_default();
        let data = frame.get("data").cloned().unwrap_or(Value::Null);
        merge_position(position, data.get("position"));
        match event {
            "ping" => {
                // The server ignores the cursor; it must still be a number.
                let pong = serde_json::json!({"type": "pong", "cursor": 0}).to_string();
                sink.send(WsMessage::Text(pong.into())).await.map_err(|e| e.to_string())?;
            }
            "connection_state" => {
                *generation = data.get("generation").and_then(Value::as_u64);
                let _ = tx.send(Event::Connected);
            }
            "message" => {
                if let Some(m) = room_message(&data) {
                    let _ = tx.send(Event::Message(m));
                }
            }
            "approval_outcome" => {
                if let Ok(o) = serde_json::from_value::<ApprovalOutcome>(data) {
                    let _ = tx.send(Event::Approval(o));
                }
            }
            "gap" => {
                if let Some(p) = data.get("resumed_position").and_then(Value::as_object) {
                    position.clear();
                    position.extend(p.iter().filter_map(|(k, v)| v.as_u64().map(|n| (k.clone(), n))));
                }
                let _ = tx.send(Event::Gap { rooms: strings(data.get("rooms")) });
            }
            "evicted" => {
                let code = data.get("code").and_then(Value::as_str).unwrap_or_default();
                if code == "taken_over" {
                    return Ok(Some("another connection took over this agent".into()));
                }
                return Ok(None);
            }
            "refused" => {
                return Err(format!("connection refused: {}", data.get("detail").map(Value::to_string).unwrap_or_default()));
            }
            _ => {}
        }
    }
    Ok(None)
}

fn room_message(data: &Value) -> Option<RoomMessage> {
    let p = data.get("payload")?;
    let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    Some(RoomMessage {
        room_id: data.get("room_id")?.as_str()?.to_string(),
        message_id: s("message_id"),
        body: s("body"),
        sender_name: s("sender_name"),
        sender_kind: s("sender_kind"),
        addressed: p.get("addressed").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn merge_position(position: &mut BTreeMap<String, u64>, seen: Option<&Value>) {
    if let Some(obj) = seen.and_then(Value::as_object) {
        for (k, v) in obj {
            if let Some(n) = v.as_u64() {
                let e = position.entry(k.clone()).or_insert(0);
                *e = (*e).max(n);
            }
        }
    }
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()
}

/// Percent-encode a query value.
fn enc(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}
