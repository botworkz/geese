use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
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
    sync::watch,
    task::JoinSet,
};
use tracing::info;

#[derive(Debug, Default)]
pub struct RunOpts {
    pub runtime_dir: Option<PathBuf>,
    pub shutdown: Option<watch::Receiver<bool>>,
}

impl RunOpts {
    pub fn with_runtime_dir(mut self, runtime_dir: impl Into<PathBuf>) -> Self {
        self.runtime_dir = Some(runtime_dir.into());
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
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub async fn run(opts: RunOpts) -> Result<(), RunError> {
    let runtime_dir = RuntimeDir::new(opts.runtime_dir.unwrap_or_else(default_runtime_dir));
    runtime_dir.ensure()?;

    let lockfile = Lockfile::acquire(runtime_dir.lockfile_path())?;
    let control_socket = ControlSocket::bind(runtime_dir.socket_path())?;
    let state = Arc::new(DaemonState::new(lockfile.pid()));

    info!(
        "geesed v{} listening on {} pid={}",
        env!("CARGO_PKG_VERSION"),
        control_socket.path().display(),
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
                tasks.spawn(async move {
                    if let Err(error) = handle_connection(stream, state).await {
                        tracing::debug!("control connection ended with error: {error}");
                    }
                });
            }
            _ = sigterm.recv() => break,
            _ = sigint.recv() => break,
            _ = wait_for_shutdown(&mut shutdown) => break,
        }
    }

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

async fn handle_connection(stream: UnixStream, state: Arc<DaemonState>) -> std::io::Result<()> {
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
            Ok(line) => rpc_response(line, &state),
            Err(_) => rpc_error(Value::Null, -32700, "Parse error"),
        };

        write_half.write_all(response.as_bytes()).await?;
        write_half.write_all(b"\n").await?;
    }
}

fn rpc_response(line: &str, state: &DaemonState) -> String {
    let value: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(_) => return rpc_error(Value::Null, -32700, "Parse error"),
    };

    let id = value.get("id").cloned().unwrap_or(Value::Null);
    match value.get("method").and_then(Value::as_str) {
        Some("status") => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": state.status(),
        })
        .to_string(),
        _ => rpc_error(id, -32601, "Method not found"),
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

fn default_runtime_dir() -> PathBuf {
    match env::var_os("XDG_RUNTIME_DIR") {
        Some(runtime_dir) => PathBuf::from(runtime_dir).join("geese"),
        None => env::temp_dir().join(format!("geese-{}", Uid::current().as_raw())),
    }
}
