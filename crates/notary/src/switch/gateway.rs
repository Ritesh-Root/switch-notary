//! The one thing the Notary reads as a person: Switch's tool-activity log for the work room.
//! Console's session host writes a row per tool call (title = the command or `Edit <path>`,
//! status completed/failed). Rows are corroboration only: they are reported with each agent's
//! own key, whose file sits in that agent's working directory.

use serde::Deserialize;
use serde_json::json;

use super::{Result, SwitchError};

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ActivityRow {
    pub agent_id: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub turn_id: String,
    #[serde(default)]
    pub item_id: String,
    pub kind: String,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub occurred_at: String,
}

pub struct Gateway {
    base: String,
    http: reqwest::Client,
    email: String,
    password: String,
}

impl Gateway {
    /// # Panics
    /// If the HTTP client can't be built.
    #[must_use]
    pub fn new(base: &str, email: &str, password: &str) -> Self {
        let http = reqwest::Client::builder()
            .cookie_store(true)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("an HTTP client with a cookie store always builds");
        Self {
            base: base.trim_end_matches('/').to_string(),
            http,
            email: email.to_string(),
            password: password.to_string(),
        }
    }

    /// # Errors
    /// Wrong credentials or Switch unreachable.
    pub async fn login(&self) -> Result<()> {
        let resp = self
            .http
            .post(format!("{}/gateway/auth/login", self.base))
            .json(&json!({"email": self.email, "password": self.password}))
            .send()
            .await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(SwitchError {
                status: Some(resp.status().as_u16()),
                detail: "gateway login refused".into(),
            })
        }
    }

    /// Activity rows updated in `room` over the last 7 days (no paging in 0.31.0).
    /// Logs in again once if the cookie expired.
    ///
    /// # Errors
    /// 403 if the gateway user's member client isn't in the room (scripts/install.sh adds it).
    pub async fn activity(&self, room: &str) -> Result<Vec<ActivityRow>> {
        for attempt in 0..2 {
            let resp = self
                .http
                .get(format!("{}/gateway/chats/{room}/activity", self.base))
                .send()
                .await?;
            if resp.status().as_u16() == 401 && attempt == 0 {
                self.login().await?;
                continue;
            }
            if !resp.status().is_success() {
                return Err(SwitchError {
                    status: Some(resp.status().as_u16()),
                    detail: "activity read refused".into(),
                });
            }
            let v: serde_json::Value = resp.json().await?;
            let items = v.get("items").cloned().unwrap_or_default();
            return serde_json::from_value(items).map_err(|e| SwitchError {
                status: None,
                detail: format!("unexpected activity shape: {e}"),
            });
        }
        Err(SwitchError {
            status: Some(401),
            detail: "gateway session expired twice".into(),
        })
    }
}
