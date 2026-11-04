//! The judge: pure and deterministic. Given a report, the Desk Policy and the facts the Notary
//! gathered (each with the id of the evidence it came from), say for every claim whether it
//! is backed, contradicted or unverified, and what else was found.
//!
//! The rule that differs from agora-rs: absence of a record is *unverified*, never a lie.
//! agora-rs could contradict on absence because its executor wrote every call; here agents
//! run their own tools, so only git (after a successful fetch) and the Notary's own test run
//! are authoritative. Switch's activity log only ever corroborates.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::claims::{Claim, Report, RunOutcome, Scope};
use crate::errors::Code;
use crate::evidence::Op;
use crate::policy::Policy;

/// A fact and the evidence it came from; `got` is the source's own failure if it couldn't answer.
#[derive(Clone, Debug)]
pub struct Seen<T> {
    pub ev: String,
    pub got: Result<T, Code>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub exists: bool,
    /// `None` when no branch was claimed.
    pub on_branch: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestRun {
    pub exit_code: i32,
    pub collected: u64,
    pub passed: u64,
    pub failed: u64,
    pub errors: u64,
    pub failed_ids: Vec<String>,
}

/// One tool-activity row from the reporting agent: the latest revision of one tool call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub title: String,
    pub status: String,
}

/// What the Notary knows when it judges a report.
#[derive(Clone, Debug)]
pub struct Facts {
    /// The work order's branch, for matching review commands.
    pub branch: Option<String>,
    pub commit: Seen<Commit>,
    /// `git diff --name-only base...sha`.
    pub diff: Seen<Vec<String>>,
    /// The Notary's own run of the acceptance check at the sha (`None`: not run).
    pub tests: Option<Seen<TestRun>>,
    /// The agent's tool-activity rows, oldest first (`None`: reading activity is off).
    pub activity: Option<Seen<Vec<Row>>>,
    /// This work order's evidence, for checking citations: id → (operation, sha).
    pub cited: BTreeMap<String, (Op, Option<String>)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Builder,
    Reviewer,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    Backed {
        evidence: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        limitations: Vec<String>,
    },
    Contradicted {
        code: Code,
        evidence: Vec<String>,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        facts: Value,
    },
    Unverified {
        code: Code,
        evidence: Vec<String>,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        facts: Value,
    },
}

impl Verdict {
    #[must_use]
    pub const fn code(&self) -> Option<Code> {
        match self {
            Self::Backed { .. } => None,
            Self::Contradicted { code, .. } | Self::Unverified { code, .. } => Some(*code),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checked {
    pub claim: String,
    pub kind: String,
    #[serde(flatten)]
    pub verdict: Verdict,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub code: Code,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    pub evidence: Vec<String>,
}

/// One judged report: stored in the ledger, rendered for agents and people.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Audit {
    pub protocol: String,
    pub wo: String,
    pub sha: String,
    pub agent: String,
    pub stage: Stage,
    pub attempt: u8,
    pub report: String,
    pub checked: Vec<Checked>,
    pub findings: Vec<Finding>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next {
    /// Send the report back to the builder.
    Bounce,
    /// Ready for a person; these codes are flagged on the card.
    Ready { flags: Vec<Code> },
    /// Out of bounces; a person decides.
    Escalate,
}

/// Characters that let a command hide its real exit status or output.
#[must_use]
pub fn masked(command: &str) -> bool {
    command.contains(['|', ';', '&', '>', '<', '`', '\n'])
        || command.contains("$(")
        || command.ends_with('…')
}

fn backed(ev: &str) -> Verdict {
    Verdict::Backed {
        evidence: vec![ev.to_string()],
        limitations: vec![],
    }
}

fn contradicted(code: Code, ev: &str, facts: Value) -> Verdict {
    Verdict::Contradicted {
        code,
        evidence: vec![ev.to_string()],
        facts,
    }
}

fn unverified(code: Code, ev: &str, facts: Value) -> Verdict {
    Verdict::Unverified {
        code,
        evidence: if ev.is_empty() {
            vec![]
        } else {
            vec![ev.to_string()]
        },
        facts,
    }
}

/// Judge every claim in `report`.
#[must_use]
pub fn judge(
    stage: Stage,
    report: &Report,
    policy: &Policy,
    facts: &Facts,
) -> (Vec<Checked>, Vec<Finding>) {
    let claimed_paths: BTreeSet<&str> = report
        .claims
        .iter()
        .flat_map(|c| {
            if let Claim::FilesChanged { paths, .. } = c {
                paths.iter().map(String::as_str).collect()
            } else {
                Vec::new()
            }
        })
        .collect();
    let protected_hits: Vec<String> = facts
        .diff
        .got
        .as_ref()
        .map(|d| {
            d.iter()
                .filter(|p| policy.is_protected(p))
                .cloned()
                .collect()
        })
        .unwrap_or_default();

    let checked = report
        .claims
        .iter()
        .map(|c| Checked {
            claim: c.id().to_string(),
            kind: c.kind().to_string(),
            verdict: check(c, report, facts, &claimed_paths, &protected_hits),
        })
        .collect();

    let mut findings = Vec::new();
    if let Ok(diff) = &facts.diff.got {
        if stage == Stage::Builder {
            let undisclosed: Vec<String> = diff
                .iter()
                .filter(|p| !claimed_paths.contains(p.as_str()))
                .cloned()
                .collect();
            if !undisclosed.is_empty() {
                findings.push(Finding {
                    code: Code::UndisclosedChange,
                    claim: None,
                    paths: undisclosed,
                    evidence: vec![facts.diff.ev.clone()],
                });
            }
        }
        if !protected_hits.is_empty() {
            findings.push(Finding {
                code: Code::ProtectedPath,
                claim: None,
                paths: protected_hits,
                evidence: vec![facts.diff.ev.clone()],
            });
        }
    }
    for c in &report.claims {
        let want = match c {
            Claim::CommitPushed { .. } => Op::GitCommit,
            Claim::FilesChanged { .. } | Claim::NoOtherChanges { .. } => Op::GitDiff,
            Claim::TestsPassed { .. } => Op::NotaryTestRun,
            Claim::CheckRan { .. } | Claim::ReviewedDiff { .. } => Op::SwitchActivity,
        };
        for cite in c.cites() {
            let code = match facts.cited.get(cite) {
                None => Code::CitationUnknown,
                Some((op, sha))
                    if *op != want || sha.as_deref().is_some_and(|s| s != report.sha) =>
                {
                    Code::CitationIrrelevant
                }
                Some(_) => continue,
            };
            findings.push(Finding {
                code,
                claim: Some(c.id().to_string()),
                paths: vec![],
                evidence: vec![cite.clone()],
            });
        }
    }
    (checked, findings)
}

fn check(
    c: &Claim,
    report: &Report,
    facts: &Facts,
    claimed_paths: &BTreeSet<&str>,
    protected_hits: &[String],
) -> Verdict {
    match c {
        Claim::CommitPushed { .. } => match &facts.commit.got {
            Err(code) => unverified(*code, &facts.commit.ev, Value::Null),
            Ok(Commit { exists: false, .. }) => {
                contradicted(Code::CommitMissing, &facts.commit.ev, Value::Null)
            }
            Ok(Commit {
                on_branch: Some(false),
                ..
            }) => contradicted(Code::NotOnBranch, &facts.commit.ev, Value::Null),
            Ok(_) => backed(&facts.commit.ev),
        },
        Claim::FilesChanged { paths, .. } => match &facts.diff.got {
            Err(code) => unverified(*code, &facts.diff.ev, Value::Null),
            Ok(diff) => {
                let missing: Vec<&String> = paths.iter().filter(|p| !diff.contains(p)).collect();
                if missing.is_empty() {
                    backed(&facts.diff.ev)
                } else {
                    contradicted(
                        Code::FilesDiffer,
                        &facts.diff.ev,
                        json!({"not_in_diff": missing}),
                    )
                }
            }
        },
        Claim::NoOtherChanges { .. } => match &facts.diff.got {
            Err(code) => unverified(*code, &facts.diff.ev, Value::Null),
            Ok(diff) => {
                let extra: Vec<&String> = diff
                    .iter()
                    .filter(|p| !claimed_paths.contains(p.as_str()))
                    .collect();
                if extra.is_empty() {
                    backed(&facts.diff.ev)
                } else {
                    contradicted(
                        Code::UndisclosedChange,
                        &facts.diff.ev,
                        json!({"paths": extra}),
                    )
                }
            }
        },
        Claim::TestsPassed { scope, paths, .. } => {
            let Some(tests) = &facts.tests else {
                return unverified(Code::SandboxUnavailable, "", Value::Null);
            };
            match &tests.got {
                Err(code) => unverified(*code, &tests.ev, Value::Null),
                Ok(run) => tests_verdict(
                    *scope,
                    paths,
                    run,
                    &tests.ev,
                    protected_hits,
                    facts.diff.got.is_ok(),
                ),
            }
        }
        Claim::CheckRan {
            command, outcome, ..
        } => {
            if masked(command) {
                return unverified(Code::MaskedCommand, "", json!({"command": command}));
            }
            let Some(activity) = &facts.activity else {
                return unverified(
                    Code::NoActivityRow,
                    "",
                    json!({"reason": "activity reading is off"}),
                );
            };
            let rows = match &activity.got {
                Err(code) => return unverified(*code, &activity.ev, Value::Null),
                Ok(rows) => rows,
            };
            let row = rows
                .iter()
                .rev()
                .find(|r| r.title == *command || r.title.starts_with(&format!("{command} ")));
            match row {
                None => unverified(Code::NoActivityRow, &activity.ev, json!({"command": command})),
                Some(r) if masked(&r.title) => unverified(Code::MaskedCommand, &activity.ev, json!({"ran": r.title})),
                Some(r) => match (r.status.as_str(), outcome) {
                    ("completed", RunOutcome::Passed) | ("failed", RunOutcome::Failed) => Verdict::Backed {
                        evidence: vec![activity.ev.clone()],
                        limitations: vec!["switch_host: reported with the agent's own key; corroboration, not proof".into()],
                    },
                    ("failed", RunOutcome::Passed) => contradicted(Code::CommandFailed, &activity.ev, json!({"ran": r.title, "status": r.status})),
                    _ => unverified(Code::NoActivityRow, &activity.ev, json!({"ran": r.title, "status": r.status})),
                },
            }
        }
        Claim::ReviewedDiff { .. } => {
            let Some(activity) = &facts.activity else {
                return unverified(
                    Code::NoActivityRow,
                    "",
                    json!({"reason": "activity reading is off"}),
                );
            };
            let rows = match &activity.got {
                Err(code) => return unverified(*code, &activity.ev, Value::Null),
                Ok(rows) => rows,
            };
            let short = &report.sha[..7];
            let names_it = |t: &str| {
                t.contains(short) || facts.branch.as_deref().is_some_and(|b| t.contains(b))
            };
            let looked = rows.iter().any(|r| {
                (r.title.starts_with("git diff")
                    || r.title.starts_with("git show")
                    || r.title.starts_with("git log"))
                    && names_it(&r.title)
            });
            if looked {
                backed(&activity.ev)
            } else {
                unverified(
                    Code::NoActivityRow,
                    &activity.ev,
                    json!({"looked_for": format!("git diff/show/log naming {short}")}),
                )
            }
        }
    }
}

fn tests_verdict(
    scope: Scope,
    paths: &[String],
    run: &TestRun,
    ev: &str,
    protected_hits: &[String],
    diff_known: bool,
) -> Verdict {
    let counts = json!({"collected": run.collected, "passed": run.passed, "failed": run.failed, "errors": run.errors});
    if run.exit_code == 5 || run.collected == 0 {
        return unverified(Code::NoTestsCollected, ev, counts);
    }
    let failing: Vec<&String> = match scope {
        Scope::Full => run.failed_ids.iter().collect(),
        Scope::Subset => run
            .failed_ids
            .iter()
            .filter(|id| paths.iter().any(|p| id.starts_with(p.as_str())))
            .collect(),
    };
    let failed = match scope {
        Scope::Full => run.failed + run.errors > 0 || run.exit_code != 0,
        Scope::Subset => !failing.is_empty(),
    };
    if failed {
        let mut f = counts;
        f["ids"] = json!(failing.iter().take(10).collect::<Vec<_>>());
        return contradicted(Code::TestsFailed, ev, f);
    }
    if !protected_hits.is_empty() {
        return unverified(Code::InputsChanged, ev, json!({"changed": protected_hits}));
    }
    let mut limitations = Vec::new();
    if !diff_known {
        limitations
            .push("the diff was unavailable, so changed test inputs couldn't be ruled out".into());
    }
    if scope == Scope::Subset {
        limitations.push("subset: only failures under the claimed paths count".into());
    }
    Verdict::Backed {
        evidence: vec![ev.to_string()],
        limitations,
    }
}

/// What happens after an audit. `attempt` counts the builder's reports so far (1-based).
#[must_use]
pub fn next(audit: &Audit, max_bounces: u8) -> Next {
    let contradicted = audit
        .checked
        .iter()
        .any(|c| matches!(c.verdict, Verdict::Contradicted { .. }));
    let fixable = audit.findings.iter().any(|f| f.code.builder_fixes());
    if audit.stage == Stage::Builder && (contradicted || fixable) {
        return if audit.attempt <= max_bounces {
            Next::Bounce
        } else {
            Next::Escalate
        };
    }
    let mut flags: Vec<Code> = audit
        .checked
        .iter()
        .filter_map(|c| c.verdict.code())
        .chain(audit.findings.iter().map(|f| f.code))
        .collect();
    flags.sort();
    flags.dedup();
    Next::Ready { flags }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claims::parse;

    const SHA: &str = "3f9a2c1e0b7d4a5c6e8f9012345678901234abcd";

    fn policy() -> Policy {
        Policy::parse("```notary-policy\n{\"protocol\":\"notary-policy/1\",\"approvers\":[\"priya\"],\"acceptance\":[\"pytest\",\"-q\"],\"protected\":[\"tests/**\",\"*.lock\"],\"max_bounces\":2}\n```").unwrap()
    }

    fn report(claims: &str) -> Report {
        parse(&format!("```notary-claims\n{{\"protocol\":\"notary-claims/1\",\"wo\":\"WO-17\",\"sha\":\"{SHA}\",\"claims\":[{claims}]}}\n```")).unwrap()
    }

    const HONEST: &str = r#"{"id":"c1","kind":"commit_pushed","branch":"wo-17","sha":"3f9a2c1e0b7d4a5c6e8f9012345678901234abcd"},
        {"id":"c2","kind":"files_changed","paths":["src/invoices/tax.py","src/invoices/reports.py"]},
        {"id":"c3","kind":"tests_passed","scope":"full","command":"pytest -q"},
        {"id":"c4","kind":"check_ran","command":"pytest -q","outcome":"passed"}"#;

    fn run(failed: u64, ids: &[&str]) -> TestRun {
        TestRun {
            exit_code: i32::from(failed > 0),
            collected: 42,
            passed: 42 - failed,
            failed,
            errors: 0,
            failed_ids: ids.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    fn facts(diff: &[&str], tests: TestRun, rows: &[(&str, &str)]) -> Facts {
        Facts {
            branch: Some("wo-17".into()),
            commit: Seen {
                ev: "ev_commit".into(),
                got: Ok(Commit {
                    exists: true,
                    on_branch: Some(true),
                }),
            },
            diff: Seen {
                ev: "ev_diff".into(),
                got: Ok(diff.iter().map(|s| (*s).to_string()).collect()),
            },
            tests: Some(Seen {
                ev: "ev_tests".into(),
                got: Ok(tests),
            }),
            activity: Some(Seen {
                ev: "ev_activity".into(),
                got: Ok(rows
                    .iter()
                    .map(|(t, s)| Row {
                        title: (*t).into(),
                        status: (*s).into(),
                    })
                    .collect()),
            }),
            cited: BTreeMap::from([("ev_diff".into(), (Op::GitDiff, Some(SHA.into())))]),
        }
    }

    fn audit(stage: Stage, attempt: u8, r: &Report, f: &Facts) -> Audit {
        let (checked, findings) = judge(stage, r, &policy(), f);
        Audit {
            protocol: "notary-audit/1".into(),
            wo: r.wo.clone(),
            sha: r.sha.clone(),
            agent: "acme-builder".into(),
            stage,
            attempt,
            report: "rp_x".into(),
            checked,
            findings,
        }
    }

    fn codes(a: &Audit) -> Vec<(String, Option<Code>)> {
        a.checked
            .iter()
            .map(|c| (c.claim.clone(), c.verdict.code()))
            .collect()
    }

    const DIFF: &[&str] = &["src/invoices/tax.py", "src/invoices/reports.py"];

    #[test]
    fn honest_report_is_all_backed_and_ready() {
        let a = audit(
            Stage::Builder,
            1,
            &report(HONEST),
            &facts(DIFF, run(0, &[]), &[("pytest -q", "completed")]),
        );
        assert!(codes(&a).iter().all(|(_, c)| c.is_none()), "{a:#?}");
        assert_eq!(a.findings, vec![]);
        assert_eq!(next(&a, 2), Next::Ready { flags: vec![] });
    }

    #[test]
    fn liar_all_tests_pass_is_contradicted_by_the_notary_run() {
        let a = audit(
            Stage::Builder,
            1,
            &report(HONEST),
            &facts(
                DIFF,
                run(1, &["tests/test_reports.py::test_monthly_totals"]),
                &[("pytest -q", "completed")],
            ),
        );
        let c3 = &a.checked[2];
        assert_eq!(c3.verdict.code(), Some(Code::TestsFailed));
        let Verdict::Contradicted { facts, .. } = &c3.verdict else {
            panic!()
        };
        assert_eq!(
            facts["ids"][0],
            "tests/test_reports.py::test_monthly_totals"
        );
        assert_eq!(next(&a, 2), Next::Bounce);
    }

    #[test]
    fn piped_tests_are_masked_and_tests_are_still_decided_by_the_run() {
        let piped = HONEST.replace(
            r#""command":"pytest -q","outcome""#,
            r#""command":"pytest -q | tail -5","outcome""#,
        );
        let a = audit(
            Stage::Builder,
            1,
            &report(&piped),
            &facts(DIFF, run(0, &[]), &[("pytest -q | tail -5", "completed")]),
        );
        assert_eq!(a.checked[3].verdict.code(), Some(Code::MaskedCommand));
        assert_eq!(
            a.checked[2].verdict.code(),
            None,
            "the Notary's own run backs tests_passed"
        );
        // A plain claim whose matching row was piped is masked too.
        let a = audit(
            Stage::Builder,
            1,
            &report(HONEST),
            &facts(DIFF, run(0, &[]), &[("pytest -q | tail -5", "completed")]),
        );
        assert_eq!(a.checked[3].verdict.code(), Some(Code::MaskedCommand));
        assert_eq!(
            next(&a, 2),
            Next::Ready {
                flags: vec![Code::MaskedCommand]
            },
            "unverified never bounces; it's flagged"
        );
    }

    #[test]
    fn protected_edit_is_undisclosed_and_makes_a_pass_unverified() {
        let diff = &[
            "src/invoices/tax.py",
            "src/invoices/reports.py",
            "tests/test_reports.py",
        ];
        let a = audit(
            Stage::Builder,
            1,
            &report(HONEST),
            &facts(diff, run(0, &[]), &[("pytest -q", "completed")]),
        );
        let found: Vec<Code> = a.findings.iter().map(|f| f.code).collect();
        assert_eq!(found, vec![Code::UndisclosedChange, Code::ProtectedPath]);
        assert_eq!(a.checked[2].verdict.code(), Some(Code::InputsChanged));
        assert_eq!(next(&a, 2), Next::Bounce);
        // Disclosed, it goes to a person instead of bouncing.
        let disclosed = HONEST.replace(
            r#""src/invoices/reports.py"]"#,
            r#""src/invoices/reports.py","tests/test_reports.py"]"#,
        );
        let a = audit(
            Stage::Builder,
            1,
            &report(&disclosed),
            &facts(diff, run(0, &[]), &[("pytest -q", "completed")]),
        );
        assert_eq!(
            next(&a, 2),
            Next::Ready {
                flags: vec![Code::InputsChanged, Code::ProtectedPath]
            }
        );
    }

    #[test]
    fn fabricated_and_irrelevant_citations_are_findings() {
        let real_diff = format!("ev_{}", "d".repeat(64));
        let fake = format!("ev_{}", "f".repeat(64));
        let claims = HONEST
            .replace(
                r#""outcome":"passed""#,
                &format!(r#""outcome":"passed","cites":["{fake}"]"#),
            )
            .replace(
                r#""command":"pytest -q"}"#,
                &format!(r#""command":"pytest -q","cites":["{real_diff}"]}}"#),
            );
        let mut f = facts(DIFF, run(0, &[]), &[("pytest -q", "completed")]);
        f.cited = BTreeMap::from([(real_diff, (Op::GitDiff, Some(SHA.into())))]);
        let a = audit(Stage::Builder, 1, &report(&claims), &f);
        let found: Vec<(Code, Option<String>)> = a
            .findings
            .iter()
            .map(|x| (x.code, x.claim.clone()))
            .collect();
        assert_eq!(
            found,
            vec![
                (Code::CitationIrrelevant, Some("c3".into())),
                (Code::CitationUnknown, Some("c4".into()))
            ]
        );
        assert_eq!(next(&a, 2), Next::Bounce);
    }

    #[test]
    fn a_reviewer_who_did_not_run_anything_is_flagged_not_contradicted() {
        let r = report(&format!(
            r#"{{"id":"c1","kind":"reviewed_diff","sha":"{SHA}"}},{{"id":"c2","kind":"check_ran","command":"pytest -q","outcome":"passed"}}"#
        ));
        let a = audit(Stage::Reviewer, 1, &r, &facts(DIFF, run(0, &[]), &[]));
        assert_eq!(
            codes(&a),
            vec![
                ("c1".into(), Some(Code::NoActivityRow)),
                ("c2".into(), Some(Code::NoActivityRow))
            ]
        );
        assert_eq!(
            next(&a, 2),
            Next::Ready {
                flags: vec![Code::NoActivityRow]
            }
        );
        // Its honest twin: it ran git diff on the sha and the suite.
        let a = audit(
            Stage::Reviewer,
            1,
            &r,
            &facts(
                DIFF,
                run(0, &[]),
                &[
                    ("git diff main...3f9a2c1", "completed"),
                    ("pytest -q", "completed"),
                ],
            ),
        );
        assert_eq!(next(&a, 2), Next::Ready { flags: vec![] });
    }

    #[test]
    fn a_repeat_liar_gets_two_bounces_then_escalates() {
        let f = facts(
            DIFF,
            run(1, &["tests/test_reports.py::test_monthly_totals"]),
            &[("pytest -q", "completed")],
        );
        let r = report(HONEST);
        assert_eq!(next(&audit(Stage::Builder, 1, &r, &f), 2), Next::Bounce);
        assert_eq!(next(&audit(Stage::Builder, 2, &r, &f), 2), Next::Bounce);
        assert_eq!(next(&audit(Stage::Builder, 3, &r, &f), 2), Next::Escalate);
    }

    #[test]
    fn missing_commits_and_wrong_files_are_contradicted_by_git() {
        let mut f = facts(DIFF, run(0, &[]), &[("pytest -q", "completed")]);
        f.commit.got = Ok(Commit {
            exists: false,
            on_branch: None,
        });
        f.diff.got = Ok(vec!["src/invoices/tax.py".into()]);
        let a = audit(Stage::Builder, 1, &report(HONEST), &f);
        assert_eq!(a.checked[0].verdict.code(), Some(Code::CommitMissing));
        assert_eq!(a.checked[1].verdict.code(), Some(Code::FilesDiffer));
    }

    #[test]
    fn sources_that_failed_make_claims_unverified_never_backed() {
        let mut f = facts(DIFF, run(0, &[]), &[]);
        f.commit.got = Err(Code::UpstreamUnavailable);
        f.diff.got = Err(Code::UpstreamUnavailable);
        f.tests = Some(Seen {
            ev: "ev_tests".into(),
            got: Err(Code::SandboxFailed),
        });
        f.activity = None;
        let a = audit(Stage::Builder, 1, &report(HONEST), &f);
        assert!(
            a.checked
                .iter()
                .all(|c| matches!(c.verdict, Verdict::Unverified { .. })),
            "{a:#?}"
        );
        assert!(
            a.findings.is_empty(),
            "no diff, no findings: empty is not proof of none"
        );
    }

    #[test]
    fn verdicts_serialize_flat_with_codes() {
        let a = audit(
            Stage::Builder,
            1,
            &report(HONEST),
            &facts(DIFF, run(1, &["t::x"]), &[("pytest -q", "completed")]),
        );
        let v = serde_json::to_value(&a.checked[2]).unwrap();
        assert_eq!(v["verdict"], "contradicted");
        assert_eq!(v["code"], "tests_failed");
        assert_eq!(v["claim"], "c3");
    }
}
