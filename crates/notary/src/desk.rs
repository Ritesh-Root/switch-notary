//! The work-order desk: everything the Notary does, one event at a time.
//!
//! Hub (people + Notary): `new <request>` opens a work order; `status`; `show <WO|id>`.
//! Work room (builder, reviewer, Notary): reports with a `notary-claims` block are checked
//! against git, the Notary's own sandboxed test run and Switch's tool activity, then bounced
//! (at most `max_bounces` times) or passed on. People only ever see what the Notary posts.

use std::collections::{BTreeMap, HashMap};

use notary_core::claims::{self, Report};
use notary_core::errors::{Code, NotaryError};
use notary_core::evidence::{Authority, Body, Confidence, Coverage, Evidence, Op, Subject};
use notary_core::judge::{self, Audit, Commit, Facts, Next, Row, Seen, Stage, TestRun};
use notary_core::ledger::{Ledger, RecordKind};
use notary_core::policy::Policy;
use notary_core::receipt::{self, Bundle};
use notary_core::render::{self, defuse};
use notary_core::signing::Signer;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};

use crate::config::{Approval, Config};
use crate::git::Mirror;
use crate::sandbox::Sandbox;
use crate::switch::gateway::Gateway;
use crate::switch::ws::RoomMessage;
use crate::switch::{Agent, ApprovalOption, ApprovalOutcome, Card, clip};

const POLICY_DOC: &str = "Desk Policy";
const BASE_BRANCH: &str = "main";

#[derive(Debug, Clone)]
struct WorkOrder {
    wo: String,
    requester: String,
    title: String,
    base: String,
    branch: String,
    policy: Policy,
    state: String,
    attempts: u8,
    builder_audit: Option<String>,
    approval: Option<String>,
}

pub struct Desk {
    cfg: Config,
    agent: Agent,
    connection: String,
    gateway: Option<Gateway>,
    ledger: Ledger,
    db: Connection,
    mirror: Mirror,
    sandbox: Sandbox,
    signer: Signer,
    /// (sha, argv) → the evidence id and result of the Notary's run, so a re-report at the same sha doesn't re-run.
    test_memo: HashMap<(String, String), (String, TestRun)>,
    agent_ids: HashMap<String, String>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn short(s: &str) -> &str {
    s.get(..7).unwrap_or(s)
}

impl Desk {
    /// # Errors
    /// If the databases, mirror directory or signing key can't be opened.
    pub fn open(cfg: Config, agent: Agent, connection: String) -> Result<Self, NotaryError> {
        let io = |e: String| NotaryError::new(Code::ConfigurationInvalid, e);
        std::fs::create_dir_all(&cfg.data_dir)
            .map_err(|e| io(format!("{}: {e}", cfg.data_dir.display())))?;
        std::fs::create_dir_all(&cfg.state_dir)
            .map_err(|e| io(format!("{}: {e}", cfg.state_dir.display())))?;
        let ledger = Ledger::open(&cfg.data_dir.join("ledger.db"))?;
        let db = Connection::open(cfg.data_dir.join("desk.db"))?;
        db.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS work_orders (
                 n INTEGER PRIMARY KEY AUTOINCREMENT, wo TEXT UNIQUE, requester TEXT NOT NULL, title TEXT NOT NULL,
                 base TEXT NOT NULL, branch TEXT NOT NULL, policy TEXT NOT NULL, state TEXT NOT NULL,
                 attempts INTEGER NOT NULL DEFAULT 0, builder_audit TEXT, approval TEXT, created_ms INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS seen (message_id TEXT PRIMARY KEY);",
        )?;
        let signer = Signer::load_or_create(&cfg.state_dir.join("signing.key")).map_err(io)?;
        let mirror = Mirror {
            url: cfg.repo_url.clone(),
            dir: cfg.data_dir.join("mirror.git"),
        };
        let sandbox = Sandbox::new(
            cfg.engine,
            &cfg.image,
            cfg.test_timeout,
            cfg.data_dir.join("runs"),
            &format!("notary-{}", &agent.id[..8.min(agent.id.len())]),
        );
        let gateway = cfg
            .activity
            .as_ref()
            .map(|(email, password)| Gateway::new(&cfg.switch_url, email, password));
        Ok(Self {
            cfg,
            agent,
            connection,
            gateway,
            ledger,
            db,
            mirror,
            sandbox,
            signer,
            test_memo: HashMap::new(),
            agent_ids: HashMap::new(),
        })
    }

    #[must_use]
    pub fn public_key(&self) -> String {
        self.signer.public_hex()
    }

    // ---------- Switch helpers ----------

    async fn say(&self, room: &str, text: &str) {
        if let Err(e) = self.agent.post(room, &clip(text, 3900)).await {
            eprintln!("notary: couldn't post in {room}: {e}");
        }
    }

    /// Run a room-bound operation: claim the room on our connection first.
    async fn in_room(&self, room: &str, op: &str, args: &Value) -> Result<Value, NotaryError> {
        let up = |e: crate::switch::SwitchError| {
            NotaryError::new(Code::UpstreamUnavailable, format!("{op}: {e}"))
        };
        self.agent
            .op(
                "connect_to_room",
                &json!({"room_id": room, "include_general_instructions": false}),
                Some(&self.connection),
            )
            .await
            .map_err(up)?;
        self.agent
            .op(op, args, Some(&self.connection))
            .await
            .map_err(up)
    }

    async fn load_policy(&self) -> Result<(Policy, String), NotaryError> {
        let up = |e: crate::switch::SwitchError| {
            NotaryError::new(Code::UpstreamUnavailable, e.to_string())
        };
        let room = self
            .agent
            .op(
                "connect_to_room",
                &json!({"room_id": self.cfg.work_room, "include_general_instructions": false}),
                Some(&self.connection),
            )
            .await
            .map_err(up)?;
        let id = room
            .get("documents")
            .and_then(Value::as_array)
            .and_then(|docs| {
                docs.iter()
                    .find(|d| d.get("name").and_then(Value::as_str) == Some(POLICY_DOC))
            })
            .and_then(|d| d.get("id").and_then(Value::as_str))
            .ok_or_else(|| {
                NotaryError::new(
                    Code::ConfigurationInvalid,
                    format!("the work room has no \"{POLICY_DOC}\" document"),
                )
            })?
            .to_string();
        let docs = self
            .agent
            .op(
                "load_internal_documents",
                &json!({"ids": [id]}),
                Some(&self.connection),
            )
            .await
            .map_err(up)?;
        let content = docs
            .as_array()
            .and_then(|a| a.first())
            .and_then(|d| d.get("content"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Ok((Policy::parse(&content)?, content))
    }

    async fn agent_id(&mut self, name: &str) -> Option<String> {
        if let Some(id) = self.agent_ids.get(name) {
            return Some(id.clone());
        }
        let p = self.agent.participants(&self.cfg.work_room).await.ok()?;
        for x in p
            .get("participants")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let (Some(n), Some(id)) = (
                x.get("name").and_then(Value::as_str),
                x.get("agent_id").and_then(Value::as_str),
            ) {
                self.agent_ids.insert(n.to_string(), id.to_string());
            }
        }
        self.agent_ids.get(name).cloned()
    }

    // ---------- work orders ----------

    fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<WorkOrder> {
        let policy: String = r.get(5)?;
        Ok(WorkOrder {
            wo: r.get(0)?,
            requester: r.get(1)?,
            title: r.get(2)?,
            base: r.get(3)?,
            branch: r.get(4)?,
            policy: serde_json::from_str(&policy)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
            state: r.get(6)?,
            attempts: u8::try_from(r.get::<_, i64>(7)?).unwrap_or(u8::MAX),
            builder_audit: r.get(8)?,
            approval: r.get(9)?,
        })
    }

    const COLS: &'static str =
        "wo, requester, title, base, branch, policy, state, attempts, builder_audit, approval";

    fn find_wo(&self, clause: &str, arg: &str) -> Option<WorkOrder> {
        self.db
            .query_row(
                &format!(
                    "SELECT {} FROM work_orders WHERE {clause} ORDER BY rowid DESC LIMIT 1",
                    Self::COLS
                ),
                params![arg],
                Self::row,
            )
            .optional()
            .ok()
            .flatten()
    }

    fn set_state(&self, wo: &str, state: &str) {
        let _ = self.db.execute(
            "UPDATE work_orders SET state = ?1 WHERE wo = ?2",
            params![state, wo],
        );
    }

    fn first_time(&self, message_id: &str) -> bool {
        message_id.is_empty()
            || self
                .db
                .execute(
                    "INSERT OR IGNORE INTO seen (message_id) VALUES (?1)",
                    params![message_id],
                )
                .is_ok_and(|n| n == 1)
    }

    // ---------- events ----------

    pub async fn on_message(&mut self, m: RoomMessage) {
        if !self.first_time(&m.message_id) {
            return;
        }
        if m.room_id == self.cfg.hub_room && m.addressed && m.sender_kind != "agent" {
            self.on_hub_command(&m).await;
        } else if m.room_id == self.cfg.work_room && m.addressed {
            if m.sender_name == self.cfg.builder {
                self.on_report(&m, Stage::Builder).await;
            } else if self.cfg.reviewer.as_deref() == Some(m.sender_name.as_str()) {
                self.on_report(&m, Stage::Reviewer).await;
            }
        }
    }

    async fn on_hub_command(&mut self, m: &RoomMessage) {
        let words: Vec<&str> = m
            .body
            .split_whitespace()
            .skip_while(|w| w.starts_with('@'))
            .collect();
        let verb = words.first().map(|w| w.to_lowercase()).unwrap_or_default();
        let rest = words.get(1..).unwrap_or_default().join(" ");
        match verb.as_str() {
            "new" | "file" if !rest.is_empty() => self.open_work_order(&m.sender_name, &rest).await,
            "status" => self.status().await,
            "show" => self.show(&rest).await,
            "approve" | "hold" if self.cfg.approval == Approval::Text => {
                let wo = rest
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_uppercase();
                self.on_decision(&wo, &verb, &m.sender_name).await;
            }
            _ => {
                let help = "File a work order: `@agora-notary new <what's wrong and how to tell it's fixed>`. Also `status` and `show WO-1`. The builder works in the work room; you only see results I've checked.";
                self.say(&self.cfg.hub_room, help).await;
            }
        }
    }

    async fn open_work_order(&mut self, requester: &str, title: &str) {
        let hub = self.cfg.hub_room.clone();
        let (policy, policy_text) = match self.load_policy().await {
            Ok(p) => p,
            Err(e) => {
                return self
                    .say(
                        &hub,
                        &format!(
                            "Can't open a work order: `{}`: {}",
                            e.code,
                            defuse(&e.detail)
                        ),
                    )
                    .await;
            }
        };
        if let Err(e) = self.mirror.fetch().await {
            return self
                .say(
                    &hub,
                    &format!(
                        "Can't open a work order: `{}`: {}",
                        e.code,
                        defuse(&e.detail)
                    ),
                )
                .await;
        }
        let base = match self.mirror.branch_head(BASE_BRANCH).await {
            Ok(b) => b,
            Err(e) => {
                return self
                    .say(
                        &hub,
                        &format!(
                            "Can't open a work order: `{}`: {}",
                            e.code,
                            defuse(&e.detail)
                        ),
                    )
                    .await;
            }
        };
        let title = clip(title, 300);
        let policy_json = serde_json::to_string(&policy).unwrap_or_default();
        let inserted = self.db.execute(
            "INSERT INTO work_orders (wo, requester, title, base, branch, policy, state, created_ms) VALUES (NULL, ?1, ?2, ?3, '', ?4, 'building', ?5)",
            params![requester, title, base, policy_json, now_ms()],
        );
        if inserted.is_err() {
            return self
                .say(
                    &hub,
                    "Can't open a work order: the desk database refused the write.",
                )
                .await;
        }
        let n = self.db.last_insert_rowid();
        let wo = format!("WO-{n}");
        let branch = format!("wo-{n}");
        let _ = self.db.execute(
            "UPDATE work_orders SET wo = ?1, branch = ?2 WHERE n = ?3",
            params![wo, branch, n],
        );
        let intake = json!({"protocol": "notary-intake/1", "wo": wo, "title": title, "requester": requester, "repo": self.cfg.repo_name,
            "base": base, "branch": branch, "policy": policy, "policy_doc_sha256": notary_core::ids::sha_hex(policy_text.as_bytes())});
        let _ = self.ledger.append(&wo, RecordKind::Policy, &intake);

        let protected = policy.protected.join(", ");
        let acceptance = policy.acceptance.join(" ");
        let doc = format!(
            "# {wo}: {}\n\nRequested by {} in the desk.\n\n- Repository: {}\n- Base: `{base}` (main)\n- Branch to push: `{branch}`\n- Acceptance check (the Notary runs it itself, with no network): `{acceptance}`\n- Don't edit: {protected}. Put new tests in new files.\n- Bounces allowed: {}\n\nWhen done, report to agora-notary in the work room. End your message with one `notary-claims` block (see the Reporting Contract document), with `\"wo\":\"{wo}\"` and the full 40-character sha you pushed.\n",
            defuse(&title),
            defuse(requester),
            self.cfg.repo_name,
            policy.max_bounces
        );
        let created = self.in_room(&self.cfg.work_room, "create_room_document", &json!({"name": wo, "description": clip(&format!("Work order: {title}"), 200), "instructions": "Read before working on this work order.", "content": doc})).await;
        if let Err(e) = created {
            eprintln!("notary: couldn't create the {wo} document: {e}");
        }
        let builder = self.cfg.builder.clone();
        let work = self.cfg.work_room.clone();
        self.say(&work, &format!("@{builder} {wo}: {}\nBranch `{branch}` from `{}`. Room document \"{wo}\" has the details. Report back to me with a notary-claims block.", defuse(&title), short(&base))).await;
        self.say(&hub, &format!("**{wo}** opened for {}: \"{}\". The builder works in the work room; this room only gets results I've checked.", defuse(requester), defuse(&title))).await;
    }

    async fn status(&self) {
        let Ok(mut stmt) = self.db.prepare(&format!(
            "SELECT {} FROM work_orders WHERE wo IS NOT NULL ORDER BY n DESC LIMIT 10",
            Self::COLS
        )) else {
            return;
        };
        let lines: Vec<String> = stmt
            .query_map([], Self::row)
            .map(|rows| {
                rows.flatten()
                    .map(|w| {
                        format!(
                            "- **{}** {}: {} (requested by {}, {} report(s))",
                            w.wo,
                            w.state,
                            defuse(&w.title),
                            defuse(&w.requester),
                            w.attempts
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let text = if lines.is_empty() {
            "No work orders yet.".to_string()
        } else {
            lines.join("\n")
        };
        self.say(&self.cfg.hub_room, &text).await;
    }

    async fn show(&self, args: &str) {
        let mut parts = args.split_whitespace();
        let target = parts.next().unwrap_or_default();
        let page = match (parts.next(), parts.next()) {
            (Some("page"), Some(n)) => n.parse().unwrap_or(1),
            _ => 1,
        };
        let hub = &self.cfg.hub_room;
        if let Some(w) = self.find_wo("wo = ?1", &target.to_uppercase()) {
            let lines = self.ledger_lines(&w.wo);
            return self
                .say(
                    hub,
                    &render::page(&format!("{} ledger", w.wo), &lines, page),
                )
                .await;
        }
        if target.len() >= 11 && target.contains('_') {
            let mut found = Vec::new();
            let wos: Vec<String> = self
                .db
                .prepare("SELECT wo FROM work_orders WHERE wo IS NOT NULL")
                .and_then(|mut s| s.query_map([], |r| r.get(0)).map(|r| r.flatten().collect()))
                .unwrap_or_default();
            for wo in wos {
                found.extend(self.ledger.find(&wo, target, 6).unwrap_or_default());
            }
            found.sort();
            found.dedup();
            let text = match found.as_slice() {
                [] => format!(
                    "No record starts with `{}` (`citation_unknown`).",
                    defuse(target)
                ),
                [one] => match self.ledger.record(one) {
                    Ok(Some(v)) => format!(
                        "`{one}`\n```json\n{}\n```",
                        clip(
                            &defuse(&serde_json::to_string_pretty(&v).unwrap_or_default()),
                            3500
                        )
                    ),
                    Ok(None) => format!("`{one}` is missing."),
                    Err(e) => format!("`{one}`: `{}` {}", e.code, e.detail),
                },
                many => format!(
                    "`{}` matches {} records; give more characters:\n{}",
                    defuse(target),
                    many.len(),
                    many.iter()
                        .take(5)
                        .map(|id| format!("- `{id}`"))
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
            };
            return self.say(hub, &text).await;
        }
        self.say(
            hub,
            "Usage: `show WO-1 [page N]` or `show ev_<at least 8 hex>`.",
        )
        .await;
    }

    fn ledger_lines(&self, wo: &str) -> Vec<String> {
        let entries = self.ledger.entries(wo).unwrap_or_default();
        entries
            .iter()
            .map(|e| {
                let v = self
                    .ledger
                    .record(&e.record)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                let what = match e.kind {
                    RecordKind::Evidence => format!(
                        "{} by {}: {}",
                        v["operation"].as_str().unwrap_or("?"),
                        v["authority"].as_str().unwrap_or("?"),
                        clip(&v["result"].to_string(), 90)
                    ),
                    RecordKind::Audit => {
                        let codes: Vec<String> = v["checked"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|c| {
                                format!(
                                    "{}:{}",
                                    c["claim"].as_str().unwrap_or("?"),
                                    c["code"].as_str().unwrap_or("backed")
                                )
                            })
                            .collect();
                        format!(
                            "audit of {} report {}: {}",
                            v["agent"].as_str().unwrap_or("?"),
                            v["attempt"],
                            codes.join(" ")
                        )
                    }
                    RecordKind::Decision => format!(
                        "decision: {} by {}",
                        v["answer"].as_str().unwrap_or("?"),
                        v["by"].as_str().unwrap_or("?")
                    ),
                    RecordKind::Report => format!(
                        "claims from {}: {} claim(s)",
                        v["agent"].as_str().unwrap_or("?"),
                        v["report"]["claims"].as_array().map_or(0, Vec::len)
                    ),
                    RecordKind::Policy => "intake + Desk Policy".to_string(),
                };
                format!(
                    "{}. `{}` {what}",
                    e.seq,
                    &e.record[..14.min(e.record.len())]
                )
            })
            .collect()
    }

    // ---------- reports ----------

    async fn on_report(&mut self, m: &RoomMessage, stage: Stage) {
        let work = self.cfg.work_room.clone();
        let want = if stage == Stage::Builder {
            "building"
        } else {
            "reviewing"
        };
        let Some(mut w) = self.find_wo("state = ?1", want) else {
            return;
        };
        let report = match claims::parse(&m.body) {
            Ok(r) if r.wo.eq_ignore_ascii_case(&w.wo) => r,
            Ok(r) => return self
                .say(
                    &work,
                    &format!(
                        "@{} `block_invalid`: this report is for {} but the open work order is {}.",
                        m.sender_name,
                        defuse(&r.wo),
                        w.wo
                    ),
                )
                .await,
            Err(e) => {
                let meta = e.code.meta();
                return self
                    .say(
                        &work,
                        &format!(
                            "@{} {}: `{}`: {} ({})",
                            m.sender_name,
                            w.wo,
                            e.code,
                            meta.remediation,
                            defuse(&clip(&e.detail, 300))
                        ),
                    )
                    .await;
            }
        };
        if stage == Stage::Builder {
            w.attempts = w.attempts.saturating_add(1);
            let _ = self.db.execute(
                "UPDATE work_orders SET attempts = ?1 WHERE wo = ?2",
                params![w.attempts, w.wo],
            );
        }
        let (report_id, _) = match self.ledger.append(
            &w.wo,
            RecordKind::Report,
            &json!({"agent": m.sender_name, "message_id": m.message_id, "report": report}),
        ) {
            Ok(x) => x,
            Err(e) => {
                return self
                    .say(
                        &work,
                        &format!(
                            "{}: `{}` couldn't record the report: {}",
                            w.wo, e.code, e.detail
                        ),
                    )
                    .await;
            }
        };
        let facts = self.gather(&w, &report, &m.sender_name, stage).await;
        let (checked, findings) = judge::judge(stage, &report, &w.policy, &facts);
        let attempt = if stage == Stage::Builder {
            w.attempts
        } else {
            1
        };
        let audit = Audit {
            protocol: "notary-audit/1".into(),
            wo: w.wo.clone(),
            sha: report.sha.clone(),
            agent: m.sender_name.clone(),
            stage,
            attempt,
            report: report_id,
            checked,
            findings,
        };
        let audit_id = match self.ledger.append(&w.wo, RecordKind::Audit, &audit) {
            Ok((id, _)) => id,
            Err(e) => {
                return self
                    .say(
                        &work,
                        &format!(
                            "{}: `{}` couldn't record the audit: {}",
                            w.wo, e.code, e.detail
                        ),
                    )
                    .await;
            }
        };
        let next = judge::next(&audit, w.policy.max_bounces);
        match (&next, stage) {
            (Next::Bounce, _) => {
                self.say(
                    &work,
                    &render::bounce(&audit, &audit_id, w.policy.max_bounces, &self.cfg.builder),
                )
                .await
            }
            (Next::Escalate, _) => {
                self.say(&work, &render::verdict(&audit, &audit_id, &next))
                    .await;
                let _ = self.db.execute(
                    "UPDATE work_orders SET builder_audit = ?1 WHERE wo = ?2",
                    params![audit_id, w.wo],
                );
                self.request_approval(&w, &audit, None, &[Code::RevisionLimit])
                    .await;
            }
            (Next::Ready { flags }, Stage::Builder) => {
                self.say(&work, &render::verdict(&audit, &audit_id, &next))
                    .await;
                let _ = self.db.execute(
                    "UPDATE work_orders SET builder_audit = ?1 WHERE wo = ?2",
                    params![audit_id, w.wo],
                );
                if let Some(reviewer) = self.cfg.reviewer.clone() {
                    self.set_state(&w.wo, "reviewing");
                    let ask = format!(
                        "@{reviewer} {}: please review branch `{}` at `{}` (base `{}`). Run the acceptance check yourself, then report to me with a notary-claims block: `reviewed_diff` (sha {}) and `check_ran`.",
                        w.wo,
                        w.branch,
                        report.sha,
                        short(&w.base),
                        report.sha
                    );
                    self.say(&work, &ask).await;
                } else {
                    self.request_approval(&w, &audit, None, flags).await;
                }
            }
            (Next::Ready { flags }, Stage::Reviewer) => {
                self.say(&work, &render::verdict(&audit, &audit_id, &next))
                    .await;
                let builder = w
                    .builder_audit
                    .as_deref()
                    .and_then(|id| self.ledger.record(id).ok().flatten())
                    .and_then(|v| serde_json::from_value::<Audit>(v).ok());
                let mut all = flags.clone();
                if let Some(b) = &builder
                    && let Next::Ready { flags: bf } = judge::next(b, w.policy.max_bounces)
                {
                    all.extend(bf);
                }
                all.sort();
                all.dedup();
                match builder {
                    Some(b) => self.request_approval(&w, &b, Some(&audit), &all).await,
                    None => self.request_approval(&w, &audit, None, &all).await,
                }
            }
        }
    }

    fn seal(&self, wo: &str, body: Body) -> String {
        match Evidence::seal(body) {
            Ok(ev) => match self.ledger.append(wo, RecordKind::Evidence, &ev.body) {
                Ok((id, _)) => id,
                Err(e) => {
                    eprintln!("notary: couldn't record evidence: {e}");
                    ev.id
                }
            },
            Err(e) => {
                eprintln!("notary: evidence has no canonical form: {e}");
                String::new()
            }
        }
    }

    /// An evidence body about `sha`. `source` is who vouches, how sure, and how complete.
    fn body(
        &self,
        source: (Authority, Confidence, Coverage),
        sha: &str,
        operation: Op,
        params: &Value,
        result: Value,
    ) -> Body {
        let (authority, confidence, coverage) = source;
        Body {
            authority,
            confidence,
            coverage,
            subject: Subject {
                repo: self.cfg.repo_name.clone(),
                sha: Some(sha.to_string()),
            },
            operation,
            params: params.as_object().cloned().unwrap_or_else(Map::new),
            result,
            limitations: vec![],
        }
    }

    async fn gather(&mut self, w: &WorkOrder, report: &Report, agent: &str, stage: Stage) -> Facts {
        let sha = report.sha.clone();
        let fetched = self.mirror.fetch().await;
        let unknown = |e: &NotaryError| Coverage::Unknown {
            reason: format!("{}: {}", e.code, clip(&e.detail, 200)),
        };

        // git: does the commit exist, and is it on the claimed branch?
        let branch = report.branch().map(str::to_string);
        let commit: Result<Commit, NotaryError> = match &fetched {
            Err(e) => Err(e.clone()),
            Ok(()) => match self.mirror.commit_exists(&sha).await {
                Ok(false) => Ok(Commit {
                    exists: false,
                    on_branch: None,
                }),
                Ok(true) => match &branch {
                    None => Ok(Commit {
                        exists: true,
                        on_branch: None,
                    }),
                    Some(b) => self.mirror.on_branch(&sha, b).await.map(|on| Commit {
                        exists: true,
                        on_branch: Some(on),
                    }),
                },
                Err(e) => Err(e),
            },
        };
        let commit_ev = self.seal(
            &w.wo,
            match &commit {
                Ok(c) => self.body(
                    (Authority::Git, Confidence::Observed, Coverage::Complete),
                    &sha,
                    Op::GitCommit,
                    &json!({"branch": branch}),
                    json!({"exists": c.exists, "on_branch": c.on_branch}),
                ),
                Err(e) => self.body(
                    (Authority::Git, Confidence::Observed, unknown(e)),
                    &sha,
                    Op::GitCommit,
                    &json!({"branch": branch}),
                    json!({"error": e.code}),
                ),
            },
        );
        let exists = matches!(commit, Ok(Commit { exists: true, .. }));

        // git: what changed since the base.
        let diff: Result<Vec<String>, NotaryError> = match (&fetched, exists) {
            (Err(e), _) => Err(e.clone()),
            (Ok(()), false) => Err(NotaryError::new(Code::CommitMissing, "no such commit")),
            (Ok(()), true) => self.mirror.diff_names(&w.base, &sha).await,
        };
        let diff_ev = self.seal(
            &w.wo,
            match &diff {
                Ok(paths) => self.body(
                    (Authority::Git, Confidence::Observed, Coverage::Complete),
                    &sha,
                    Op::GitDiff,
                    &json!({"base": w.base}),
                    json!({"paths": paths}),
                ),
                Err(e) => self.body(
                    (Authority::Git, Confidence::Observed, unknown(e)),
                    &sha,
                    Op::GitDiff,
                    &json!({"base": w.base}),
                    json!({"error": e.code}),
                ),
            },
        );

        // The Notary's own run of the acceptance check (builder stage; the reviewer reuses it).
        let argv_key = w.policy.acceptance.join("\u{1f}");
        let tests = if let Some((ev, run)) = self.test_memo.get(&(sha.clone(), argv_key.clone())) {
            Some(Seen {
                ev: ev.clone(),
                got: Ok(run.clone()),
            })
        } else if stage == Stage::Builder && exists {
            let params =
                json!({"argv": w.policy.acceptance, "image": self.cfg.image, "network": "none"});
            match self
                .sandbox
                .run(&self.mirror, &sha, &w.policy.acceptance)
                .await
            {
                Ok(o) => {
                    let result = json!({"run_id": o.run_id, "exit_code": o.run.exit_code, "collected": o.run.collected, "passed": o.run.passed, "failed": o.run.failed, "errors": o.run.errors, "failed_ids": o.run.failed_ids, "log_sha256": o.log_sha256, "log_bytes": o.log_bytes});
                    let ev = self.seal(
                        &w.wo,
                        self.body(
                            (
                                Authority::NotaryRun,
                                Confidence::Observed,
                                Coverage::Complete,
                            ),
                            &sha,
                            Op::NotaryTestRun,
                            &params,
                            result,
                        ),
                    );
                    self.test_memo
                        .insert((sha.clone(), argv_key), (ev.clone(), o.run.clone()));
                    Some(Seen { ev, got: Ok(o.run) })
                }
                Err(e) => {
                    let ev = self.seal(
                        &w.wo,
                        self.body(
                            (Authority::NotaryRun, Confidence::Observed, unknown(&e)),
                            &sha,
                            Op::NotaryTestRun,
                            &params,
                            json!({"error": e.code}),
                        ),
                    );
                    Some(Seen {
                        ev,
                        got: Err(e.code),
                    })
                }
            }
        } else {
            None
        };

        // Switch's tool-activity log for this agent (corroboration only).
        let activity = if self.gateway.is_some() {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await; // rows are buffered by the session host
            let agent_id = self.agent_id(agent).await;
            let rows = match (&self.gateway, agent_id) {
                (Some(g), Some(id)) => g
                    .activity(&self.cfg.work_room)
                    .await
                    .map_err(|e| NotaryError::new(Code::UpstreamUnavailable, e.to_string()))
                    .map(|rows| latest_tool_rows(rows, &id)),
                _ => Err(NotaryError::new(
                    Code::UpstreamUnavailable,
                    format!("{agent} isn't a participant of the work room"),
                )),
            };
            let limitations = vec![
                "reported by the agent's session host with the agent's own key".to_string(),
                "Bash titles are cut at 160 characters; tool output isn't recorded".to_string(),
                "rows are buffered: a missing row is not proof a command didn't run".to_string(),
            ];
            let params = json!({"room": self.cfg.work_room, "agent": agent});
            let ev = self.seal(&w.wo, match &rows {
                Ok(r) => self.body((Authority::SwitchHost, Confidence::Inferred, Coverage::Partial { limitations }), &sha, Op::SwitchActivity, &params, json!({"rows": r.iter().map(|x| json!({"title": x.title, "status": x.status})).collect::<Vec<_>>()})),
                Err(e) => self.body((Authority::SwitchHost, Confidence::Inferred, unknown(e)), &sha, Op::SwitchActivity, &params, json!({"error": e.code})),
            });
            Some(Seen {
                ev,
                got: rows.map_err(|e| e.code),
            })
        } else {
            None
        };

        // Everything this work order has on record, for checking citations.
        let mut cited = BTreeMap::new();
        for e in self
            .ledger
            .entries(&w.wo)
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.kind == RecordKind::Evidence)
        {
            if let Ok(Some(v)) = self.ledger.record(&e.record)
                && let Ok(body) = serde_json::from_value::<Body>(v)
            {
                cited.insert(e.record, (body.operation, body.subject.sha));
            }
        }

        Facts {
            branch: Some(w.branch.clone()),
            commit: Seen {
                ev: commit_ev,
                got: commit.map_err(|e| e.code),
            },
            diff: Seen {
                ev: diff_ev,
                got: diff.map_err(|e| e.code),
            },
            tests,
            activity,
            cited,
        }
    }

    // ---------- approval ----------

    async fn request_approval(
        &mut self,
        w: &WorkOrder,
        builder: &Audit,
        reviewer: Option<&Audit>,
        flags: &[Code],
    ) {
        let hub = self.cfg.hub_room.clone();
        let head = self
            .ledger
            .entries(&w.wo)
            .ok()
            .and_then(|e| e.last().map(|e| e.hash.clone()))
            .unwrap_or_default();
        self.say(
            &hub,
            &render::hub_ready(&w.wo, builder, reviewer, flags, &head),
        )
        .await;
        self.set_state(&w.wo, "awaiting_approval");
        self.open_card(w, flags).await;
    }

    async fn open_card(&self, w: &WorkOrder, flags: &[Code]) {
        let hub = &self.cfg.hub_room;
        if self.cfg.approval == Approval::Text {
            let who = w.policy.approvers.join(", ");
            return self.say(hub, &format!("Approvers ({}): reply `@agora-notary approve {}` or `@agora-notary hold {}`.", defuse(&who), w.wo, w.wo)).await;
        }
        let k: i64 = self
            .db
            .query_row(
                "SELECT COUNT(*) FROM seen WHERE message_id LIKE ?1",
                params![format!("card:{}:%", w.wo)],
                |r| r.get(0),
            )
            .unwrap_or(0)
            + 1;
        let request = format!("{}-a{k}", w.wo.to_lowercase());
        let _ = self.db.execute(
            "INSERT OR IGNORE INTO seen (message_id) VALUES (?1)",
            params![format!("card:{}:{k}", w.wo)],
        );
        let flagged = if flags.is_empty() {
            "nothing flagged".to_string()
        } else {
            flags
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        };
        let detail = format!(
            "{}. Flags: {flagged}. Only {} can approve (Desk Policy).",
            w.title,
            w.policy.approvers.join(", ")
        );
        let options = [
            ApprovalOption {
                id: "approve".into(),
                label: "Approve and ship".into(),
                decision: "accept".into(),
            },
            ApprovalOption {
                id: "hold".into(),
                label: "Hold".into(),
                decision: "decline".into(),
            },
        ];
        let expires = crate::iso_in_hours(23);
        let session = w.wo.to_lowercase();
        let title = format!("{}: ship it?", w.wo);
        let detail = defuse(&detail);
        let card = Card {
            session: &session,
            request: &request,
            room: hub,
            title: &title,
            detail: &detail,
            options: &options,
            expires_at: &expires,
        };
        match self.agent.open_approval(&card).await {
            Ok(_) => {
                let _ = self.db.execute("UPDATE work_orders SET approval = ?1 WHERE wo = ?2", params![request, w.wo]);
            }
            Err(e) => self.say(hub, &format!("Couldn't open the approval card ({e}). Approvers can reply `@agora-notary approve {}` instead.", w.wo)).await,
        }
    }

    pub async fn on_outcome(&mut self, o: ApprovalOutcome) {
        let Some(w) = self.find_wo("approval = ?1", &o.request_id) else {
            let _ = self.agent.delivered(&o.session_id, &o.request_id).await;
            return;
        };
        let by = o
            .answered_by
            .as_deref()
            .map(username_of)
            .unwrap_or_default();
        if o.state == "expired" {
            self.say(
                &self.cfg.hub_room,
                &format!("The approval card for {} expired; opening a new one.", w.wo),
            )
            .await;
            self.open_card(&w, &[]).await;
        } else {
            let answer = o.answer.clone().unwrap_or_default();
            self.on_decision(&w.wo, &answer, &by).await;
        }
        let _ = self.agent.delivered(&o.session_id, &o.request_id).await;
    }

    async fn on_decision(&mut self, wo: &str, answer: &str, by: &str) {
        let hub = self.cfg.hub_room.clone();
        let Some(w) = self.find_wo("wo = ?1", wo) else {
            return;
        };
        if w.state != "awaiting_approval" {
            return;
        }
        let approver = w.policy.is_approver(by);
        let _ = self.ledger.append(&w.wo, RecordKind::Decision, &json!({"protocol": "notary-decision/1", "wo": w.wo, "answer": answer, "by": by, "counted": approver, "request": w.approval}));
        if !approver {
            self.say(&hub, &format!("{} isn't an approver for {} (`not_an_approver`; Desk Policy: {}). That answer is on record but doesn't count. Opening a new card.", defuse(by), w.wo, w.policy.approvers.join(", "))).await;
            return self.open_card(&w, &[Code::NotAnApprover]).await;
        }
        if answer != "approve" {
            self.set_state(&w.wo, "held");
            return self
                .say(&hub, &format!("**{} held** by {}.", w.wo, defuse(by)))
                .await;
        }
        self.finalize(&w, by).await;
    }

    async fn finalize(&mut self, w: &WorkOrder, by: &str) {
        let hub = self.cfg.hub_room.clone();
        let (entries, records) = match self.ledger.export(&w.wo) {
            Ok(x) => x,
            Err(e) => {
                return self
                    .say(
                        &hub,
                        &format!(
                            "{}: `{}` the ledger didn't verify, so no receipt: {}",
                            w.wo, e.code, e.detail
                        ),
                    )
                    .await;
            }
        };
        let Some(last) = entries.last() else { return };
        let head = self.signer.sign(
            &w.wo,
            entries.len(),
            &last.hash,
            u64::try_from(now_ms()).unwrap_or(0),
        );
        let bundle = Bundle {
            protocol: receipt::PROTOCOL.into(),
            head: head.clone(),
            entries,
            records,
        };
        if let Err(why) = receipt::verify(&bundle, Some(&self.signer.public_hex())) {
            return self.say(&hub, &format!("{}: `integrity_mismatch`: the receipt doesn't verify ({why}). Not signing.", w.wo)).await;
        }
        let json = serde_json::to_vec_pretty(&bundle).unwrap_or_default();
        let file = format!("receipt-{}.json", w.wo.to_lowercase());
        let dir = self.cfg.data_dir.join("receipts");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(&file), &json);

        let audits: Vec<(String, Audit)> = bundle
            .records
            .iter()
            .filter(|(id, _)| id.starts_with("au_"))
            .filter_map(|(id, v)| {
                serde_json::from_value::<Audit>(v.clone())
                    .ok()
                    .map(|a| (id.clone(), a))
            })
            .collect();
        let mut ordered: Vec<(&str, &Audit)> =
            audits.iter().map(|(id, a)| (id.as_str(), a)).collect();
        ordered.sort_by_key(|(_, a)| (a.stage == Stage::Reviewer, a.attempt));
        let doc = render::receipt_doc(
            &w.wo,
            &ordered,
            &format!("approved by {by}"),
            &head.head,
            &head.key,
            head.entries,
        );
        if let Err(e) = self.in_room(&hub, "create_room_document", &json!({"name": format!("{} Receipt", w.wo), "description": clip(&w.title, 200), "instructions": "The Notary's signed receipt for this work order.", "content": doc})).await {
            eprintln!("notary: couldn't create the receipt document: {e}");
        }
        if let Err(e) = self
            .agent
            .upload(
                &hub,
                &file,
                "application/json",
                json,
                &format!("{} receipt (agora-notary-receipt/1)", w.wo),
            )
            .await
        {
            eprintln!("notary: couldn't upload the receipt: {e}");
        }
        self.set_state(&w.wo, "approved");
        self.say(
            &hub,
            &format!(
                "**{} approved by {}.** Signed ledger head `{}` ({} entries, key `{}`). This message is the room's witness: `agora-verify {file}` must report this same head.",
                w.wo,
                defuse(by),
                head.head,
                head.entries,
                &head.key[..16]
            ),
        )
        .await;
    }
}

/// The latest revision of each tool call this agent made, oldest first.
fn latest_tool_rows(rows: Vec<crate::switch::gateway::ActivityRow>, agent_id: &str) -> Vec<Row> {
    let mut latest: BTreeMap<(String, String, String), crate::switch::gateway::ActivityRow> =
        BTreeMap::new();
    for r in rows
        .into_iter()
        .filter(|r| r.agent_id == agent_id && r.kind == "tool-activity")
    {
        let key = (r.session_id.clone(), r.turn_id.clone(), r.item_id.clone());
        if latest
            .get(&key)
            .is_none_or(|old| r.revision >= old.revision)
        {
            latest.insert(key, r);
        }
    }
    let mut v: Vec<_> = latest.into_values().collect();
    v.sort_by(|a, b| a.occurred_at.cmp(&b.occurred_at));
    v.into_iter()
        .map(|r| Row {
            title: r.title,
            status: r.status,
        })
        .collect()
}

/// `@switch-mattermost-<bridge uuid>-<username>:<server>` → `username`.
fn username_of(participant: &str) -> String {
    let local = participant
        .trim_start_matches('@')
        .split(':')
        .next()
        .unwrap_or_default();
    match local.strip_prefix("switch-mattermost-") {
        Some(rest) if rest.len() > 37 => rest[37..].to_string(),
        _ => local.rsplit('-').next().unwrap_or(local).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approvers_are_read_from_switch_participant_ids() {
        assert_eq!(
            username_of("@switch-mattermost-699baaf2-f47a-4c10-af38-74f78c4202ab-priya:localhost"),
            "priya"
        );
        assert_eq!(
            username_of(
                "@switch-mattermost-699baaf2-f47a-4c10-af38-74f78c4202ab-mary-jane:localhost"
            ),
            "mary-jane"
        );
    }
}
