//! The Notary's own mirror of the work repository. Every command is argv (never a shell),
//! has a deadline, and runs with prompts disabled.

use std::path::{Path, PathBuf};
use std::time::Duration;

use notary_core::errors::{Code, NotaryError};
use tokio::process::Command;

const DEADLINE: Duration = Duration::from_secs(60);

pub struct Mirror {
    pub url: String,
    pub dir: PathBuf,
}

async fn git(dir: Option<&Path>, args: &[&str]) -> Result<std::process::Output, NotaryError> {
    let mut cmd = Command::new("git");
    if let Some(d) = dir {
        cmd.arg("-C").arg(d);
    }
    cmd.args(args).env("GIT_TERMINAL_PROMPT", "0").kill_on_drop(true);
    match tokio::time::timeout(DEADLINE, cmd.output()).await {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(e)) => Err(NotaryError::new(Code::UpstreamUnavailable, format!("git {}: {e}", args.first().unwrap_or(&"")))),
        Err(_) => Err(NotaryError::new(Code::UpstreamUnavailable, format!("git {} took over {}s", args.first().unwrap_or(&""), DEADLINE.as_secs()))),
    }
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or_default().to_string()
}

impl Mirror {
    /// Clone the mirror if it isn't there yet, then fetch every branch.
    ///
    /// # Errors
    /// `upstream_unavailable` if the remote can't be reached.
    pub async fn fetch(&self) -> Result<(), NotaryError> {
        if !self.dir.join("HEAD").exists() {
            if let Some(parent) = self.dir.parent() {
                std::fs::create_dir_all(parent).map_err(|e| NotaryError::new(Code::UpstreamUnavailable, e.to_string()))?;
            }
            let dir = self.dir.to_string_lossy();
            let out = git(None, &["clone", "--quiet", "--mirror", "--", &self.url, &dir]).await?;
            if !out.status.success() {
                return Err(NotaryError::new(Code::UpstreamUnavailable, format!("git clone: {}", stderr(&out))));
            }
            return Ok(());
        }
        let out = git(Some(&self.dir), &["fetch", "--quiet", "--prune", "origin", "+refs/heads/*:refs/heads/*"]).await?;
        if out.status.success() { Ok(()) } else { Err(NotaryError::new(Code::UpstreamUnavailable, format!("git fetch: {}", stderr(&out)))) }
    }

    /// The commit a branch points at.
    ///
    /// # Errors
    /// If the branch doesn't exist.
    pub async fn branch_head(&self, branch: &str) -> Result<String, NotaryError> {
        let out = git(Some(&self.dir), &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}^{{commit}}")]).await?;
        let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() && notary_core::ids::is_sha(&sha) {
            Ok(sha)
        } else {
            Err(NotaryError::new(Code::UpstreamUnavailable, format!("no branch {branch}")))
        }
    }

    /// Whether `sha` is a commit in the mirror.
    ///
    /// # Errors
    /// If git can't run.
    pub async fn commit_exists(&self, sha: &str) -> Result<bool, NotaryError> {
        Ok(git(Some(&self.dir), &["cat-file", "-e", &format!("{sha}^{{commit}}")]).await?.status.success())
    }

    /// Whether `sha` is on `branch` (the branch contains it). A missing branch is `false`.
    ///
    /// # Errors
    /// If git can't run.
    pub async fn on_branch(&self, sha: &str, branch: &str) -> Result<bool, NotaryError> {
        Ok(git(Some(&self.dir), &["merge-base", "--is-ancestor", sha, &format!("refs/heads/{branch}")]).await?.status.success())
    }

    /// Paths changed between the merge base of `base` and `sha`, and `sha`.
    ///
    /// # Errors
    /// If git can't compute the diff (unknown commit).
    pub async fn diff_names(&self, base: &str, sha: &str) -> Result<Vec<String>, NotaryError> {
        let out = git(Some(&self.dir), &["diff", "--name-only", "-z", "--no-renames", &format!("{base}...{sha}")]).await?;
        if !out.status.success() {
            return Err(NotaryError::new(Code::UpstreamUnavailable, format!("git diff: {}", stderr(&out))));
        }
        Ok(out.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned()).collect())
    }

    /// Write the tree at `sha` into `dest` (git archive, then tar: two commands, no shell).
    ///
    /// # Errors
    /// If either step fails.
    pub async fn export(&self, sha: &str, dest: &Path) -> Result<(), NotaryError> {
        let tar = dest.with_extension("tar");
        let tar_s = tar.to_string_lossy();
        let out = git(Some(&self.dir), &["archive", "--format=tar", "-o", &tar_s, sha]).await?;
        if !out.status.success() {
            return Err(NotaryError::new(Code::SandboxFailed, format!("git archive: {}", stderr(&out))));
        }
        std::fs::create_dir_all(dest).map_err(|e| NotaryError::new(Code::SandboxFailed, e.to_string()))?;
        let status = Command::new("tar")
            .arg("-xf")
            .arg(&tar)
            .arg("-C")
            .arg(dest)
            .status()
            .await
            .map_err(|e| NotaryError::new(Code::SandboxFailed, format!("tar: {e}")))?;
        let _ = std::fs::remove_file(&tar);
        if status.success() { Ok(()) } else { Err(NotaryError::new(Code::SandboxFailed, "tar couldn't unpack the archive")) }
    }
}
