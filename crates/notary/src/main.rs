//! `notary`: the Agora Notary agent.

mod switch;

use std::path::PathBuf;

use serde_json::{Value, json};

fn state_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"), PathBuf::from).join("agora-notary")
}

fn load_agent() -> Result<switch::Agent, String> {
    let path = state_dir().join("agent.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("can't read {}: {e} (run scripts/register.sh)", path.display()))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("{} isn't JSON: {e}", path.display()))?;
    let field = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string).ok_or_else(|| format!("{} has no {k}", path.display()));
    let base = std::env::var("NOTARY_SWITCH_URL").unwrap_or_else(|_| "http://127.0.0.1:8000".into());
    Ok(switch::Agent::new(&base, &field("id")?, &field("api_key")?))
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("probe") => probe().await,
        _ => Err("usage: notary probe".into()),
    };
    if let Err(e) = result {
        eprintln!("notary: {e}");
        std::process::exit(1);
    }
}

/// Spike S2: print what arrives; `@agora-notary card` opens an approval card in that room,
/// `@agora-notary ops` tries connect_to_room + send_targeted_message back to the sender.
async fn probe() -> Result<(), String> {
    let agent = load_agent()?;
    let connection = format!("probe-{}", std::process::id());
    let mut events = switch::ws::spawn(agent.clone(), connection.clone());
    while let Some(event) = events.recv().await {
        match event {
            switch::ws::Event::Message(m) => {
                println!("message room={} from={} ({}) addressed={} body={:?}", m.room_id, m.sender_name, m.sender_kind, m.addressed, switch::clip(&m.body, 160));
                if !m.addressed {
                    continue;
                }
                if m.body.contains("card") {
                    let options = [
                        switch::ApprovalOption { id: "approve".into(), label: "Approve".into(), decision: "accept".into() },
                        switch::ApprovalOption { id: "hold".into(), label: "Hold".into(), decision: "decline".into() },
                    ];
                    let r = agent.open_approval("probe", &format!("probe-{}", m.message_id), &m.room_id, "Probe: approve?", "Spike S2 approval card from a custom agent", &options, &in_hours(2)).await;
                    println!("open_approval -> {r:?}");
                }
                if m.body.contains("ops") {
                    let c = agent.op("connect_to_room", &json!({"room_id": m.room_id}), Some(&connection)).await;
                    println!("connect_to_room -> {}", c.as_ref().map_or_else(ToString::to_string, |v| switch::clip(&v.to_string(), 600)));
                    let t = agent.op("send_targeted_message", &json!({"body": "probe: targeted reply", "target_names": [m.sender_name]}), Some(&connection)).await;
                    println!("send_targeted_message -> {t:?}");
                    println!("participants -> {:?}", agent.participants(&m.room_id).await);
                }
            }
            switch::ws::Event::Approval(o) => {
                println!("approval_outcome {o:?}");
                println!("delivered -> {:?}", agent.delivered(&o.session_id, &o.request_id).await);
            }
            other => println!("{other:?}"),
        }
    }
    Ok(())
}

/// An ISO-8601 UTC timestamp `hours` from now.
fn in_hours(hours: u64) -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) + hours * 3600;
    iso_utc(secs)
}

fn iso_utc(secs: u64) -> String {
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
