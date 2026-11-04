//! One closed set of codes, used inside the Notary and in what it tells agents and people.
//! Each code says whether a resubmission can succeed, who has to act, and what to do, so a
//! builder agent can recover mechanically instead of parsing prose.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    // The builder can fix these; they bounce the report.
    BlockMissing,
    BlockInvalid,
    CommitMissing,
    NotOnBranch,
    FilesDiffer,
    UndisclosedChange,
    TestsFailed,
    CommandFailed,
    CitationUnknown,
    CitationIrrelevant,
    MaskedCommand,
    NoTestsCollected,
    // Not provable either way: shown to people, never counted as backed.
    NoActivityRow,
    InputsChanged,
    // A person decides.
    ProtectedPath,
    RevisionLimit,
    NotAnApprover,
    // Faults in the Notary's own sources.
    SandboxUnavailable,
    SandboxFailed,
    CleanupIncomplete,
    UpstreamUnavailable,
    IntegrityMismatch,
    ConfigurationInvalid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Fixer {
    Builder,
    Human,
    Operator,
    Notary,
}

#[derive(Clone, Copy, Debug)]
pub struct Meta {
    pub retryable: bool,
    pub fixer: Fixer,
    pub remediation: &'static str,
}

impl Code {
    #[must_use]
    pub const fn meta(self) -> Meta {
        use Fixer::{Builder, Human, Notary, Operator};
        let (retryable, fixer, remediation) = match self {
            Self::BlockMissing => (true, Builder, "End the report with exactly one notary-claims block (room document \"Reporting Contract\")."),
            Self::BlockInvalid => (true, Builder, "Strict JSON, known fields only, 40-hex sha, unique claim ids c1..c999, at most 40 claims, block at most 16 KiB."),
            Self::CommitMissing => (true, Builder, "Push the commit: this sha is not in the repository after fetch."),
            Self::NotOnBranch => (true, Builder, "Push the sha to the branch you claimed."),
            Self::FilesDiffer => (true, Builder, "List exactly the paths in git diff base...sha."),
            Self::UndisclosedChange => (true, Builder, "Add these paths to files_changed, or revert them."),
            Self::TestsFailed => (true, Builder, "Fix the listed tests; the Notary's own run at the sha decides."),
            Self::CommandFailed => (true, Builder, "Switch recorded this command as failed."),
            Self::CitationUnknown => (true, Builder, "Cite only ev_ ids the Notary published for this work order."),
            Self::CitationIrrelevant => (true, Builder, "Cite evidence about this claim's kind and sha."),
            Self::MaskedCommand => (true, Builder, "Run checks as one plain command (no | ; & > < $( or backticks), under 160 characters."),
            Self::NoTestsCollected => (true, Builder, "0 tests were collected; an empty run is not a pass."),
            Self::NoActivityRow => (false, Human, "Switch has no record of this command. That is not proof it didn't run (rows are buffered, or it ran inside another shell)."),
            Self::InputsChanged => (false, Human, "Test files or the lockfile changed at this sha, so a passing run can't back the claim."),
            Self::ProtectedPath => (true, Human, "An approver must accept this change to a protected path, or the builder reverts it."),
            Self::RevisionLimit => (false, Human, "The bounce limit is used up; a person decides."),
            Self::NotAnApprover => (true, Human, "Only the approvers in the Desk Policy can approve."),
            Self::SandboxUnavailable => (false, Operator, "Load the pinned sandbox image (make sandbox-image). There is no fallback engine."),
            Self::SandboxFailed => (true, Notary, "The sandboxed run timed out, failed its handshake or overflowed; the detail names the setting."),
            Self::CleanupIncomplete => (true, Notary, "A sandbox container couldn't be removed; new runs wait until removal succeeds."),
            Self::UpstreamUnavailable => (true, Notary, "git or Switch didn't answer; a 403 on activity means the gateway user isn't a member of the work room."),
            Self::IntegrityMismatch => (false, Human, "A ledger record changed after it was written. Stop and investigate."),
            Self::ConfigurationInvalid => (false, Operator, "Fix the named setting; there are no silent defaults."),
        };
        Meta { retryable, fixer, remediation }
    }

    /// Whether the builder can fix this by reporting again.
    #[must_use]
    pub const fn builder_fixes(self) -> bool {
        matches!(self.meta().fixer, Fixer::Builder)
    }
}

impl std::fmt::Display for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = serde_json::to_value(self).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
        f.write_str(&s)
    }
}

/// An error with its code. `detail` names the setting or fact involved, never a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotaryError {
    pub code: Code,
    pub detail: String,
}

impl NotaryError {
    pub fn new(code: Code, detail: impl Into<String>) -> Self {
        Self { code, detail: detail.into() }
    }
}

impl std::fmt::Display for NotaryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for NotaryError {}

impl From<rusqlite::Error> for NotaryError {
    fn from(e: rusqlite::Error) -> Self {
        Self::new(Code::UpstreamUnavailable, format!("sqlite: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_serialize_as_snake_case() {
        assert_eq!(Code::TestsFailed.to_string(), "tests_failed");
        assert_eq!(serde_json::from_str::<Code>("\"masked_command\"").unwrap(), Code::MaskedCommand);
    }

    #[test]
    fn only_builder_codes_bounce() {
        assert!(Code::TestsFailed.builder_fixes());
        assert!(!Code::NoActivityRow.builder_fixes());
        assert!(!Code::ProtectedPath.builder_fixes());
        assert!(!Code::SandboxUnavailable.builder_fixes());
    }
}
