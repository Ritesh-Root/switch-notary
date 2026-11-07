//! The Notary's own test run: the acceptance check at the claimed sha, in a container with no
//! network, a read-only root and the source mounted read-only. One engine, chosen explicitly
//! (podman or docker), no fallback. Containers carry our labels; we only ever kill or remove a
//! container whose labels prove we started it, and failed removals are retried before any new run.

use std::path::{Path, PathBuf};
use std::time::Duration;

use notary_core::errors::{Code, NotaryError};
use notary_core::judge::TestRun;
use notary_core::signing::random_hex;
use serde::Deserialize;
use tokio::process::Command;

use crate::git::Mirror;

const OWNER_LABEL: &str = "io.switch-notary.owner";
const RUN_LABEL: &str = "io.switch-notary.run";
const MAX_RESULT: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Podman,
    Docker,
}

impl Engine {
    const fn bin(self) -> &'static str {
        match self {
            Self::Podman => "podman",
            Self::Docker => "docker",
        }
    }
}

pub struct Sandbox {
    pub engine: Engine,
    /// Image id (sha256 hex) pinned by scripts/sandbox-image.sh.
    pub image: String,
    pub timeout: Duration,
    pub work_dir: PathBuf,
    /// Labels every container we start; set per Notary instance.
    pub owner: String,
    /// Container ids whose removal failed; retried before the next run.
    pending_cleanup: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunResult {
    protocol: String,
    run_id: String,
    sha: String,
    exit_code: i32,
    counts: Counts,
    failed_ids: Vec<String>,
    #[allow(dead_code)]
    failed_truncated: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Counts {
    collected: u64,
    passed: u64,
    failed: u64,
    errors: u64,
    #[allow(dead_code)]
    skipped: u64,
}

/// What the run produced, with the facts its evidence records.
pub struct Outcome {
    pub run: TestRun,
    pub run_id: String,
    pub log_sha256: String,
    pub log_bytes: usize,
}

async fn engine(engine: Engine, args: &[&str]) -> Result<std::process::Output, NotaryError> {
    Command::new(engine.bin())
        .args(args)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| NotaryError::new(Code::SandboxUnavailable, format!("{} {}: {e}", engine.bin(), args.first().unwrap_or(&""))))
}

impl Sandbox {
    #[must_use]
    pub fn new(engine: Engine, image: &str, timeout: Duration, work_dir: PathBuf, owner: &str) -> Self {
        Self { engine, image: image.to_string(), timeout, work_dir, owner: owner.to_string(), pending_cleanup: Vec::new() }
    }

    /// Whether the pinned image is present (no pulling: `--pull never` everywhere).
    ///
    /// # Errors
    /// `sandbox_unavailable` naming what's missing.
    pub async fn check(&self) -> Result<(), NotaryError> {
        let out = engine(self.engine, &["image", "inspect", "--format", "{{.Id}}", &self.image]).await?;
        let id = String::from_utf8_lossy(&out.stdout).trim().trim_start_matches("sha256:").to_string();
        if out.status.success() && id == self.image {
            Ok(())
        } else {
            Err(NotaryError::new(
                Code::SandboxUnavailable,
                format!("image {} isn't loaded in {}; run make sandbox-image", &self.image[..12.min(self.image.len())], self.engine.bin()),
            ))
        }
    }

    /// Remove a container, but only after proving it's ours.
    async fn remove(&self, cid: &str) -> bool {
        let Ok(out) = engine(self.engine, &["inspect", "--format", &format!("{{{{index .Config.Labels \"{OWNER_LABEL}\"}}}}"), cid]).await else {
            return false;
        };
        if !out.status.success() {
            return true; // already gone
        }
        if String::from_utf8_lossy(&out.stdout).trim() != self.owner {
            return true; // not ours: never touch it
        }
        for delay in [0, 250, 1000, 4000] {
            tokio::time::sleep(Duration::from_millis(delay)).await;
            if engine(self.engine, &["rm", "-f", cid]).await.is_ok_and(|o| o.status.success()) {
                return true;
            }
        }
        false
    }

    /// Run `argv` against the tree at `sha`.
    ///
    /// # Errors
    /// `sandbox_unavailable` (no engine or image), `cleanup_incomplete` (an old container is
    /// stuck), or `sandbox_failed` (timeout, bad handshake, unreadable result).
    pub async fn run(&mut self, mirror: &Mirror, sha: &str, argv: &[String]) -> Result<Outcome, NotaryError> {
        let mut still = Vec::new();
        for cid in std::mem::take(&mut self.pending_cleanup) {
            if !self.remove(&cid).await {
                still.push(cid);
            }
        }
        if !still.is_empty() {
            self.pending_cleanup = still;
            return Err(NotaryError::new(Code::CleanupIncomplete, "a previous sandbox container couldn't be removed"));
        }
        self.check().await?;

        let run_id = random_hex(8);
        let dir = self.work_dir.join(&run_id);
        let src = dir.join("src");
        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).map_err(|e| NotaryError::new(Code::SandboxFailed, format!("can't create {}: {e}", out_dir.display())))?;
        set_private(&dir);
        mirror.export(sha, &src).await?;
        let result = self.run_in(&dir, &src, &out_dir, &run_id, sha, argv).await;
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    async fn run_in(&mut self, dir: &Path, src: &Path, out_dir: &Path, run_id: &str, sha: &str, argv: &[String]) -> Result<Outcome, NotaryError> {
        let failed = |why: String| NotaryError::new(Code::SandboxFailed, why);
        let cidfile = dir.join("cid");
        let argv_json = serde_json::to_string(argv).map_err(|e| failed(e.to_string()))?;
        let owner_label = format!("{OWNER_LABEL}={}", self.owner);
        let run_label = format!("{RUN_LABEL}={run_id}");
        let src_mount = format!("{}:/src:ro,Z", src.display());
        let out_mount = format!("{}:/out:rw,Z", out_dir.display());
        let cidfile_s = cidfile.to_string_lossy().to_string();
        let mut create: Vec<String> = [
            "create",
            "--label",
            &owner_label,
            "--label",
            &run_label,
            "--network",
            "none",
            "--read-only",
            "--tmpfs",
            "/tmp:rw,size=256m",
            "--memory",
            "1g",
            "--pids-limit",
            "256",
            "--cpus",
            "2",
            "--cap-drop",
            "all",
            "--security-opt",
            "no-new-privileges",
            "--pull",
            "never",
            "--cidfile",
            &cidfile_s,
            "-v",
            &src_mount,
            "-v",
            &out_mount,
            "-e",
            &format!("NOTARY_RUN_ID={run_id}"),
            "-e",
            &format!("NOTARY_SHA={sha}"),
            "-e",
            &format!("NOTARY_ARGV={argv_json}"),
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        if self.engine == Engine::Docker {
            // Rootful docker: run as the invoking user so the container can write /out but not as root.
            create.push("--user".into());
            create.push(format!("{}:{}", current_id("-u"), current_id("-g")));
        }
        create.push(self.image.clone());
        let arg_refs: Vec<&str> = create.iter().map(String::as_str).collect();
        let created = engine(self.engine, &arg_refs).await?;
        if !created.status.success() {
            return Err(failed(format!("create: {}", String::from_utf8_lossy(&created.stderr).lines().last().unwrap_or_default())));
        }
        let cid = std::fs::read_to_string(&cidfile).map_err(|e| failed(format!("no cidfile: {e}")))?.trim().to_string();

        let outcome = self.start_and_read(&cid, out_dir, run_id, sha).await;
        if !self.remove(&cid).await {
            self.pending_cleanup.push(cid);
        }
        outcome
    }

    async fn start_and_read(&self, cid: &str, out_dir: &Path, run_id: &str, sha: &str) -> Result<Outcome, NotaryError> {
        let failed = |why: String| NotaryError::new(Code::SandboxFailed, why);
        engine(self.engine, &["start", cid]).await?;
        let waited = tokio::time::timeout(self.timeout, engine(self.engine, &["wait", cid])).await;
        if waited.is_err() {
            let _ = engine(self.engine, &["kill", cid]).await;
            return Err(failed(format!("the acceptance check ran over NOTARY_TEST_TIMEOUT_S={}", self.timeout.as_secs())));
        }
        let inspect = engine(self.engine, &["inspect", "--format", "{{.State.ExitCode}} {{.Image}}", cid]).await?;
        let inspected = String::from_utf8_lossy(&inspect.stdout).trim().to_string();
        let (exit, image) = inspected.split_once(' ').unwrap_or_default();
        if image.trim_start_matches("sha256:") != self.image {
            return Err(failed(format!("container ran image {image}, not the pinned one")));
        }
        if exit != "0" {
            return Err(failed(format!("the wrapper itself exited {exit}")));
        }
        let logs = engine(self.engine, &["logs", cid]).await.map(|o| [o.stdout, o.stderr].concat()).unwrap_or_default();
        let result = read_result(&out_dir.join("result.json"))?;
        if result.protocol != "notary-run/1" || result.run_id != run_id || result.sha != sha {
            return Err(failed("result handshake mismatch: the result isn't from this run".into()));
        }
        let c = result.counts;
        if (result.exit_code == 0) != (c.failed + c.errors == 0) && result.exit_code != 5 {
            return Err(failed(format!("inconsistent result: exit {} with {} failed, {} errors", result.exit_code, c.failed, c.errors)));
        }
        Ok(Outcome {
            run: TestRun {
                exit_code: result.exit_code,
                collected: c.collected,
                passed: c.passed,
                failed: c.failed,
                errors: c.errors,
                failed_ids: result.failed_ids,
            },
            run_id: run_id.to_string(),
            log_sha256: notary_core::ids::sha_hex(&logs),
            log_bytes: logs.len(),
        })
    }
}

/// Read result.json only if it's a small regular file (a test could leave a symlink there).
fn read_result(path: &Path) -> Result<RunResult, NotaryError> {
    let failed = |why: String| NotaryError::new(Code::SandboxFailed, why);
    let meta = std::fs::symlink_metadata(path).map_err(|_| failed("the run wrote no result.json".into()))?;
    if !meta.file_type().is_file() || meta.len() > MAX_RESULT {
        return Err(failed("result.json isn't a regular file under 64 KiB".into()));
    }
    let bytes = std::fs::read(path).map_err(|e| failed(e.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|e| failed(format!("result.json: {e}")))
}

fn set_private(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
}

fn current_id(flag: &str) -> String {
    std::process::Command::new("id").arg(flag).output().map_or_else(|_| "65534".into(), |o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use notary_core::errors::{Code, NotaryError};

    use super::*;

    /// A `result.json` the sandbox wrapper would write. `exit_code` is raw JSON so a test can pass a non-integer.
    fn result_json(exit_code: &str) -> String {
        format!(
            r#"{{"protocol":"notary-run/1","run_id":"run1","sha":"abc","exit_code":{exit_code},"counts":{{"collected":2,"passed":1,"failed":1,"errors":0,"skipped":0}},"failed_ids":["test_pay"],"failed_truncated":false}}"#
        )
    }

    struct Tmp(PathBuf);

    impl Tmp {
        fn create() -> Self {
            let dir = std::env::temp_dir().join(format!("notary-result-{}", notary_core::signing::random_hex(6)));
            std::fs::create_dir_all(&dir).expect("tmp dir");
            Self(dir)
        }

        fn write(&self, bytes: &[u8]) -> PathBuf {
            let path = self.0.join("result.json");
            std::fs::write(&path, bytes).expect("write result.json");
            path
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn must_fail(path: &Path) -> NotaryError {
        let Err(err) = read_result(path) else {
            panic!("expected sandbox_failed");
        };
        err
    }

    #[test]
    fn read_result_parses_a_valid_result() {
        let tmp = Tmp::create();
        let path = tmp.write(result_json("0").as_bytes());
        let got = read_result(&path).expect("valid result");
        assert_eq!(got.protocol, "notary-run/1");
        assert_eq!(got.run_id, "run1");
        assert_eq!(got.sha, "abc");
        assert_eq!(got.exit_code, 0);
        assert_eq!((got.counts.collected, got.counts.passed, got.counts.failed, got.counts.errors), (2, 1, 1, 0));
        assert_eq!(got.failed_ids, ["test_pay"]);
    }

    #[test]
    fn read_result_rejects_output_over_64_kib() {
        let tmp = Tmp::create();
        let path = tmp.write(&vec![b'x'; usize::try_from(MAX_RESULT).expect("64 KiB fits") + 1]);
        let err = must_fail(&path);
        assert_eq!(err, NotaryError::new(Code::SandboxFailed, "result.json isn't a regular file under 64 KiB"));
    }

    #[test]
    fn read_result_rejects_a_malformed_exit_code() {
        let tmp = Tmp::create();
        let path = tmp.write(result_json("\"nope\"").as_bytes());
        let err = must_fail(&path);
        assert_eq!(err.code, Code::SandboxFailed);
        assert!(err.detail.starts_with("result.json: invalid type:") && err.detail.contains("expected i32"), "{err}");
    }
}
