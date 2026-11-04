//! `notary`: the Agora Notary agent.
//!
//! - `notary run`: join Switch and run the work-order desk.
//! - `notary doctor`: check every dependency and say what's wrong, with a code.

mod config;
mod desk;
mod git;
mod sandbox;
mod switch;
mod viewer;

use std::path::{Path, PathBuf};

use notary_core::errors::{Code, NotaryError};
use serde_json::Value;

fn load_agent(cfg: &config::Config) -> Result<switch::Agent, NotaryError> {
    let path = cfg.state_dir.join("agent.json");
    let bad = |why: String| NotaryError::new(Code::ConfigurationInvalid, why);
    let text = std::fs::read_to_string(&path).map_err(|e| bad(format!("can't read {}: {e} (run scripts/register.sh)", path.display())))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| bad(format!("{} isn't JSON: {e}", path.display())))?;
    let field = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string).ok_or_else(|| bad(format!("{} has no {k}", path.display())));
    Ok(switch::Agent::new(&cfg.switch_url, &field("id")?, &field("api_key")?))
}

/// The connection id is kept across restarts, so a restart takes over the old connection.
fn connection_id(state_dir: &Path) -> String {
    let path: PathBuf = state_dir.join("connection_id");
    if let Ok(id) = std::fs::read_to_string(&path)
        && !id.trim().is_empty()
    {
        return id.trim().to_string();
    }
    let id = format!("notary-{}", notary_core::signing::random_hex(8));
    let _ = std::fs::write(&path, &id);
    id
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("run") => run().await,
        Some("doctor") => doctor().await,
        _ => {
            eprintln!("usage: notary run | notary doctor");
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("notary: {e}");
        eprintln!("        {}", e.code.meta().remediation);
        std::process::exit(1);
    }
}

async fn run() -> Result<(), NotaryError> {
    let cfg = config::Config::from_env()?;
    std::fs::create_dir_all(&cfg.state_dir).map_err(|e| NotaryError::new(Code::ConfigurationInvalid, e.to_string()))?;
    let agent = load_agent(&cfg)?;
    let connection = connection_id(&cfg.state_dir);
    let view = cfg.view_addr.map(|addr| (addr, cfg.hub_room.clone(), cfg.work_room.clone(), cfg.data_dir.clone()));
    let mut desk = desk::Desk::open(cfg, agent.clone(), connection.clone())?;
    eprintln!("notary: running as agent {} (signing key {})", agent.id, desk.public_key());
    if let Some((addr, hub, work, data_dir)) = view {
        viewer::spawn(addr, viewer::View { agent: agent.clone(), hub, work, data_dir, key: desk.public_key() });
    }
    let mut events = switch::ws::spawn(agent.clone(), connection);
    while let Some(event) = events.recv().await {
        match event {
            switch::ws::Event::Connected => {
                eprintln!("notary: connected to Switch");
                // Approvals answered while we were away.
                match agent.outcomes().await {
                    Ok(list) => {
                        for o in list {
                            desk.on_outcome(o).await;
                        }
                    }
                    Err(e) => eprintln!("notary: couldn't read pending approvals: {e}"),
                }
            }
            switch::ws::Event::Message(m) => desk.on_message(m).await,
            switch::ws::Event::Approval(o) => desk.on_outcome(o).await,
            switch::ws::Event::Gap { rooms } => eprintln!("notary: missed events in {rooms:?}"),
            switch::ws::Event::Stopped { reason } => {
                return Err(NotaryError::new(Code::UpstreamUnavailable, reason));
            }
        }
    }
    Ok(())
}

async fn doctor() -> Result<(), NotaryError> {
    let mut failed = 0;
    let mut report = |name: &str, r: Result<String, NotaryError>| match r {
        Ok(note) => println!("ok    {name}{}", if note.is_empty() { String::new() } else { format!(": {note}") }),
        Err(e) => {
            failed += 1;
            println!("FAIL  {name}: {} ({})\n      {}", e.code, e.detail, e.code.meta().remediation);
        }
    };
    let cfg = match config::Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            report("config", Err(e));
            return Err(NotaryError::new(Code::ConfigurationInvalid, "fix the configuration first"));
        }
    };
    report(
        "config",
        Ok(format!("sandbox {:?}, approval {:?}, activity {}", cfg.engine, cfg.approval, if cfg.activity.is_some() { "on" } else { "off" })),
    );
    let up = |e: switch::SwitchError| NotaryError::new(Code::UpstreamUnavailable, e.to_string());
    let health = reqwest::get(format!("{}/health", cfg.switch_url)).await.map_err(|e| NotaryError::new(Code::UpstreamUnavailable, e.to_string()));
    report("switch", health.map(|r| r.status().to_string()));
    match load_agent(&cfg) {
        Ok(agent) => {
            for (name, room) in [("hub room", &cfg.hub_room), ("work room", &cfg.work_room)] {
                let r = agent.participants(room).await.map_err(up).and_then(|p| {
                    let names: Vec<String> = p
                        .get("participants")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|x| x.get("name").and_then(Value::as_str).map(str::to_string))
                        .collect();
                    if names.iter().any(|n| n == "agora-notary") {
                        Ok(names.join(", "))
                    } else {
                        Err(NotaryError::new(Code::ConfigurationInvalid, format!("agora-notary isn't in it ({})", names.join(", "))))
                    }
                });
                report(name, r);
            }
        }
        Err(e) => report("agent key", Err(e)),
    }
    let mirror = git::Mirror { url: cfg.repo_url.clone(), dir: cfg.data_dir.join("mirror.git") };
    let fetched = async {
        mirror.fetch().await?;
        mirror.branch_head("main").await
    }
    .await;
    report("git", fetched.map(|sha| format!("{} main at {}", cfg.repo_name, &sha[..7])));
    let sandbox = sandbox::Sandbox::new(cfg.engine, &cfg.image, cfg.test_timeout, cfg.data_dir.join("runs"), "doctor");
    report("sandbox", sandbox.check().await.map(|()| format!("image {}", &cfg.image[..12])));
    if let Some((email, password)) = &cfg.activity {
        let g = switch::gateway::Gateway::new(&cfg.switch_url, email, password);
        let r = async {
            g.login().await.map_err(up)?;
            g.activity(&cfg.work_room).await.map_err(up)
        }
        .await;
        report("activity", r.map(|rows| format!("{} rows readable", rows.len())));
    }
    if failed == 0 { Ok(()) } else { Err(NotaryError::new(Code::ConfigurationInvalid, format!("{failed} check(s) failed"))) }
}

/// An ISO-8601 UTC timestamp `hours` from now.
pub(crate) fn iso_in_hours(hours: u64) -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) + hours * 3600;
    let days = secs / 86_400;
    let rem = secs % 86_400;
    // Civil-from-days (Howard Hinnant), for dates after 1970.
    let z = i64::try_from(days).unwrap_or(0) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    #[test]
    fn iso_timestamps_are_well_formed() {
        let t = super::iso_in_hours(0);
        assert_eq!(t.len(), 20);
        assert!(t.starts_with("20") && t.ends_with('Z'));
    }
}
