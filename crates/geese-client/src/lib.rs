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
}

#[derive(Debug, Deserialize)]
pub struct StatusResponse {
    pub pid: u32,
    pub version: String,
    pub uptime_ms: u64,
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
