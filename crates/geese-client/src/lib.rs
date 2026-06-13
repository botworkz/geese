use std::{
    env,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use nix::unistd::Uid;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

#[derive(Debug, Clone)]
pub struct GeesedClient {
    socket_path: PathBuf,
}

impl GeesedClient {
    pub async fn connect() -> Result<Self, ClientError> {
        let socket_path = default_socket_path();
        Self::connect_at(socket_path).await
    }

    pub async fn connect_at(socket: impl AsRef<Path>) -> Result<Self, ClientError> {
        let socket_path = socket.as_ref().to_path_buf();
        connect_socket(&socket_path).await?;
        Ok(Self { socket_path })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub async fn status(&mut self) -> Result<StatusResponse, ClientError> {
        let mut stream = connect_socket(&self.socket_path).await?;
        stream
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"status\"}\n")
            .await?;

        let mut line = String::new();
        let mut reader = BufReader::new(stream);
        let bytes = reader.read_line(&mut line).await?;
        if bytes == 0 {
            return Err(ClientError::Protocol(
                "connection closed before response".into(),
            ));
        }

        let response: RpcResponse<Value> = serde_json::from_str(line.trim_end())
            .map_err(|error| ClientError::Protocol(error.to_string()))?;

        if let Some(error) = response.error {
            return Err(ClientError::Rpc {
                code: error.code,
                message: error.message,
            });
        }

        let result = response
            .result
            .ok_or_else(|| ClientError::Protocol("missing result field".into()))?;
        serde_json::from_value(result).map_err(|error| ClientError::Protocol(error.to_string()))
    }

    pub async fn list_profiles(&mut self) -> Result<Vec<ProfileEntry>, ClientError> {
        self.call("profile.list", serde_json::json!({})).await
    }

    pub async fn get_profile(&mut self, name: &str) -> Result<ProfileEntry, ClientError> {
        self.call("profile.get", serde_json::json!({"name": name}))
            .await
    }

    pub async fn create_profile(&mut self, name: &str) -> Result<ProfileEntry, ClientError> {
        self.call("profile.create", serde_json::json!({"name": name}))
            .await
    }

    pub async fn delete_profile(&mut self, name: &str) -> Result<(), ClientError> {
        self.call_void("profile.delete", serde_json::json!({"name": name}))
            .await
    }

    pub async fn lock_profile(&mut self, name: &str) -> Result<ProfileEntry, ClientError> {
        self.call("profile.lock", serde_json::json!({"name": name}))
            .await
    }

    pub async fn unlock_profile(&mut self, name: &str) -> Result<ProfileEntry, ClientError> {
        self.call("profile.unlock", serde_json::json!({"name": name}))
            .await
    }

    pub async fn copy_profile(
        &mut self,
        src: &str,
        dest: &str,
    ) -> Result<ProfileEntry, ClientError> {
        self.call(
            "profile.copy",
            serde_json::json!({"src": src, "dest": dest}),
        )
        .await
    }

    pub async fn start_goosed(&mut self, name: &str) -> Result<StartGoosedResponse, ClientError> {
        self.call("goosed.start", serde_json::json!({"name": name}))
            .await
    }

    pub async fn stop_goosed(&mut self, name: &str) -> Result<(), ClientError> {
        self.call_void("goosed.stop", serde_json::json!({"name": name}))
            .await
    }

    pub async fn kill_goosed(&mut self, name: &str) -> Result<(), ClientError> {
        self.call_void("goosed.kill", serde_json::json!({"name": name}))
            .await
    }

    pub async fn list_running_goosed(&mut self) -> Result<Vec<RunningGoosed>, ClientError> {
        self.call("goosed.list_running", serde_json::json!({}))
            .await
    }

    pub async fn set_profile_cwd(
        &mut self,
        name: &str,
        cwd: &str,
    ) -> Result<ProfileEntry, ClientError> {
        self.call(
            "profile.set_cwd",
            serde_json::json!({"name": name, "cwd": cwd}),
        )
        .await
    }

    pub async fn unset_profile_cwd(&mut self, name: &str) -> Result<ProfileEntry, ClientError> {
        self.call("profile.unset_cwd", serde_json::json!({"name": name}))
            .await
    }

    pub async fn get_global_config(&mut self) -> Result<GlobalConfigResponse, ClientError> {
        self.call("config.get_global", serde_json::json!({})).await
    }

    pub async fn set_global_config(
        &mut self,
        cwd: Option<&str>,
    ) -> Result<GlobalConfigResponse, ClientError> {
        let cwd_value = match cwd {
            Some(s) => serde_json::Value::String(s.to_owned()),
            None => serde_json::Value::Null,
        };
        self.call("config.set_global", serde_json::json!({"cwd": cwd_value}))
            .await
    }

    /// Convenience: return the resolved cwd for a profile (calls `profile.get`).
    pub async fn resolve_cwd(&mut self, name: &str) -> Result<std::path::PathBuf, ClientError> {
        let entry = self.get_profile(name).await?;
        let resolved = entry
            .resolved_cwd
            .ok_or_else(|| ClientError::Protocol("missing resolved_cwd field".into()))?;
        Ok(std::path::PathBuf::from(resolved))
    }

    async fn rpc_call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<RpcResponse<Value>, ClientError> {
        let mut stream = connect_socket(&self.socket_path).await?;
        let request = serde_json::json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
        let mut line = request.to_string();
        line.push('\n');
        stream.write_all(line.as_bytes()).await?;

        let mut response_line = String::new();
        let mut reader = BufReader::new(stream);
        let bytes = reader.read_line(&mut response_line).await?;
        if bytes == 0 {
            return Err(ClientError::Protocol(
                "connection closed before response".into(),
            ));
        }

        serde_json::from_str(response_line.trim_end())
            .map_err(|e| ClientError::Protocol(e.to_string()))
    }

    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<T, ClientError> {
        let response = self.rpc_call(method, params).await?;

        if let Some(error) = response.error {
            return Err(ClientError::Rpc {
                code: error.code,
                message: error.message,
            });
        }

        let result = response
            .result
            .ok_or_else(|| ClientError::Protocol("missing result field".into()))?;

        serde_json::from_value(result).map_err(|e| ClientError::Protocol(e.to_string()))
    }

    async fn call_void(&self, method: &str, params: serde_json::Value) -> Result<(), ClientError> {
        let response = self.rpc_call(method, params).await?;

        if let Some(error) = response.error {
            return Err(ClientError::Rpc {
                code: error.code,
                message: error.message,
            });
        }

        Ok(())
    }
}

/// Connect to geesed, autospawning the daemon if it isn't running.
///
/// Behaviour:
/// 1. Attempt `GeesedClient::connect()`. Return immediately on success.
/// 2. On `ClientError::NotRunning(_)`, locate the `geesed` binary
///    (next to the current exe, then `$PATH`), spawn it detached,
///    poll the socket every 50ms up to 5s.
/// 3. Retry `GeesedClient::connect()` once after the socket appears.
///
/// The lockfile in geesed handles the "two callers racing to autospawn"
/// case — the second spawn loses the lock and exits, the second
/// caller's connect attempt finds the first one's socket.
pub async fn ensure_running() -> Result<GeesedClient, ClientError> {
    match GeesedClient::connect().await {
        Ok(client) => return Ok(client),
        Err(ClientError::NotRunning(_)) => {}
        Err(e) => return Err(e),
    }

    let geesed_bin =
        find_geesed_binary().ok_or_else(|| ClientError::NotRunning(default_socket_path()))?;

    // Capture geesed's stdout/stderr to a log file in the runtime dir so
    // autospawn failures (binary missing dep, lockfile race we lost, panic
    // on startup, ...) leave a forensic trail instead of being silently
    // swallowed. Without this every `geese new foo` that hits a daemon
    // problem dead-ends at "geesed: not running" with no actionable info.
    let dir = runtime_dir();
    std::fs::create_dir_all(&dir).ok();
    let log_path = dir.join("geesed.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(ClientError::Io)?;
    let log_clone = log.try_clone().map_err(ClientError::Io)?;

    let mut cmd = std::process::Command::new(&geesed_bin);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(log_clone));

    // Detach from the cli's controlling terminal / session so the daemon
    // survives the cli exiting. Without setsid a SIGHUP on terminal close
    // would kill geesed along with the shell that spawned it.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe; pre_exec only invokes it.
        unsafe {
            cmd.pre_exec(|| {
                nix::unistd::setsid()
                    .map(|_| ())
                    .map_err(std::io::Error::from)
            });
        }
    }

    cmd.spawn().map_err(ClientError::Io)?;

    let socket_path = default_socket_path();
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }

    GeesedClient::connect().await
}

pub(crate) fn find_geesed_binary() -> Option<PathBuf> {
    // 1. Next to the current executable
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        let candidate = parent.join("geesed");
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    // 2. Search $PATH
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("geesed");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

#[derive(Debug, Deserialize)]
pub struct StatusResponse {
    pub pid: u32,
    pub version: String,
    pub uptime_ms: u64,
    pub started_at: String,
}

/// Wire-shape of a profile entry returned by geesed CRUD methods.
#[derive(Debug, Clone, Deserialize)]
pub struct ProfileEntry {
    pub name: String,
    pub locked: bool,
    pub parent: Option<String>,
    pub path: String,
    /// Raw per-profile cwd setting (None if not set).
    #[serde(default)]
    pub cwd: Option<String>,
    /// Resolved cwd after walking the 5-tier priority chain.
    /// Only present on responses to `profile.get`, `profile.set_cwd`,
    /// `profile.unset_cwd`.
    #[serde(default)]
    pub resolved_cwd: Option<String>,
}

/// Global geese configuration returned by `config.get_global`.
#[derive(Debug, Clone, Deserialize)]
pub struct GlobalConfigResponse {
    pub cwd: Option<String>,
}

/// Response from `goosed.start`.
#[derive(Debug, Deserialize)]
pub struct StartGoosedResponse {
    pub pid: u32,
}

/// Entry in the list returned by `goosed.list_running`.
#[derive(Debug, Deserialize)]
pub struct RunningGoosed {
    pub name: String,
    pub pid: u32,
    pub started_at: String,
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("geesed not running (no socket at {0})")]
    NotRunning(PathBuf),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("rpc error {code}: {message}")]
    Rpc { code: i64, message: String },
}

#[derive(Debug, Deserialize)]
struct RpcResponse<T> {
    #[allow(dead_code)]
    jsonrpc: String,
    #[allow(dead_code)]
    id: Value,
    #[serde(default)]
    result: Option<T>,
    #[serde(default)]
    error: Option<RpcError>,
}

#[derive(Debug, Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

async fn connect_socket(socket_path: &Path) -> Result<UnixStream, ClientError> {
    UnixStream::connect(socket_path)
        .await
        .map_err(|error| map_connect_error(error, socket_path.to_path_buf()))
}

fn map_connect_error(error: std::io::Error, socket_path: PathBuf) -> ClientError {
    if matches!(
        error.kind(),
        ErrorKind::NotFound | ErrorKind::ConnectionRefused
    ) {
        ClientError::NotRunning(socket_path)
    } else {
        ClientError::Io(error)
    }
}

fn default_socket_path() -> PathBuf {
    runtime_dir().join("control.sock")
}

fn runtime_dir() -> PathBuf {
    match env::var_os("XDG_RUNTIME_DIR") {
        Some(runtime_dir) => PathBuf::from(runtime_dir).join("geese"),
        None => env::temp_dir().join(format!("geese-{}", Uid::current().as_raw())),
    }
}
