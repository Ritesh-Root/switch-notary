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
    /// Where the read-only viewer listens; `None` when `NOTARY_VIEW_ADDR=off`.
    pub view_addr: Option<std::net::SocketAddr>,
}

fn var(get: &impl Fn(&str) -> Option<String>, name: &str) -> Result<String, NotaryError> {
    get(name).filter(|v| !v.trim().is_empty()).ok_or_else(|| NotaryError::new(Code::ConfigurationInvalid, format!("{name} is not set")))
}

fn home(get_os: &impl Fn(&str) -> Option<std::ffi::OsString>) -> PathBuf {
    PathBuf::from(get_os("HOME").unwrap_or_default())
}

impl Config {
    /// # Errors
    /// `configuration_invalid` naming the first bad variable.
    pub fn from_env() -> Result<Self, NotaryError> {
        Self::from_lookup(|name| std::env::var(name).ok(), |name| std::env::var_os(name))
    }

    /// Same rules as [`Self::from_env`], reading from the supplied lookups so tests don't touch
    /// the process environment (`std::env::set_var` is `unsafe`, and this crate forbids unsafe).
    fn from_lookup(get: impl Fn(&str) -> Option<String>, get_os: impl Fn(&str) -> Option<std::ffi::OsString>) -> Result<Self, NotaryError> {
        let bad = |name: &str, why: &str| NotaryError::new(Code::ConfigurationInvalid, format!("{name}: {why}"));
        let engine = match var(&get, "NOTARY_SANDBOX_ENGINE")?.as_str() {
            "podman" => Engine::Podman,
            "docker" => Engine::Docker,
            _ => return Err(bad("NOTARY_SANDBOX_ENGINE", "must be podman or docker")),
        };
        let approval = match var(&get, "NOTARY_APPROVAL")?.as_str() {
            "card" => Approval::Card,
            "text" => Approval::Text,
            _ => return Err(bad("NOTARY_APPROVAL", "must be card or text")),
        };
        let activity = match var(&get, "NOTARY_ACTIVITY")?.as_str() {
            "on" => Some((var(&get, "GATEWAY_ADMIN_EMAIL")?, var(&get, "GATEWAY_ADMIN_PASSWORD")?)),
            "off" => None,
            _ => return Err(bad("NOTARY_ACTIVITY", "must be on or off")),
        };
        let image = var(&get, "NOTARY_SANDBOX_IMAGE")?.trim_start_matches("sha256:").to_string();
        if image.len() != 64 || !image.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(bad("NOTARY_SANDBOX_IMAGE", "must be the image id (64 hex) from make sandbox-image"));
        }
        let test_timeout = match get("NOTARY_TEST_TIMEOUT_S") {
            Some(v) => Duration::from_secs(v.parse().map_err(|_| bad("NOTARY_TEST_TIMEOUT_S", "must be whole seconds"))?),
            None => Duration::from_secs(300),
        };
        let repo_url = var(&get, "NOTARY_REPO_URL")?;
        let repo_name = repo_url.trim_end_matches('/').trim_end_matches(".git").rsplit('/').next().unwrap_or("repo").to_string();
        let data_dir = get_os("NOTARY_DATA_DIR").map_or_else(|| home(&get_os).join(".local/share/switch-notary"), PathBuf::from);
        let state_dir = get_os("XDG_CONFIG_HOME").map_or_else(|| home(&get_os).join(".config"), PathBuf::from).join("switch-notary");
        let view_addr = match get("NOTARY_VIEW_ADDR").as_deref() {
            Some("off") => None,
            Some(v) => Some(v.parse().map_err(|_| bad("NOTARY_VIEW_ADDR", "must be host:port (e.g. 127.0.0.1:8090) or off"))?),
            None => Some(std::net::SocketAddr::from(([127, 0, 0, 1], 8090))),
        };
        Ok(Self {
            switch_url: get("NOTARY_SWITCH_URL").unwrap_or_else(|| "http://127.0.0.1:8000".into()),
            repo_url,
            repo_name,
            hub_room: var(&get, "NOTARY_HUB_ROOM")?,
            work_room: var(&get, "NOTARY_WORK_ROOM")?,
            builder: var(&get, "NOTARY_BUILDER")?,
            reviewer: get("NOTARY_REVIEWER").filter(|v| !v.is_empty() && v != "none"),
            engine,
            image,
            test_timeout,
            activity,
            approval,
            data_dir,
            state_dir,
            view_addr,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMAGE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const BASE: &[(&str, &str)] = &[
        ("NOTARY_SANDBOX_ENGINE", "podman"),
        ("NOTARY_APPROVAL", "card"),
        ("NOTARY_ACTIVITY", "off"),
        ("NOTARY_SANDBOX_IMAGE", IMAGE),
        ("NOTARY_REPO_URL", "https://git.example/acme/invoices.git"),
        ("NOTARY_HUB_ROOM", "acme-desk"),
        ("NOTARY_WORK_ROOM", "acme-work"),
        ("NOTARY_BUILDER", "builder"),
        ("NOTARY_DATA_DIR", "/var/lib/notary"),
        ("XDG_CONFIG_HOME", "/var/lib/notary-config"),
        ("HOME", "/home/notary"),
    ];

    /// `drop` removes base keys; `overrides` replace or add. Later keys win.
    fn parse(drop: &[&str], overrides: &[(&str, &str)]) -> Result<Config, NotaryError> {
        let mut pairs: Vec<(&str, &str)> = BASE.iter().copied().filter(|(k, _)| !drop.contains(k)).collect();
        pairs.extend_from_slice(overrides);
        let map: std::collections::BTreeMap<&str, &str> = pairs.into_iter().collect();
        Config::from_lookup(|name| map.get(name).map(|v| (*v).to_string()), |name| map.get(name).map(|v| std::ffi::OsString::from(*v)))
    }

    fn bad(drop: &[&str], overrides: &[(&str, &str)]) -> NotaryError {
        let Err(err) = parse(drop, overrides) else {
            panic!("expected configuration_invalid");
        };
        assert_eq!(err.code, Code::ConfigurationInvalid);
        err
    }

    #[test]
    fn a_full_environment_parses() {
        let cfg = parse(&[], &[]).unwrap();
        assert_eq!(cfg.engine, Engine::Podman);
        assert_eq!(cfg.approval, Approval::Card);
        assert_eq!(cfg.activity, None);
        assert_eq!(cfg.image, IMAGE);
        assert_eq!(cfg.test_timeout, Duration::from_secs(300));
        assert_eq!(cfg.repo_url, "https://git.example/acme/invoices.git");
        assert_eq!(cfg.repo_name, "invoices");
        assert_eq!(cfg.hub_room, "acme-desk");
        assert_eq!(cfg.work_room, "acme-work");
        assert_eq!(cfg.builder, "builder");
        assert_eq!(cfg.reviewer, None);
        assert_eq!(cfg.switch_url, "http://127.0.0.1:8000");
        assert_eq!(cfg.data_dir, PathBuf::from("/var/lib/notary"));
        assert_eq!(cfg.state_dir, PathBuf::from("/var/lib/notary-config/switch-notary"));
        assert_eq!(cfg.view_addr, Some(std::net::SocketAddr::from(([127, 0, 0, 1], 8090))));

        let upper = "ABCDEF0123456789".repeat(4);
        let cfg = parse(
            &[],
            &[
                ("NOTARY_SANDBOX_ENGINE", "docker"),
                ("NOTARY_APPROVAL", "text"),
                ("NOTARY_ACTIVITY", "on"),
                ("GATEWAY_ADMIN_EMAIL", "ops@example.com"),
                ("GATEWAY_ADMIN_PASSWORD", "pw"),
                ("NOTARY_SANDBOX_IMAGE", &format!("sha256:{upper}")),
                ("NOTARY_TEST_TIMEOUT_S", "15"),
                ("NOTARY_REPO_URL", "https://git.example/acme/invoices.git/"),
                ("NOTARY_REVIEWER", "priya"),
                ("NOTARY_SWITCH_URL", "http://switch:8000"),
                ("NOTARY_VIEW_ADDR", "[::1]:8090"),
            ],
        )
        .unwrap();
        assert_eq!(cfg.engine, Engine::Docker);
        assert_eq!(cfg.approval, Approval::Text);
        assert_eq!(cfg.activity.as_ref().map(|(e, p)| (e.as_str(), p.as_str())), Some(("ops@example.com", "pw")));
        assert_eq!(cfg.image, upper);
        assert_eq!(cfg.test_timeout, Duration::from_secs(15));
        assert_eq!(cfg.repo_name, "invoices");
        assert_eq!(cfg.reviewer.as_deref(), Some("priya"));
        assert_eq!(cfg.switch_url, "http://switch:8000");
        assert_eq!(cfg.view_addr, Some("[::1]:8090".parse().unwrap()));
    }

    #[test]
    fn defaults_cover_only_the_optional_settings() {
        let cfg = parse(
            &["NOTARY_DATA_DIR", "XDG_CONFIG_HOME"],
            &[("NOTARY_REVIEWER", "none"), ("NOTARY_VIEW_ADDR", "off"), ("NOTARY_REPO_URL", "invoices.git")],
        )
        .unwrap();
        assert_eq!(cfg.data_dir, PathBuf::from("/home/notary/.local/share/switch-notary"));
        assert_eq!(cfg.state_dir, PathBuf::from("/home/notary/.config/switch-notary"));
        assert_eq!(cfg.view_addr, None);
        assert_eq!(cfg.reviewer, None);
        assert_eq!(cfg.repo_name, "invoices");

        let cfg = parse(
            &["NOTARY_DATA_DIR", "XDG_CONFIG_HOME", "HOME"],
            &[("NOTARY_REVIEWER", ""), ("NOTARY_SWITCH_URL", ""), ("NOTARY_REPO_URL", "https://git.example/acme/invoices")],
        )
        .unwrap();
        assert_eq!(cfg.data_dir, PathBuf::from(".local/share/switch-notary"));
        assert_eq!(cfg.state_dir, PathBuf::from(".config/switch-notary"));
        assert_eq!(cfg.reviewer, None);
        assert_eq!(cfg.switch_url, "");
        assert_eq!(cfg.repo_name, "invoices");
        assert_eq!(parse(&[], &[("NOTARY_TEST_TIMEOUT_S", "0")]).unwrap().test_timeout, Duration::from_secs(0));
    }

    #[test]
    fn blank_or_unknown_values_are_configuration_errors() {
        let cases = [
            (bad(&["NOTARY_SANDBOX_ENGINE"], &[]).detail, "NOTARY_SANDBOX_ENGINE is not set"),
            (bad(&[], &[("NOTARY_SANDBOX_ENGINE", "   ")]).detail, "NOTARY_SANDBOX_ENGINE is not set"),
            (
                bad(&[], &[("NOTARY_SANDBOX_ENGINE", " podman"), ("NOTARY_APPROVAL", "nope")]).detail,
                "NOTARY_SANDBOX_ENGINE: must be podman or docker",
            ),
            (bad(&[], &[("NOTARY_APPROVAL", "email")]).detail, "NOTARY_APPROVAL: must be card or text"),
            (bad(&[], &[("NOTARY_ACTIVITY", "yes")]).detail, "NOTARY_ACTIVITY: must be on or off"),
            (bad(&[], &[("NOTARY_ACTIVITY", "on")]).detail, "GATEWAY_ADMIN_EMAIL is not set"),
            (
                bad(&[], &[("NOTARY_ACTIVITY", "on"), ("GATEWAY_ADMIN_EMAIL", "ops@example.com"), ("GATEWAY_ADMIN_PASSWORD", " ")]).detail,
                "GATEWAY_ADMIN_PASSWORD is not set",
            ),
            (bad(&[], &[("NOTARY_SANDBOX_IMAGE", "abc")]).detail, "NOTARY_SANDBOX_IMAGE: must be the image id (64 hex) from make sandbox-image"),
            (
                bad(&[], &[("NOTARY_SANDBOX_IMAGE", &"g".repeat(64))]).detail,
                "NOTARY_SANDBOX_IMAGE: must be the image id (64 hex) from make sandbox-image",
            ),
            (
                bad(&[], &[("NOTARY_SANDBOX_IMAGE", &format!("SHA256:{IMAGE}"))]).detail,
                "NOTARY_SANDBOX_IMAGE: must be the image id (64 hex) from make sandbox-image",
            ),
            (bad(&[], &[("NOTARY_TEST_TIMEOUT_S", "")]).detail, "NOTARY_TEST_TIMEOUT_S: must be whole seconds"),
            (bad(&[], &[("NOTARY_TEST_TIMEOUT_S", " 10")]).detail, "NOTARY_TEST_TIMEOUT_S: must be whole seconds"),
            (bad(&[], &[("NOTARY_TEST_TIMEOUT_S", "10x")]).detail, "NOTARY_TEST_TIMEOUT_S: must be whole seconds"),
            (bad(&[], &[("NOTARY_REPO_URL", "  ")]).detail, "NOTARY_REPO_URL is not set"),
            (bad(&["NOTARY_HUB_ROOM"], &[]).detail, "NOTARY_HUB_ROOM is not set"),
            (bad(&[], &[("NOTARY_BUILDER", "  ")]).detail, "NOTARY_BUILDER is not set"),
            (bad(&[], &[("NOTARY_VIEW_ADDR", "localhost:8090")]).detail, "NOTARY_VIEW_ADDR: must be host:port (e.g. 127.0.0.1:8090) or off"),
            (bad(&[], &[("NOTARY_VIEW_ADDR", "OFF")]).detail, "NOTARY_VIEW_ADDR: must be host:port (e.g. 127.0.0.1:8090) or off"),
        ];
        for (got, want) in cases {
            assert_eq!(got, want);
        }
    }
}
