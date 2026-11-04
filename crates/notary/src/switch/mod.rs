//! The Switch agent protocol, as the Notary uses it: HTTP out (messages, ops, documents,
//! media, approval cards) and one WebSocket in (every message in the Notary's rooms).
//! Shapes follow switch-core 0.31.0 (`bridges/agent/api/handlers.py`, `operations/definitions.py`,
//! `api/activity_routes.py`); there is no published SDK.

pub mod gateway;
pub mod ws;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Why a call to Switch failed, kept small: the status and the body's `detail`/`message`.
#[derive(Debug)]
pub struct SwitchError {
    pub status: Option<u16>,
    pub detail: String,
}

impl std::fmt::Display for SwitchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(s) => write!(f, "switch answered {s}: {}", self.detail),
            None => write!(f, "switch unreachable: {}", self.detail),
        }
    }
}

impl std::error::Error for SwitchError {}

impl From<reqwest::Error> for SwitchError {
    fn from(e: reqwest::Error) -> Self {
        Self { status: e.status().map(|s| s.as_u16()), detail: e.without_url().to_string() }
    }
}

pub type Result<T> = std::result::Result<T, SwitchError>;

/// The Notary's identity on one Switch server.
#[derive(Clone)]
pub struct Agent {
    pub base: String,
    pub id: String,
    key: String,
    http: reqwest::Client,
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent").field("base", &self.base).field("id", &self.id).finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ApprovalOption {
    pub id: String,
    pub label: String,
    /// `accept`, `acceptForSession`, `decline` or `cancel`.
    pub decision: String,
}

/// An approval card to open in `room`. `session` groups a work order's cards.
pub struct Card<'a> {
    pub session: &'a str,
    pub request: &'a str,
    pub room: &'a str,
    pub title: &'a str,
    pub detail: &'a str,
    pub options: &'a [ApprovalOption],
    /// ISO-8601, at most 24 hours ahead.
    pub expires_at: &'a str,
}

/// An answered or expired approval card. The HTTP view is `camelCase`, the frame `snake_case`.
#[derive(Debug, Clone, Deserialize)]
pub struct ApprovalOutcome {
    #[serde(alias = "sessionId")]
    pub session_id: String,
    #[serde(alias = "requestId")]
    pub request_id: String,
    pub state: String,
    #[serde(default)]
    pub answer: Option<String>,
    #[serde(default, alias = "answeredBy")]
    pub answered_by: Option<String>,
}

impl Agent {
    /// # Panics
    /// If the TLS backend can't be initialised (then no HTTP is possible at all).
    #[must_use]
    pub fn new(base: &str, id: &str, key: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("an HTTP client with default settings always builds");
        Self { base: base.trim_end_matches('/').to_string(), id: id.to_string(), key: key.to_string(), http }
    }

    pub(crate) fn key(&self) -> &str {
        &self.key
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<Value> {
        let resp = req.bearer_auth(&self.key).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            let detail = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("detail").or_else(|| v.get("message")).map(Value::to_string))
                .unwrap_or(text);
            return Err(SwitchError { status: Some(status.as_u16()), detail: clip(&detail, 400) });
        }
        if text.is_empty() {
            Ok(Value::Null)
        } else {
            serde_json::from_str(&text).map_err(|e| SwitchError { status: Some(status.as_u16()), detail: e.to_string() })
        }
    }

    /// Post `content` in `room` as the Notary. `@name` in the content addresses that agent.
    ///
    /// # Errors
    /// 403 if the Notary isn't in the room, 404 if there is no such room.
    pub async fn post(&self, room: &str, content: &str) -> Result<Value> {
        self.send(self.http.post(format!("{}/agents/{}/message", self.base, self.id)).json(&json!({"room_id": room, "content": content}))).await
    }

    /// Call operation `name`. Room-bound operations need `connection` (after `connect_to_room`).
    ///
    /// # Errors
    /// The operation's refusal.
    pub async fn op(&self, name: &str, args: &Value, connection: Option<&str>) -> Result<Value> {
        let mut req = self.http.post(format!("{}/agents/{}/ops/{name}", self.base, self.id)).json(args);
        if let Some(c) = connection {
            req = req.header("X-Switch-Connection-Id", c);
        }
        let v = self.send(req).await?;
        Ok(v.get("result").cloned().unwrap_or(v))
    }

    /// Attach a file to a message in `room`.
    ///
    /// # Errors
    /// If the upload is refused (empty, over 20 MiB, not a member).
    pub async fn upload(&self, room: &str, filename: &str, mime: &str, bytes: Vec<u8>, caption: &str) -> Result<Value> {
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename.to_string())
            .mime_str(mime)
            .map_err(|e| SwitchError { status: None, detail: e.to_string() })?;
        let form = reqwest::multipart::Form::new().part("file", part).text("caption", caption.to_string());
        self.send(self.http.post(format!("{}/agents/{}/rooms/{room}/media", self.base, self.id)).multipart(form)).await
    }

    /// Open an approval card. Ids are ours (any stable string); retrying the same request is
    /// idempotent. The Notary must hold a live connection or the card can't be answered.
    ///
    /// # Errors
    /// 409 if the same ids were used with different content, 403/404 if not a room member.
    pub async fn open_approval(&self, card: &Card<'_>) -> Result<Value> {
        let body = json!({
            "request_id": card.request, "turn_id": card.request, "kind": "approval",
            "title": clip(card.title, 500), "detail": clip(card.detail, 4000), "options": card.options,
            "questions": [], "room_id": card.room, "thread_id": null, "expires_at": card.expires_at,
        });
        self.send(self.http.post(format!("{}/agent-sessions/{}/approvals", self.base, card.session)).json(&body)).await
    }

    /// Answered or expired cards not yet acknowledged, oldest first.
    ///
    /// # Errors
    /// If Switch can't be read.
    pub async fn outcomes(&self) -> Result<Vec<ApprovalOutcome>> {
        let v = self.send(self.http.get(format!("{}/agent-sessions/approvals/outcomes", self.base))).await?;
        let list = v.get("outcomes").cloned().unwrap_or(v);
        serde_json::from_value(list).map_err(|e| SwitchError { status: None, detail: format!("unexpected outcomes shape: {e}") })
    }

    /// Acknowledge an outcome so it isn't delivered again.
    ///
    /// # Errors
    /// 409 while the request is still open.
    pub async fn delivered(&self, session: &str, request: &str) -> Result<Value> {
        self.send(self.http.post(format!("{}/agent-sessions/{session}/approvals/{request}/delivered", self.base))).await
    }

    /// Who is in `room`: `[{type, agent_id, name, status}]`.
    ///
    /// # Errors
    /// If Switch can't be read.
    pub async fn participants(&self, room: &str) -> Result<Value> {
        self.send(self.http.get(format!("{}/agents/rooms/{room}/participants", self.base))).await
    }
}

/// At most `max` bytes of `s`, cut at a character boundary.
#[must_use]
pub fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}
