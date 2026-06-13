use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::{DateTime, SecondsFormat, Utc};
use thiserror::Error;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("goose binary unavailable: {0}")]
    GooseBinaryUnavailable(String),
    #[error("spawn failed: {0}")]
    SpawnFailed(String),
    #[error("wait error: {0}")]
    Wait(std::io::Error),
    #[error("signal error: {0}")]
    Signal(nix::errno::Errno),
    #[error("profile already bound")]
    ProfileInUse,
}

impl From<nix::errno::Errno> for ProcessError {
    fn from(e: nix::errno::Errno) -> Self {
        ProcessError::Signal(e)
    }
}

pub struct GoosedChild {
    pub child: Child,
    pub pid: u32,
    pub started_at: DateTime<Utc>,
    pub acp_bound: bool,
}

pub struct RunningProcess {
    pub name: String,
    pub pid: u32,
    pub started_at: String,
}

pub struct ProcessMap {
    children: HashMap<String, GoosedChild>,
    binary: Option<PathBuf>,
}

pub struct AcpHandles {
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
    pub pid: u32,
}

impl ProcessMap {
    pub fn new(binary: Option<PathBuf>) -> Self {
        Self {
            children: HashMap::new(),
            binary,
        }
    }

    pub async fn start(
        &mut self,
        name: &str,
        profile_path: &Path,
        cwd: &Path,
    ) -> Result<u32, ProcessError> {
        let binary = self.binary.as_ref().ok_or_else(|| {
            ProcessError::GooseBinaryUnavailable(
                "set GEESE_GOOSE_BIN or install goose on PATH".to_string(),
            )
        })?;

        // Idempotent: return existing pid if already running, but only if
        // it was started via the same path. If an ACP connection currently
        // owns this profile, refuse — single-owner contract per #22.
        if let Some(child) = self.children.get(name) {
            if child.acp_bound {
                return Err(ProcessError::ProfileInUse);
            }
            return Ok(child.pid);
        }

        use std::process::Stdio;
        let mut cmd = Command::new(binary);
        cmd.arg("acp")
            .env("GOOSE_PATH_ROOT", profile_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);

        // Apply cwd only if it exists on disk; otherwise inherit geesed's cwd
        // with a warning. This prevents `Command::spawn` from failing with an
        // ambiguous ENOENT (which would otherwise be indistinguishable from a
        // missing binary) when the configured cwd is stale or never existed.
        if cwd.is_dir() {
            cmd.current_dir(cwd);
        } else {
            tracing::warn!(
                cwd = %cwd.display(),
                profile = name,
                "configured cwd does not exist; falling back to geesed's working directory"
            );
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| ProcessError::SpawnFailed(e.to_string()))?;

        let pid = child
            .id()
            .ok_or_else(|| ProcessError::SpawnFailed("process has no pid".to_string()))?;

        // 100ms startup sanity check: if the binary exits immediately it's broken
        tokio::time::sleep(Duration::from_millis(100)).await;

        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(ProcessError::SpawnFailed(format!(
                    "process exited immediately with status: {status}"
                )));
            }
            Ok(None) => {} // still running
            Err(e) => return Err(ProcessError::SpawnFailed(e.to_string())),
        }

        let started_at = Utc::now();
        self.children.insert(
            name.to_string(),
            GoosedChild {
                child,
                pid,
                started_at,
                acp_bound: false,
            },
        );

        Ok(pid)
    }

    /// Bind an ACP connection to this profile. Spawns goose if not running.
    /// Returns the stdio handles to use for the byte pump.
    /// Errors if already bound (single-client enforcement).
    pub async fn bind_acp(
        &mut self,
        name: &str,
        profile_path: &Path,
        cwd: &Path,
    ) -> Result<AcpHandles, ProcessError> {
        // Single-owner contract (#22): if any entry exists for this
        // profile, refuse — regardless of whether it was started via
        // goosed.start (acp_bound=false) or a previous connect_profile
        // (acp_bound=true). Both flavours of "in use" map to the same
        // error to the client.
        if self.children.contains_key(name) {
            return Err(ProcessError::ProfileInUse);
        }

        // Not running — spawn fresh
        let pid = self.start(name, profile_path, cwd).await?;

        // Take stdio handles
        let child = self
            .children
            .get_mut(name)
            .expect("just inserted or exists");

        let stdin = child
            .child
            .stdin
            .take()
            .expect("stdin should be available for ACP binding");
        let stdout = child
            .child
            .stdout
            .take()
            .expect("stdout should be available for ACP binding");

        child.acp_bound = true;

        Ok(AcpHandles { stdin, stdout, pid })
    }

    /// Unbind an ACP connection and stop the goose process.
    pub async fn unbind_acp(&mut self, name: &str) -> Result<(), ProcessError> {
        self.stop(name).await
    }

    pub async fn stop(&mut self, name: &str) -> Result<(), ProcessError> {
        let Some(mut entry) = self.children.remove(name) else {
            return Ok(()); // idempotent: not running is fine
        };

        use nix::errno::Errno;
        match nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(entry.pid as i32),
            nix::sys::signal::Signal::SIGTERM,
        ) {
            Ok(()) => {}
            Err(Errno::ESRCH) => return Ok(()), // already dead
            Err(e) => return Err(ProcessError::Signal(e)),
        }

        match tokio::time::timeout(Duration::from_secs(5), entry.child.wait()).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(ProcessError::Wait(e)),
            Err(_timeout) => {
                // Timeout: escalate to SIGKILL
                entry.child.start_kill().ok();
                let _ = entry.child.wait().await;
                Ok(())
            }
        }
    }

    pub async fn kill(&mut self, name: &str) -> Result<(), ProcessError> {
        let Some(mut entry) = self.children.remove(name) else {
            return Ok(()); // idempotent
        };

        entry.child.start_kill().ok();
        let _ = entry.child.wait().await;
        Ok(())
    }

    pub fn list(&self) -> Vec<RunningProcess> {
        let mut result: Vec<RunningProcess> = self
            .children
            .iter()
            .map(|(name, child)| RunningProcess {
                name: name.clone(),
                pid: child.pid,
                started_at: child.started_at.to_rfc3339_opts(SecondsFormat::Secs, true),
            })
            .collect();
        result.sort_by(|a, b| a.name.cmp(&b.name));
        result
    }
}
