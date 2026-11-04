//! Configuration, read once from the environment (`make run` loads infra/.env and
//! infra/notary.env). Modes have no silent defaults: an unset or unknown value is a
//! `configuration_invalid` error naming the variable.

use std::path::PathBuf;
use std::time::Duration;

use notary_core::errors::{Code, NotaryError};

use crate::sandbox::Engine;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Approval {
    /// A Switch approval card with buttons.
    Card,
    /// `approve WO-n` typed in the hub.
    Text,
}

pub struct Config {
    pub switch_url: String,
    pub repo_url: String,
    pub repo_name: String,
    pub hub_room: String,
    pub work_room: String,
    pub builder: String,
    /// `None`: no review stage.
    pub reviewer: Option<String>,
    pub engine: Engine,
    pub image: String,
    pub test_timeout: Duration,
    /// Gateway login for reading tool activity; `None` means activity reading is off.
    pub activity: Option<(String, String)>,
    pub approval: Approval,
    pub data_dir: PathBuf,
    pub state_dir: PathBuf,
}

fn var(name: &str) -> Result<String, NotaryError> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| NotaryError::new(Code::ConfigurationInvalid, format!("{name} is not set")))
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

impl Config {
    /// # Errors
    /// `configuration_invalid` naming the first bad variable.
    pub fn from_env() -> Result<Self, NotaryError> {
        let bad = |name: &str, why: &str| NotaryError::new(Code::ConfigurationInvalid, format!("{name}: {why}"));
        let engine = match var("NOTARY_SANDBOX_ENGINE")?.as_str() {
            "podman" => Engine::Podman,
            "docker" => Engine::Docker,
            _ => return Err(bad("NOTARY_SANDBOX_ENGINE", "must be podman or docker")),
        };
        let approval = match var("NOTARY_APPROVAL")?.as_str() {
            "card" => Approval::Card,
            "text" => Approval::Text,
            _ => return Err(bad("NOTARY_APPROVAL", "must be card or text")),
        };
        let activity = match var("NOTARY_ACTIVITY")?.as_str() {
            "on" => Some((var("GATEWAY_ADMIN_EMAIL")?, var("GATEWAY_ADMIN_PASSWORD")?)),
            "off" => None,
            _ => return Err(bad("NOTARY_ACTIVITY", "must be on or off")),
        };
        let image = var("NOTARY_SANDBOX_IMAGE")?.trim_start_matches("sha256:").to_string();
        if image.len() != 64 || !image.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(bad("NOTARY_SANDBOX_IMAGE", "must be the image id (64 hex) from make sandbox-image"));
        }
        let test_timeout = match std::env::var("NOTARY_TEST_TIMEOUT_S") {
            Ok(v) => Duration::from_secs(v.parse().map_err(|_| bad("NOTARY_TEST_TIMEOUT_S", "must be whole seconds"))?),
            Err(_) => Duration::from_secs(300),
        };
        let repo_url = var("NOTARY_REPO_URL")?;
        let repo_name = repo_url.trim_end_matches('/').trim_end_matches(".git").rsplit('/').next().unwrap_or("repo").to_string();
        let data_dir = std::env::var_os("NOTARY_DATA_DIR").map_or_else(|| home().join(".local/share/agora-notary"), PathBuf::from);
        let state_dir = std::env::var_os("XDG_CONFIG_HOME").map_or_else(|| home().join(".config"), PathBuf::from).join("agora-notary");
        Ok(Self {
            switch_url: std::env::var("NOTARY_SWITCH_URL").unwrap_or_else(|_| "http://127.0.0.1:8000".into()),
            repo_url,
            repo_name,
            hub_room: var("NOTARY_HUB_ROOM")?,
            work_room: var("NOTARY_WORK_ROOM")?,
            builder: var("NOTARY_BUILDER")?,
            reviewer: std::env::var("NOTARY_REVIEWER").ok().filter(|v| !v.is_empty() && v != "none"),
            engine,
            image,
            test_timeout,
            activity,
            approval,
            data_dir,
            state_dir,
        })
    }
}
