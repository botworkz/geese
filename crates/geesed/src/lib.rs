use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use chrono::{SecondsFormat, Utc};
use fs2::FileExt;
use nix::unistd::Uid;
use serde::Serialize;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{Mutex as TokioMutex, watch},
    task::JoinSet,
};
use tracing::info;

pub mod processes;
use processes::{ProcessError, ProcessMap};

#[derive(Debug, Default)]
pub struct RunOpts {
    pub runtime_dir: Option<PathBuf>,
    pub geese_root: Option<PathBuf>,
    pub goose_bin: Option<PathBuf>,
    pub shutdown: Option<watch::Receiver<bool>>,
}

impl RunOpts {
    pub fn with_runtime_dir(mut self, runtime_dir: impl Into<PathBuf>) -> Self {
        self.runtime_dir = Some(runtime_dir.into());
        self
    }

    pub fn with_geese_root(mut self, geese_root: impl Into<PathBuf>) -> Self {
        self.geese_root = Some(geese_root.into());
        self
    }

    pub fn with_goose_bin(mut self, goose_bin: impl Into<PathBuf>) -> Self {
        self.goose_bin = Some(goose_bin.into());
        self
    }

    pub fn with_shutdown(mut self, shutdown: watch::Receiver<bool>) -> Self {
        self.shutdown = Some(shutdown);
        self
    }
}

#[derive(Debug, Error)]
pub enum RunError {
    #[error("geesed: already running, pid={pid}")]
    AlreadyRunning { pid: String },
    #[error("geesed: storage: {0}")]
    Storage(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub async fn run(opts: RunOpts) -> Result<(), RunError> {
    let runtime_dir = RuntimeDir::new(opts.runtime_dir.unwrap_or_else(default_runtime_dir));
    runtime_dir.ensure()?;

    let lockfile = Lockfile::acquire(runtime_dir.lockfile_path())?;
    let control_socket = ControlSocket::bind(runtime_dir.socket_path())?;
    let acp_socket = AcpSocket::bind(runtime_dir.acp_socket_path())?;
    let state = Arc::new(DaemonState::new(lockfile.pid()));

    let storage = {
        let s = match opts.geese_root {
            Some(root) => geese::Storage::at(root),
            None => geese::Storage::from_env().map_err(|e| RunError::Storage(e.to_string()))?,
        };
        Arc::new(Mutex::new(s))
    };

    let goose_bin = resolve_goose_bin(opts.goose_bin.as_deref());
    let processes = Arc::new(TokioMutex::new(ProcessMap::new(goose_bin)));

    info!(
        "geesed v{} listening on {} (control) and {} (acp) pid={}",
        env!("CARGO_PKG_VERSION"),
        control_socket.path().display(),
        acp_socket.path().display(),
        state.pid,
    );

    let mut shutdown = opts.shutdown;
    let mut tasks = JoinSet::new();
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;

    loop {
        tokio::select! {
            accept_result = control_socket.accept() => {
                let (stream, _) = accept_result?;
                let state = Arc::clone(&state);
                let storage = Arc::clone(&storage);
                let processes = Arc::clone(&processes);
                tasks.spawn(async move {
                    if let Err(error) = handle_connection(stream, state, storage, processes).await {
                        tracing::debug!("control connection ended with error: {error}");
                    }
                });
            }
            accept_result = acp_socket.accept() => {
                let (stream, _) = accept_result?;
                let storage = Arc::clone(&storage);
                let processes = Arc::clone(&processes);
                tasks.spawn(async move {
                    if let Err(error) = handle_acp_connection(stream, storage, processes).await {
                        tracing::debug!("acp connection ended with error: {error}");
                    }
                });
            }
            _ = sigterm.recv() => break,
            _ = sigint.recv() => break,
            _ = wait_for_shutdown(&mut shutdown) => break,
        }
    }

    drop(acp_socket);
    drop(control_socket);
    drop(lockfile);

    let wait_for_tasks = async { while tasks.join_next().await.is_some() {} };

    if tokio::time::timeout(Duration::from_secs(5), wait_for_tasks)
        .await
        .is_err()
    {
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }

    Ok(())
}

#[derive(Debug)]
struct RuntimeDir {
    path: PathBuf,
}

impl RuntimeDir {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn ensure(&self) -> Result<(), RunError> {
        fs::create_dir_all(&self.path)?;
        fs::set_permissions(&self.path, fs::Permissions::from_mode(0o700))?;
        Ok(())
    }

    fn socket_path(&self) -> PathBuf {
        self.path.join("control.sock")
    }

    fn acp_socket_path(&self) -> PathBuf {
        self.path.join("acp.sock")
    }

    fn lockfile_path(&self) -> PathBuf {
        self.path.join("geesed.pid")
    }
}

#[derive(Debug)]
struct Lockfile {
    path: PathBuf,
    file: File,
    pid: u32,
}

impl Lockfile {
    fn acquire(path: PathBuf) -> Result<Self, RunError> {
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)?;

        match file.try_lock_exclusive() {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let pid = fs::read_to_string(&path)
                    .ok()
                    .map(|pid| pid.trim().to_owned())
                    .filter(|pid| !pid.is_empty())
                    .unwrap_or_else(|| "unknown".to_string());
                return Err(RunError::AlreadyRunning { pid });
            }
            Err(error) => return Err(RunError::Io(error)),
        }

        let pid = std::process::id();
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        writeln!(file, "{pid}")?;
        file.sync_all()?;

        Ok(Self { path, file, pid })
    }

    fn pid(&self) -> u32 {
        self.pid
    }
}

impl Drop for Lockfile {
    fn drop(&mut self) {
        let _ = self.file.unlock();
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Debug)]
struct ControlSocket {
    path: PathBuf,
    listener: UnixListener,
}

impl ControlSocket {
    fn bind(path: PathBuf) -> Result<Self, RunError> {
        if path.exists() {
            fs::remove_file(&path)?;
        }

        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        Ok(Self { path, listener })
    }

    async fn accept(&self) -> Result<(UnixStream, tokio::net::unix::SocketAddr), RunError> {
        self.listener.accept().await.map_err(RunError::Io)
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ControlSocket {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Debug)]
struct AcpSocket {
    path: PathBuf,
    listener: UnixListener,
}

impl AcpSocket {
    fn bind(path: PathBuf) -> Result<Self, RunError> {
        if path.exists() {
            fs::remove_file(&path)?;
        }

        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        Ok(Self { path, listener })
    }

    async fn accept(&self) -> Result<(UnixStream, tokio::net::unix::SocketAddr), RunError> {
        self.listener.accept().await.map_err(RunError::Io)
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for AcpSocket {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Debug)]
struct DaemonState {
    pid: u32,
    started_at: String,
    started: Instant,
}

impl DaemonState {
    fn new(pid: u32) -> Self {
        Self {
            pid,
            started_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            started: Instant::now(),
        }
    }

    fn status(&self) -> StatusPayload {
        StatusPayload {
            pid: self.pid,
            version: env!("CARGO_PKG_VERSION").to_string(),
            uptime_ms: self
                .started
                .elapsed()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            started_at: self.started_at.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
struct StatusPayload {
    pid: u32,
    version: String,
    uptime_ms: u64,
    started_at: String,
}

async fn handle_connection(
    stream: UnixStream,
    state: Arc<DaemonState>,
    storage: Arc<Mutex<geese::Storage>>,
    processes: Arc<TokioMutex<ProcessMap>>,
) -> std::io::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut buf = Vec::new();

    loop {
        buf.clear();
        let bytes = reader.read_until(b'\n', &mut buf).await?;
        if bytes == 0 {
            return Ok(());
        }

        while matches!(buf.last(), Some(b'\n' | b'\r')) {
            buf.pop();
        }

        let response = match std::str::from_utf8(&buf) {
            Ok(line) => dispatch_rpc(line, &state, &storage, &processes).await,
            Err(_) => rpc_error(Value::Null, -32700, "Parse error"),
        };

        write_half.write_all(response.as_bytes()).await?;
        write_half.write_all(b"\n").await?;
    }
}

async fn dispatch_rpc(
    line: &str,
    state: &DaemonState,
    storage: &Mutex<geese::Storage>,
    processes: &TokioMutex<ProcessMap>,
) -> String {
    let value: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(_) => return rpc_error(Value::Null, -32700, "Parse error"),
    };

    let id = value.get("id").cloned().unwrap_or(Value::Null);
    let params = value.get("params").cloned().unwrap_or(Value::Null);

    match value.get("method").and_then(Value::as_str) {
        Some("status") => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": state.status(),
        })
        .to_string(),

        Some("profile.list") => {
            let guard = storage.lock().expect("storage mutex poisoned");
            match guard.list_full() {
                Ok(entries) => {
                    let entries: Vec<Value> = entries
                        .iter()
                        .map(|(meta, path)| profile_entry_from_parts(meta, path))
                        .collect();
                    json!({"jsonrpc":"2.0","id":id,"result":entries}).to_string()
                }
                Err(e) => rpc_error(id, -32000, &e.to_string()),
            }
        }

        Some("profile.get") => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return rpc_error(id, -32602, "Invalid params");
            };
            let guard = storage.lock().expect("storage mutex poisoned");
            match guard.get(name) {
                Ok(profile) => {
                    let resolved_cwd = guard.resolve_cwd(name);
                    json!({"jsonrpc":"2.0","id":id,"result":profile_entry_with_cwd(&profile, &resolved_cwd)}).to_string()
                }
                Err(e) => rpc_error(id, geese_error_code(&e), &e.to_string()),
            }
        }

        Some("profile.create") => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return rpc_error(id, -32602, "Invalid params");
            };
            let guard = storage.lock().expect("storage mutex poisoned");
            match guard.create(name) {
                Ok(profile) => {
                    json!({"jsonrpc":"2.0","id":id,"result":profile_entry(&profile)}).to_string()
                }
                Err(e) => rpc_error(id, geese_error_code(&e), &e.to_string()),
            }
        }

        Some("profile.delete") => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return rpc_error(id, -32602, "Invalid params");
            };
            let guard = storage.lock().expect("storage mutex poisoned");
            match guard.delete(name) {
                Ok(()) => json!({"jsonrpc":"2.0","id":id,"result":null}).to_string(),
                Err(e) => rpc_error(id, geese_error_code(&e), &e.to_string()),
            }
        }

        Some("profile.lock") => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return rpc_error(id, -32602, "Invalid params");
            };
            let guard = storage.lock().expect("storage mutex poisoned");
            match guard.get(name) {
                Ok(mut profile) => match profile.lock() {
                    Ok(()) => json!({"jsonrpc":"2.0","id":id,"result":profile_entry(&profile)})
                        .to_string(),
                    Err(e) => rpc_error(id, geese_error_code(&e), &e.to_string()),
                },
                Err(e) => rpc_error(id, geese_error_code(&e), &e.to_string()),
            }
        }

        Some("profile.unlock") => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return rpc_error(id, -32602, "Invalid params");
            };
            let guard = storage.lock().expect("storage mutex poisoned");
            match guard.get(name) {
                Ok(mut profile) => match profile.unlock() {
                    Ok(()) => json!({"jsonrpc":"2.0","id":id,"result":profile_entry(&profile)})
                        .to_string(),
                    Err(e) => rpc_error(id, geese_error_code(&e), &e.to_string()),
                },
                Err(e) => rpc_error(id, geese_error_code(&e), &e.to_string()),
            }
        }

        Some("profile.copy") => {
            let (src, dest) = match (
                params.get("src").and_then(Value::as_str),
                params.get("dest").and_then(Value::as_str),
            ) {
                (Some(s), Some(d)) => (s, d),
                _ => return rpc_error(id, -32602, "Invalid params"),
            };
            let guard = storage.lock().expect("storage mutex poisoned");
            match guard.copy(src, dest) {
                Ok(profile) => {
                    json!({"jsonrpc":"2.0","id":id,"result":profile_entry(&profile)}).to_string()
                }
                Err(e) => rpc_error(id, geese_error_code(&e), &e.to_string()),
            }
        }

        Some("goosed.start") => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return rpc_error(id, -32602, "Invalid params");
            };
            // Look up profile path and resolve cwd (sync lock, released before any await)
            let (profile_path, resolved_cwd) = {
                let guard = storage.lock().expect("storage mutex poisoned");
                match guard.get(name) {
                    Ok(profile) => {
                        let cwd = guard.resolve_cwd(name);
                        (profile.path().to_path_buf(), cwd)
                    }
                    Err(e) => return rpc_error(id, geese_error_code(&e), &e.to_string()),
                }
            };
            let mut procs = processes.lock().await;
            match procs.start(name, &profile_path, &resolved_cwd).await {
                Ok(pid) => json!({"jsonrpc":"2.0","id":id,"result":{"pid":pid}}).to_string(),
                Err(ProcessError::GooseBinaryUnavailable(msg)) => rpc_error(id, -32010, &msg),
                Err(ProcessError::SpawnFailed(msg)) => rpc_error(id, -32011, &msg),
                Err(ProcessError::ProfileInUse) => {
                    rpc_error(id, -32020, "profile already owned by an ACP connection")
                }
                Err(e) => rpc_error(id, -32000, &e.to_string()),
            }
        }

        Some("goosed.stop") => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return rpc_error(id, -32602, "Invalid params");
            };
            let mut procs = processes.lock().await;
            match procs.stop(name).await {
                Ok(()) => json!({"jsonrpc":"2.0","id":id,"result":null}).to_string(),
                Err(e) => rpc_error(id, -32000, &e.to_string()),
            }
        }

        Some("goosed.kill") => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return rpc_error(id, -32602, "Invalid params");
            };
            let mut procs = processes.lock().await;
            match procs.kill(name).await {
                Ok(()) => json!({"jsonrpc":"2.0","id":id,"result":null}).to_string(),
                Err(e) => rpc_error(id, -32000, &e.to_string()),
            }
        }

        Some("goosed.list_running") => {
            let procs = processes.lock().await;
            let running = procs.list();
            let entries: Vec<Value> = running
                .iter()
                .map(|p| {
                    json!({
                        "name": p.name,
                        "pid": p.pid,
                        "started_at": p.started_at,
                    })
                })
                .collect();
            json!({"jsonrpc":"2.0","id":id,"result":entries}).to_string()
        }

        Some("profile.set_cwd") => {
            let (Some(name), Some(cwd_str)) = (
                params.get("name").and_then(Value::as_str),
                params.get("cwd").and_then(Value::as_str),
            ) else {
                return rpc_error(id, -32602, "Invalid params");
            };
            let guard = storage.lock().expect("storage mutex poisoned");
            match guard.set_profile_cwd(name, std::path::PathBuf::from(cwd_str)) {
                Ok(()) => match guard.get(name) {
                    Ok(profile) => {
                        let resolved_cwd = guard.resolve_cwd(name);
                        json!({"jsonrpc":"2.0","id":id,"result":profile_entry_with_cwd(&profile, &resolved_cwd)}).to_string()
                    }
                    Err(e) => rpc_error(id, geese_error_code(&e), &e.to_string()),
                },
                Err(e) => rpc_error(id, geese_error_code(&e), &e.to_string()),
            }
        }

        Some("profile.unset_cwd") => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return rpc_error(id, -32602, "Invalid params");
            };
            let guard = storage.lock().expect("storage mutex poisoned");
            match guard.unset_profile_cwd(name) {
                Ok(()) => match guard.get(name) {
                    Ok(profile) => {
                        let resolved_cwd = guard.resolve_cwd(name);
                        json!({"jsonrpc":"2.0","id":id,"result":profile_entry_with_cwd(&profile, &resolved_cwd)}).to_string()
                    }
                    Err(e) => rpc_error(id, geese_error_code(&e), &e.to_string()),
                },
                Err(e) => rpc_error(id, geese_error_code(&e), &e.to_string()),
            }
        }

        Some("config.get_global") => {
            let guard = storage.lock().expect("storage mutex poisoned");
            match guard.load_global_config() {
                Ok(config) => json!({"jsonrpc":"2.0","id":id,"result":{
                    "cwd": config.cwd.as_deref().map(|p| p.to_string_lossy().into_owned()),
                }})
                .to_string(),
                Err(e) => rpc_error(id, -32000, &e.to_string()),
            }
        }

        Some("config.set_global") => {
            let guard = storage.lock().expect("storage mutex poisoned");
            let mut config = match guard.load_global_config() {
                Ok(c) => c,
                Err(e) => return rpc_error(id, -32000, &e.to_string()),
            };
            // cwd may be null to clear, or a string to set
            match params.get("cwd") {
                Some(Value::Null) => config.cwd = None,
                Some(Value::String(s)) => config.cwd = Some(std::path::PathBuf::from(s)),
                None => {} // not provided — no-op
                Some(_) => {
                    return rpc_error(id, -32602, "Invalid params: cwd must be a string or null");
                }
            }
            match guard.save_global_config(&config) {
                Ok(()) => json!({"jsonrpc":"2.0","id":id,"result":{
                    "cwd": config.cwd.as_deref().map(|p| p.to_string_lossy().into_owned()),
                }})
                .to_string(),
                Err(e) => rpc_error(id, -32000, &e.to_string()),
            }
        }

        _ => rpc_error(id, -32601, "Method not found"),
    }
}

fn profile_entry(profile: &geese::Profile) -> Value {
    json!({
        "name": profile.name(),
        "locked": profile.meta().locked,
        "parent": profile.meta().parent,
        "path": profile.path().to_string_lossy(),
    })
}

fn profile_entry_with_cwd(profile: &geese::Profile, resolved_cwd: &std::path::Path) -> Value {
    json!({
        "name": profile.name(),
        "locked": profile.meta().locked,
        "parent": profile.meta().parent,
        "path": profile.path().to_string_lossy(),
        "cwd": profile.meta().cwd.as_deref().map(|p| p.to_string_lossy().into_owned()),
        "resolved_cwd": resolved_cwd.to_string_lossy(),
    })
}

fn profile_entry_from_parts(meta: &geese::ProfileMeta, path: &std::path::Path) -> Value {
    json!({
        "name": meta.name,
        "locked": meta.locked,
        "parent": meta.parent,
        "path": path.to_string_lossy(),
    })
}

fn geese_error_code(e: &geese::Error) -> i64 {
    match e {
        geese::Error::ProfileNotFound(_) => -32001,
        geese::Error::ProfileExists(_) => -32002,
        geese::Error::InvalidName(_) => -32003,
        geese::Error::ProfileLocked(_) => -32004,
        _ => -32000,
    }
}

fn rpc_error(id: Value, code: i64, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        }
    })
    .to_string()
}

async fn wait_for_shutdown(shutdown: &mut Option<watch::Receiver<bool>>) {
    if let Some(shutdown) = shutdown {
        let _ = shutdown.changed().await;
    } else {
        std::future::pending::<()>().await;
    }
}

async fn handle_acp_connection(
    stream: UnixStream,
    storage: Arc<Mutex<geese::Storage>>,
    processes: Arc<TokioMutex<ProcessMap>>,
) -> std::io::Result<()> {
    let (mut read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(&mut read_half);
    let mut buf = Vec::new();

    // Read handshake line
    let bytes = reader.read_until(b'\n', &mut buf).await?;
    if bytes == 0 {
        return Ok(()); // Connection closed
    }

    // Trim trailing newlines and carriage returns efficiently
    if let Some(last_valid) = buf.iter().rposition(|&b| !matches!(b, b'\n' | b'\r')) {
        buf.truncate(last_valid + 1);
    } else {
        buf.clear(); // All bytes are whitespace
    }

    let handshake_line = match std::str::from_utf8(&buf) {
        Ok(line) => line,
        Err(_) => {
            let response = rpc_error(Value::Null, -32700, "Parse error");
            write_half.write_all(response.as_bytes()).await?;
            write_half.write_all(b"\n").await?;
            return Ok(());
        }
    };

    let value: Value = match serde_json::from_str(handshake_line) {
        Ok(value) => value,
        Err(_) => {
            let response = rpc_error(Value::Null, -32700, "Parse error");
            write_half.write_all(response.as_bytes()).await?;
            write_half.write_all(b"\n").await?;
            return Ok(());
        }
    };

    let id = value.get("id").cloned().unwrap_or(Value::Null);
    let method = value.get("method").and_then(Value::as_str);
    let params = value.get("params").cloned().unwrap_or(Value::Null);

    // Validate method is connect_profile
    if method != Some("connect_profile") {
        let response = rpc_error(
            id,
            -32021,
            "Invalid handshake: method must be connect_profile",
        );
        write_half.write_all(response.as_bytes()).await?;
        write_half.write_all(b"\n").await?;
        return Ok(());
    }

    // Extract profile name and make it owned to avoid lifetime issues
    let name = match params.get("name").and_then(Value::as_str) {
        Some(n) => n.to_string(),
        None => {
            let response = rpc_error(id, -32602, "Invalid params: name required");
            write_half.write_all(response.as_bytes()).await?;
            write_half.write_all(b"\n").await?;
            return Ok(());
        }
    };

    // Look up profile path
    let profile_result = {
        let guard = storage.lock().expect("storage mutex poisoned");
        guard.get(&name).map(|profile| {
            let cwd = guard.resolve_cwd(&name);
            (profile.path().to_path_buf(), cwd)
        })
    };

    let (profile_path, resolved_cwd) = match profile_result {
        Ok(pair) => pair,
        Err(geese::Error::ProfileNotFound(_)) => {
            let response = rpc_error(id, -32001, &format!("Profile not found: {}", name));
            write_half.write_all(response.as_bytes()).await?;
            write_half.write_all(b"\n").await?;
            return Ok(());
        }
        Err(e) => {
            let response = rpc_error(id, -32000, &e.to_string());
            write_half.write_all(response.as_bytes()).await?;
            write_half.write_all(b"\n").await?;
            return Ok(());
        }
    };

    // Bind ACP connection
    let handles = {
        let mut procs = processes.lock().await;
        match procs.bind_acp(&name, &profile_path, &resolved_cwd).await {
            Ok(handles) => handles,
            Err(ProcessError::ProfileInUse) => {
                let response = rpc_error(id, -32020, &format!("Profile in use: {}", name));
                write_half.write_all(response.as_bytes()).await?;
                write_half.write_all(b"\n").await?;
                return Ok(());
            }
            Err(ProcessError::GooseBinaryUnavailable(msg)) => {
                let response = rpc_error(id, -32010, &msg);
                write_half.write_all(response.as_bytes()).await?;
                write_half.write_all(b"\n").await?;
                return Ok(());
            }
            Err(ProcessError::SpawnFailed(msg)) => {
                let response = rpc_error(id, -32011, &msg);
                write_half.write_all(response.as_bytes()).await?;
                write_half.write_all(b"\n").await?;
                return Ok(());
            }
            Err(e) => {
                let response = rpc_error(id, -32000, &e.to_string());
                write_half.write_all(response.as_bytes()).await?;
                write_half.write_all(b"\n").await?;
                return Ok(());
            }
        }
    };

    // Send success response
    let response = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {"pid": handles.pid}
    })
    .to_string();
    write_half.write_all(response.as_bytes()).await?;
    write_half.write_all(b"\n").await?;

    // Enter byte pump mode
    let mut stdin = handles.stdin;
    let mut stdout = handles.stdout;

    let to_goose = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut read_half, &mut stdin).await;
    });

    let from_goose = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut stdout, &mut write_half).await;
    });

    tokio::select! {
        _ = to_goose => {},
        _ = from_goose => {},
    }

    // Unbind and stop the process
    let mut procs = processes.lock().await;
    let _ = procs.unbind_acp(&name).await;

    Ok(())
}

fn default_runtime_dir() -> PathBuf {
    match env::var_os("XDG_RUNTIME_DIR") {
        Some(runtime_dir) => PathBuf::from(runtime_dir).join("geese"),
        None => env::temp_dir().join(format!("geese-{}", Uid::current().as_raw())),
    }
}

/// Resolve the goose binary path. Resolution order:
/// 1. `override_path` from `RunOpts::with_goose_bin` (test override)
/// 2. `GEESE_GOOSE_BIN` env var (absolute path or bare name resolved against PATH)
/// 3. `which("goose")` from PATH
/// 4. `None` — start succeeds but `goosed.start` will return GooseBinaryUnavailable
fn resolve_goose_bin(override_path: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = override_path {
        return Some(p.to_path_buf());
    }

    if let Ok(val) = env::var("GEESE_GOOSE_BIN") {
        let path = PathBuf::from(&val);
        if path.is_absolute() {
            return Some(path);
        }
        // Bare name: search PATH
        if let Some(found) = search_path(&val) {
            return Some(found);
        }
    }

    search_path("goose")
}

fn search_path(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path_var = env::var_os("PATH")?;
    for dir in env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if let Ok(meta) = candidate.metadata()
            && meta.is_file()
            && meta.permissions().mode() & 0o111 != 0
        {
            return Some(candidate);
        }
    }
    None
}
