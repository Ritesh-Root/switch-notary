//! The Desk Policy: a room document in Switch whose fenced `notary-policy` block says who
//! approves, how acceptance is checked, which paths are protected and how many bounces a
//! work order gets. The Notary reads it from the room at intake, so the policy lives with
//! the team, not in the Notary's config.

use serde::{Deserialize, Serialize};

use crate::claims::fenced_blocks;
use crate::errors::{Code, NotaryError};

pub const PROTOCOL: &str = "notary-policy/1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub protocol: String,
    /// Mattermost usernames whose approval counts.
    pub approvers: Vec<String>,
    /// The acceptance check, as argv (never through a shell), run by the Notary in its sandbox.
    pub acceptance: Vec<String>,
    /// Patterns: `dir/**`, an exact path, or a file name with at most one `*` (any directory).
    pub protected: Vec<String>,
    pub max_bounces: u8,
}

impl Policy {
    /// Parse the policy block out of a Desk Policy document.
    ///
    /// # Errors
    /// `configuration_invalid` naming what's wrong.
    pub fn parse(doc: &str) -> Result<Self, NotaryError> {
        let bad = |why: String| NotaryError::new(Code::ConfigurationInvalid, format!("Desk Policy: {why}"));
        let blocks = fenced_blocks(doc, "notary-policy");
        let [block] = blocks.as_slice() else { return Err(bad(format!("expected one notary-policy block, found {}", blocks.len()))) };
        let p: Self = serde_json::from_str(block).map_err(|e| bad(e.to_string()))?;
        if p.protocol != PROTOCOL {
            return Err(bad(format!("protocol must be {PROTOCOL}")));
        }
        if p.approvers.is_empty() || p.acceptance.is_empty() {
            return Err(bad("approvers and acceptance can't be empty".into()));
        }
        if p.max_bounces == 0 || p.max_bounces > 5 {
            return Err(bad("max_bounces must be 1 to 5".into()));
        }
        Ok(p)
    }

    #[must_use]
    pub fn is_protected(&self, path: &str) -> bool {
        self.protected.iter().any(|pat| matches(pat, path))
    }

    #[must_use]
    pub fn is_approver(&self, username: &str) -> bool {
        self.approvers.iter().any(|a| a.eq_ignore_ascii_case(username))
    }
}

fn matches(pattern: &str, path: &str) -> bool {
    if let Some(dir) = pattern.strip_suffix("/**") {
        return path.starts_with(&format!("{dir}/"));
    }
    if pattern.contains('/') {
        return pattern == path;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    match pattern.split_once('*') {
        None => pattern == name,
        Some((pre, post)) => name.len() >= pre.len() + post.len() && name.starts_with(pre) && name.ends_with(post),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "Approvers sign off here.\n```notary-policy\n{\"protocol\":\"notary-policy/1\",\"approvers\":[\"priya\"],\"acceptance\":[\"pytest\",\"-q\"],\"protected\":[\"tests/**\",\".github/**\",\"*.lock\",\"requirements*.txt\",\"conftest.py\"],\"max_bounces\":2}\n```\n";

    #[test]
    fn the_policy_parses_and_matches_paths() {
        let p = Policy::parse(DOC).unwrap();
        assert!(p.is_approver("Priya"));
        assert!(!p.is_approver("sam"));
        for hit in ["tests/test_reports.py", ".github/workflows/ci.yml", "uv.lock", "requirements-dev.txt", "pkg/conftest.py"] {
            assert!(p.is_protected(hit), "{hit}");
        }
        for miss in ["src/invoices/tax.py", "testsuite.py", "requirements.md", "src/tests.py"] {
            assert!(!p.is_protected(miss), "{miss}");
        }
    }

    #[test]
    fn a_broken_policy_is_a_configuration_error() {
        assert_eq!(Policy::parse("no block").unwrap_err().code, Code::ConfigurationInvalid);
        assert_eq!(Policy::parse(&DOC.replace("\"max_bounces\":2", "\"max_bounces\":0")).unwrap_err().code, Code::ConfigurationInvalid);
    }
}
