//! The Reporting Contract: an agent's report ends with exactly one fenced `notary-claims`
//! block of strict JSON. Prose outside the block is never judged and never forwarded.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::errors::{Code, NotaryError};
use crate::ids;

pub const PROTOCOL: &str = "notary-claims/1";
const MAX_MESSAGE: usize = 64 * 1024;
const MAX_BLOCK: usize = 16 * 1024;
const MAX_CLAIMS: usize = 40;
const MAX_PATHS: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Full,
    Subset,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Passed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Claim {
    CommitPushed {
        id: String,
        branch: String,
        sha: String,
        #[serde(default)]
        cites: Vec<String>,
    },
    FilesChanged {
        id: String,
        paths: Vec<String>,
        #[serde(default)]
        cites: Vec<String>,
    },
    TestsPassed {
        id: String,
        scope: Scope,
        command: String,
        #[serde(default)]
        paths: Vec<String>,
        #[serde(default)]
        cites: Vec<String>,
    },
    CheckRan {
        id: String,
        command: String,
        outcome: RunOutcome,
        #[serde(default)]
        cites: Vec<String>,
    },
    ReviewedDiff {
        id: String,
        sha: String,
        #[serde(default)]
        cites: Vec<String>,
    },
    NoOtherChanges {
        id: String,
        #[serde(default)]
        cites: Vec<String>,
    },
}

impl Claim {
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::CommitPushed { id, .. }
            | Self::FilesChanged { id, .. }
            | Self::TestsPassed { id, .. }
            | Self::CheckRan { id, .. }
            | Self::ReviewedDiff { id, .. }
            | Self::NoOtherChanges { id, .. } => id,
        }
    }

    #[must_use]
    pub fn cites(&self) -> &[String] {
        match self {
            Self::CommitPushed { cites, .. }
            | Self::FilesChanged { cites, .. }
            | Self::TestsPassed { cites, .. }
            | Self::CheckRan { cites, .. }
            | Self::ReviewedDiff { cites, .. }
            | Self::NoOtherChanges { cites, .. } => cites,
        }
    }

    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::CommitPushed { .. } => "commit_pushed",
            Self::FilesChanged { .. } => "files_changed",
            Self::TestsPassed { .. } => "tests_passed",
            Self::CheckRan { .. } => "check_ran",
            Self::ReviewedDiff { .. } => "reviewed_diff",
            Self::NoOtherChanges { .. } => "no_other_changes",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub protocol: String,
    pub wo: String,
    pub sha: String,
    pub claims: Vec<Claim>,
}

impl Report {
    /// The branch named by the report's `commit_pushed` claim, if any.
    #[must_use]
    pub fn branch(&self) -> Option<&str> {
        self.claims.iter().find_map(|c| if let Claim::CommitPushed { branch, .. } = c { Some(branch.as_str()) } else { None })
    }
}

/// The contents of every fenced block tagged `tag` in `message`.
#[must_use]
pub fn fenced_blocks<'a>(message: &'a str, tag: &str) -> Vec<&'a str> {
    let mut blocks = Vec::new();
    let mut rest = message;
    let open = format!("```{tag}");
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        // The tag must end the fence line ("```notary-claims" not "```notary-claimsX").
        let Some(nl) = after.find('\n') else { break };
        if !after[..nl].trim().is_empty() {
            rest = &after[nl..];
            continue;
        }
        let body = &after[nl + 1..];
        let Some(end) = body.find("```") else { break };
        blocks.push(&body[..end]);
        rest = &body[end + 3..];
    }
    blocks
}

fn invalid(why: impl Into<String>) -> NotaryError {
    NotaryError::new(Code::BlockInvalid, why)
}

/// Whether `p` is a plain repository path: relative, no `..`, no globs, at most 300 bytes.
#[must_use]
pub fn is_repo_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 300
        && !p.starts_with('/')
        && !p.contains('\\')
        && !p.contains(['*', '?', '[', '\n', '\0'])
        && p.split('/').all(|seg| !seg.is_empty() && seg != ".." && seg != ".")
}

/// Parse the one claims block in `message`, strictly.
///
/// # Errors
/// `block_missing` if there is none; `block_invalid` for anything else that isn't exactly right.
pub fn parse(message: &str) -> Result<Report, NotaryError> {
    if message.len() > MAX_MESSAGE {
        return Err(invalid("the message is over 64 KiB"));
    }
    let blocks = fenced_blocks(message, "notary-claims");
    let block = match blocks.as_slice() {
        [] => {
            return Err(NotaryError::new(Code::BlockMissing, "no notary-claims block"));
        }
        [one] => *one,
        _ => {
            return Err(invalid(format!("{} notary-claims blocks; send exactly one", blocks.len())));
        }
    };
    if block.len() > MAX_BLOCK {
        return Err(invalid("the block is over 16 KiB"));
    }
    let report: Report = serde_json::from_str(block).map_err(|e| invalid(e.to_string()))?;
    if report.protocol != PROTOCOL {
        return Err(invalid(format!("protocol must be {PROTOCOL}")));
    }
    if !ids::is_sha(&report.sha) {
        return Err(invalid("sha must be the full 40-character commit id"));
    }
    if report.claims.is_empty() || report.claims.len() > MAX_CLAIMS {
        return Err(invalid("1 to 40 claims"));
    }
    let mut seen = BTreeSet::new();
    let mut branches = 0;
    for c in &report.claims {
        let id = c.id();
        let numeric = id.strip_prefix('c').is_some_and(|n| (1..=3).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit()));
        if !numeric || !seen.insert(id.to_string()) {
            return Err(invalid(format!("claim id {id:?} must be unique and look like c1..c999")));
        }
        if let Some(bad) = c.cites().iter().find(|e| !ids::is_id("ev", e)) {
            return Err(invalid(format!("{id}: {bad:?} isn't an ev_ id")));
        }
        match c {
            Claim::CommitPushed { branch, sha, .. } => {
                branches += 1;
                if sha != &report.sha {
                    return Err(invalid(format!("{id}: its sha must equal the report's sha")));
                }
                if branch.is_empty()
                    || branch.len() > 200
                    || branch.starts_with('-')
                    || branch.contains(['\0', ' ', '\n', '~', '^', ':', '\\'])
                    || branch.contains("..")
                {
                    return Err(invalid(format!("{id}: {branch:?} isn't a plain branch name")));
                }
            }
            Claim::ReviewedDiff { sha, .. } if sha != &report.sha => {
                return Err(invalid(format!("{id}: its sha must equal the report's sha")));
            }
            Claim::FilesChanged { paths, .. } | Claim::TestsPassed { paths, .. }
                if paths.len() > MAX_PATHS || paths.iter().any(|p| !is_repo_path(p)) =>
            {
                return Err(invalid(format!("{id}: paths must be plain relative repository paths, at most 200")));
            }
            _ => {}
        }
        if let Claim::TestsPassed { command, .. } | Claim::CheckRan { command, .. } = c
            && (command.is_empty() || command.len() > 500)
        {
            return Err(invalid(format!("{id}: command must be 1 to 500 characters")));
        }
    }
    if branches > 1 {
        return Err(invalid("at most one commit_pushed claim"));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "3f9a2c1e0b7d4a5c6e8f9012345678901234abcd";

    fn message(block: &str) -> String {
        format!("@agora-notary WO-17 ready.\n```notary-claims\n{block}\n```\nthanks")
    }

    fn good() -> String {
        format!(
            r#"{{"protocol":"notary-claims/1","wo":"WO-17","sha":"{SHA}","claims":[
              {{"id":"c1","kind":"commit_pushed","branch":"wo-17","sha":"{SHA}"}},
              {{"id":"c2","kind":"files_changed","paths":["src/invoices/tax.py"]}},
              {{"id":"c3","kind":"tests_passed","scope":"full","command":"pytest -q"}},
              {{"id":"c4","kind":"check_ran","command":"pytest -q","outcome":"passed"}}]}}"#
        )
    }

    #[test]
    fn a_good_report_parses() {
        let r = parse(&message(&good())).unwrap();
        assert_eq!(r.claims.len(), 4);
        assert_eq!(r.branch(), Some("wo-17"));
        assert_eq!(r.claims[2].kind(), "tests_passed");
    }

    #[test]
    fn missing_and_doubled_blocks_are_refused() {
        assert_eq!(parse("all done, tests pass").unwrap_err().code, Code::BlockMissing);
        let two = format!("{}\n{}", message(&good()), message(&good()));
        assert_eq!(parse(&two).unwrap_err().code, Code::BlockInvalid);
    }

    #[test]
    fn unknown_fields_bad_ids_and_short_shas_are_refused() {
        for bad in [
            good().replace(r#""wo":"WO-17""#, r#""wo":"WO-17","extra":1"#),
            good().replace(r#""command":"pytest -q","outcome""#, r#""command":"pytest -q","why":"x","outcome""#),
            good().replace(r#""id":"c2""#, r#""id":"c1""#),
            good().replace(r#""id":"c2""#, r#""id":"claim2""#),
            good().replace(&format!(r#""sha":"{SHA}","claims""#), r#""sha":"3f9a2c1","claims""#),
            good().replace("src/invoices/tax.py", "../etc/passwd"),
            good().replace("src/invoices/tax.py", "src/**"),
            good().replace(r#""kind":"files_changed""#, r#""kind":"deployed""#),
        ] {
            assert_eq!(parse(&message(&bad)).unwrap_err().code, Code::BlockInvalid, "{bad}");
        }
    }

    #[test]
    fn duplicate_keys_are_refused() {
        let dup = good().replace(r#""branch":"wo-17""#, r#""branch":"wo-17","branch":"main""#);
        assert_eq!(parse(&message(&dup)).unwrap_err().code, Code::BlockInvalid);
    }

    #[test]
    fn citations_must_look_like_evidence_ids() {
        let cited = good().replace(r#""outcome":"passed""#, &format!(r#""outcome":"passed","cites":["ev_{}"]"#, "a".repeat(64)));
        assert!(parse(&message(&cited)).is_ok());
        let bogus = good().replace(r#""outcome":"passed""#, r#""outcome":"passed","cites":["trust me"]"#);
        assert_eq!(parse(&message(&bogus)).unwrap_err().code, Code::BlockInvalid);
    }

    #[test]
    fn a_fence_with_a_longer_tag_is_not_ours() {
        let other = format!("```notary-claimsX\n{}\n```", good());
        assert_eq!(parse(&other).unwrap_err().code, Code::BlockMissing);
    }
}
