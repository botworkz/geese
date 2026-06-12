use std::path::{Path, PathBuf};

use geesed::{RunOpts, run};
use serde_json::Value;
use tempfile::tempdir;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    sync::watch,
    task::JoinHandle,
    time::{Duration, sleep, timeout},
};

fn runtime_dir(root: &Path) -> PathBuf {
    root.join("runtime")
}

fn socket_path(root: &Path) -> PathBuf {
    runtime_dir(root).join("control.sock")
}

async fn spawn_daemon(
    root: &Path,
) -> (
    watch::Sender<bool>,
    JoinHandle<Result<(), geesed::RunError>>,
) {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let task = tokio::spawn(run(RunOpts::default()
        .with_runtime_dir(runtime_dir(root))
        .with_shutdown(shutdown_rx)));
    wait_for_socket(&socket_path(root)).await;
    (shutdown_tx, task)
}

async fn wait_for_socket(path: &Path) {
    timeout(Duration::from_secs(5), async {
        loop {
            if path.exists() {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
}

async fn read_json_line(reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>) -> Value {
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    serde_json::from_str(line.trim_end()).unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn control_socket_handles_status_errors_and_reuse() {
    let tempdir = tempdir().unwrap();
    let socket = socket_path(tempdir.path());
    let (shutdown_tx, task) = spawn_daemon(tempdir.path()).await;

    let stream = UnixStream::connect(&socket).await.unwrap();
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    write_half
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"status\"}\n")
        .await
        .unwrap();
    let status = read_json_line(&mut reader).await;
    assert_eq!(
        status["result"]["pid"].as_u64().unwrap(),
        std::process::id() as u64
    );
    assert!(!status["result"]["version"].as_str().unwrap().is_empty());
    assert!(!status["result"]["started_at"].as_str().unwrap().is_empty());
    assert!(status["result"]["uptime_ms"].as_u64().is_some());

    write_half
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"wat\"}\n")
        .await
        .unwrap();
    let unknown = read_json_line(&mut reader).await;
    assert_eq!(unknown["error"]["code"].as_i64().unwrap(), -32601);

    write_half.write_all(b"{oops\n").await.unwrap();
    let malformed = read_json_line(&mut reader).await;
    assert_eq!(malformed["error"]["code"].as_i64().unwrap(), -32700);

    write_half
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"status\"}\n{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"status\"}\n")
        .await
        .unwrap();
    let first = read_json_line(&mut reader).await;
    let second = read_json_line(&mut reader).await;
    assert_eq!(first["id"].as_u64().unwrap(), 3);
    assert_eq!(second["id"].as_u64().unwrap(), 4);

    drop(reader);
    drop(write_half);
    shutdown_tx.send(true).unwrap();
    task.await.unwrap().unwrap();
}
