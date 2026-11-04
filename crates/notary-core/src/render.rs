//! Everything the Notary says, written by code from audits: never the builder's prose.
//! Each agent-facing post carries a fenced JSON block (`notary-bounce` / `notary-verdict`)
//! so agents and tests read codes and evidence ids, not sentences.
//!
//! Text that came from an agent (paths, commands) is passed through [`defuse`] first: Switch
//! treats any `@name` an agent writes as addressing that agent, even inside backticks.

use std::fmt::Write as _;

use serde_json::{Value, json};

use crate::errors::Code;
use crate::judge::{Audit, Next, Verdict};

/// Stop `@word` from addressing anyone: a zero-width space after every `@`.
#[must_use]
pub fn defuse(s: &str) -> String {
    s.replace('@', "@\u{200b}")
}

fn defuse_value(v: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(defuse(s)),
        Value::Array(a) => Value::Array(a.iter().map(defuse_value).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), defuse_value(v))).collect()),
        other => other.clone(),
    }
}

fn short(id: &str) -> &str {
    id.get(..11).unwrap_or(id)
}

/// The claims that were backed, contradicted and unverified, by id.
fn tally(a: &Audit) -> (Vec<&str>, usize, usize) {
    let backed = a.checked.iter().filter(|c| matches!(c.verdict, Verdict::Backed { .. })).map(|c| c.claim.as_str()).collect();
    let contradicted = a.checked.iter().filter(|c| matches!(c.verdict, Verdict::Contradicted { .. })).count();
    let unverified = a.checked.iter().filter(|c| matches!(c.verdict, Verdict::Unverified { .. })).count();
    (backed, contradicted, unverified)
}

fn item(claim: Option<&str>, code: Code, facts: &Value, evidence: &[String]) -> Value {
    let meta = code.meta();
    json!({
        "claim": claim, "code": code, "retryable": meta.retryable, "fixer": meta.fixer,
        "do": meta.remediation, "facts": defuse_value(facts), "ev": evidence,
    })
}

/// The message sent back to the builder: what to fix, as codes with remediation and evidence.
#[must_use]
pub fn bounce(a: &Audit, audit_id: &str, max: u8, builder: &str) -> String {
    let mut fix = Vec::new();
    let mut unverified = Vec::new();
    for c in &a.checked {
        match &c.verdict {
            Verdict::Contradicted { code, evidence, facts } => fix.push(item(Some(&c.claim), *code, facts, evidence)),
            Verdict::Unverified { code, evidence, facts } => unverified.push(item(Some(&c.claim), *code, facts, evidence)),
            Verdict::Backed { .. } => {}
        }
    }
    for f in &a.findings {
        let facts = if f.paths.is_empty() { Value::Null } else { json!({"paths": f.paths}) };
        let entry = item(f.claim.as_deref(), f.code, &facts, &f.evidence);
        if f.code.builder_fixes() {
            fix.push(entry);
        } else {
            unverified.push(entry);
        }
    }
    let (backed, _, _) = tally(a);
    let block = json!({
        "protocol": "notary-bounce/1", "wo": a.wo, "sha": a.sha, "attempt": a.attempt, "max": max,
        "audit": audit_id, "fix": fix, "unverified": unverified, "backed": backed,
    });
    format!(
        "@{builder} {} bounced ({} of {max}): {} to fix. Report again with a new claims block; ask notary `show {}` for the evidence.\n```notary-bounce\n{}\n```",
        a.wo,
        a.attempt,
        fix.len(),
        a.wo,
        serde_json::to_string_pretty(&block).unwrap_or_default()
    )
}

/// The Notary's verdict on a report that isn't bounced, posted in the work room.
#[must_use]
pub fn verdict(a: &Audit, audit_id: &str, next: &Next) -> String {
    let (backed, contradicted, unverified) = tally(a);
    let next_name = match next {
        Next::Bounce => "bounce",
        Next::Ready { .. } => "ready",
        Next::Escalate => "escalate",
    };
    let checked: Vec<Value> = a.checked.iter().map(|c| json!({"claim": c.claim, "kind": c.kind, "code": c.verdict.code()})).collect();
    let findings: Vec<Value> = a.findings.iter().map(|f| json!({"code": f.code, "claim": f.claim, "paths": defuse_value(&json!(f.paths))})).collect();
    let block = json!({
        "protocol": "notary-verdict/1", "wo": a.wo, "sha": a.sha, "agent": a.agent, "stage": a.stage,
        "attempt": a.attempt, "audit": audit_id, "next": next_name, "checked": checked, "findings": findings,
    });
    format!(
        "{} {}: {} of {} claims backed at {} ({contradicted} contradicted, {unverified} unverified).\n```notary-verdict\n{}\n```",
        a.wo,
        match next {
            Next::Escalate => "escalated to a person",
            _ => "checked",
        },
        backed.len(),
        a.checked.len(),
        &a.sha[..7.min(a.sha.len())],
        serde_json::to_string_pretty(&block).unwrap_or_default()
    )
}

/// The one line the hub sees when a work order is ready for approval.
#[must_use]
pub fn hub_ready(wo: &str, builder: &Audit, reviewer: Option<&Audit>, flags: &[Code], head: &str) -> String {
    let (backed, _, _) = tally(builder);
    let review_note = reviewer.map_or(String::new(), |r| {
        let (b, _, _) = tally(r);
        format!(", review {}/{} backed", b.len(), r.checked.len())
    });
    let flagged = if flags.is_empty() {
        "nothing flagged".to_string()
    } else {
        format!("flagged: {}", flags.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "))
    };
    format!(
        "**{wo} ready for approval**: {}/{} claims backed at `{}` after {} report(s){review_note}; {flagged}. Ledger head `{}`. Details: room doc \"{wo} Receipt\", or `@notary show {wo}`.",
        backed.len(),
        builder.checked.len(),
        &builder.sha[..7.min(builder.sha.len())],
        builder.attempt,
        &head[..12.min(head.len())]
    )
}

/// The receipt document: verified claims only, then what couldn't be verified, then how to check it.
#[must_use]
pub fn receipt_doc(wo: &str, audits: &[(&str, &Audit)], decision: &str, head: &str, key: &str, entries: usize) -> String {
    let mut out = format!("# {wo} Receipt\n\nDecision: **{}**\n\n## Verified\n", defuse(decision));
    let mut unverified = String::new();
    for (id, a) in audits {
        for c in &a.checked {
            let who = format!("{} ({:?}, report {})", a.agent, a.stage, a.attempt);
            match &c.verdict {
                Verdict::Backed { evidence, .. } => {
                    let cited = evidence.iter().map(|e| format!("`{}`", short(e))).collect::<Vec<_>>().join(", ");
                    let _ = writeln!(out, "- {} `{}` by {who}: backed by {cited}", c.claim, c.kind);
                }
                Verdict::Contradicted { code, .. } => {
                    let _ = writeln!(unverified, "- {} `{}` by {who}: **contradicted** `{code}` (audit `{}`)", c.claim, c.kind, short(id));
                }
                Verdict::Unverified { code, .. } => {
                    let _ = writeln!(unverified, "- {} `{}` by {who}: unverified `{code}`: {}", c.claim, c.kind, code.meta().remediation);
                }
            }
        }
        for f in &a.findings {
            let paths = if f.paths.is_empty() { String::new() } else { format!(": {}", defuse(&f.paths.join(", "))) };
            let _ = writeln!(unverified, "- finding `{}`{paths}", f.code);
        }
    }
    if unverified.is_empty() {
        unverified.push_str("- nothing\n");
    }
    let _ = write!(
        out,
        "\n## Not verified\n{unverified}\n## Ledger\n- entries: {entries}\n- head: `{head}`\n- signed by: `{key}`\n\n## Check it yourself\nDownload `receipt-{}.json` from the hub and run `notary-verify receipt-{}.json`. It re-hashes every record, checks the chain ends at the signed head, and checks the signature. Change one byte and it fails.\n",
        wo.to_lowercase(),
        wo.to_lowercase()
    );
    out
}

/// Page `page` (1-based) of `lines`, 20 per page, each line clipped to 200 characters.
#[must_use]
pub fn page(title: &str, lines: &[String], page: usize) -> String {
    const PER: usize = 20;
    let total = lines.len();
    let pages = total.div_ceil(PER).max(1);
    let page = page.clamp(1, pages);
    let start = (page - 1) * PER;
    let mut out = format!("**{}**\n", defuse(title));
    for l in lines.iter().skip(start).take(PER) {
        let l: String = l.chars().take(200).collect();
        let _ = writeln!(out, "{}", defuse(&l));
    }
    let end = (start + PER).min(total);
    let _ = write!(out, "_showing {}-{end} of {total}_", if total == 0 { 0 } else { start + 1 });
    if page < pages {
        let _ = write!(out, "; next: `show {} page {}`", defuse(title.split_whitespace().next().unwrap_or(title)), page + 1);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::judge::{Checked, Finding, Stage};

    fn audit() -> Audit {
        Audit {
            protocol: "notary-audit/1".into(),
            wo: "WO-17".into(),
            sha: "3f9a2c1e0b7d4a5c6e8f9012345678901234abcd".into(),
            agent: "acme-builder".into(),
            stage: Stage::Builder,
            attempt: 1,
            report: "rp_x".into(),
            checked: vec![
                Checked {
                    claim: "c1".into(),
                    kind: "commit_pushed".into(),
                    verdict: Verdict::Backed { evidence: vec!["ev_aaaaaaaaaaaa".into()], limitations: vec![] },
                },
                Checked {
                    claim: "c3".into(),
                    kind: "tests_passed".into(),
                    verdict: Verdict::Contradicted {
                        code: Code::TestsFailed,
                        evidence: vec!["ev_bbbbbbbbbbbb".into()],
                        facts: json!({"failed": 1, "ids": ["tests/test_reports.py::x"]}),
                    },
                },
            ],
            findings: vec![Finding {
                code: Code::UndisclosedChange,
                claim: None,
                paths: vec!["@acme-reviewer.lock".into()],
                evidence: vec!["ev_cccccccccccc".into()],
            }],
        }
    }

    fn block(msg: &str, tag: &str) -> Value {
        let b = crate::claims::fenced_blocks(msg, tag);
        serde_json::from_str(b[0]).unwrap()
    }

    #[test]
    fn a_bounce_carries_codes_remediation_and_evidence() {
        let msg = bounce(&audit(), "au_1", 2, "builder");
        assert!(msg.starts_with("@builder WO-17 bounced (1 of 2): 2 to fix."));
        let b = block(&msg, "notary-bounce");
        assert_eq!(b["fix"][0]["code"], "tests_failed");
        assert_eq!(b["fix"][0]["ev"][0], "ev_bbbbbbbbbbbb");
        assert_eq!(b["fix"][1]["code"], "undisclosed_change");
        assert_eq!(b["backed"], json!(["c1"]));
    }

    #[test]
    fn agent_text_never_addresses_anyone() {
        let msg = bounce(&audit(), "au_1", 2, "builder");
        assert_eq!(msg.matches('@').count(), msg.matches("@\u{200b}").count() + 1, "only the leading @builder addresses");
        assert!(!msg.contains("@acme-reviewer"));
    }

    #[test]
    fn verdicts_and_pages_render() {
        let a = audit();
        let v = block(&verdict(&a, "au_1", &Next::Escalate), "notary-verdict");
        assert_eq!(v["next"], "escalate");
        assert_eq!(v["checked"][1]["code"], "tests_failed");
        let lines: Vec<String> = (1..=25).map(|i| format!("line {i}")).collect();
        let p1 = page("WO-17 evidence", &lines, 1);
        assert!(p1.contains("showing 1-20 of 25") && p1.contains("show WO-17 page 2"));
        assert!(page("WO-17", &lines, 9).contains("showing 21-25 of 25"));
    }
}
