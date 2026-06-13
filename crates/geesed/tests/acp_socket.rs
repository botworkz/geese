use std::path::{Path, PathBuf};

use geesed::{RunOpts, run};
use serde_json::{Value, json};
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

fn acp_socket_path(root: &Path) -> PathBuf {
    runtime_dir(root).join("acp.sock")
}

fn control_socket_path(root: &Path) -> PathBuf {
    runtime_dir(root).join("control.sock")
}

async fn spawn_daemon(
    root: &Path,
    goose_bin: PathBuf,
) -> (
    watch::Sender<bool>,
    JoinHandle<Result<(), geesed::RunError>>,
) {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let task = tokio::spawn(run(RunOpts::default()
        .with_runtime_dir(runtime_dir(root))
        .with_geese_root(root)
        .with_goose_bin(goose_bin)
        .with_shutdown(shutdown_rx)));
    wait_for_socket(&control_socket_path(root)).await;
    wait_for_socket(&acp_socket_path(root)).await;
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
    serde_json::from_str(&line).unwrap()
}

async fn send_json_line(writer: &mut tokio::net::unix::OwnedWriteHalf, value: &Value) {
    let line = value.to_string();
    writer.write_all(line.as_bytes()).await.unwrap();
    writer.write_all(b"\n").await.unwrap();
}

async fn create_profile_via_control(root: &Path, name: &str) {
    let stream = UnixStream::connect(control_socket_path(root))
        .await
        .unwrap();
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "profile.create",
        "params": {"name": name}
    });
    send_json_line(&mut write_half, &request).await;
    let response = read_json_line(&mut reader).await;
    assert!(response.get("result").is_some());
}

#[tokio::test]
async fn acp_handshake_success() {
    let root = tempdir().unwrap();
    let goose_bin = PathBuf::from(env!("CARGO_BIN_EXE_mock-goose"));
    let (_shutdown_tx, _task) = spawn_daemon(root.path(), goose_bin).await;

    // Create a profile
    create_profile_via_control(root.path(), "work").await;

    // Connect to ACP socket
    let stream = UnixStream::connect(acp_socket_path(root.path()))
        .await
        .unwrap();
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // Send connect_profile handshake
    let handshake = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "connect_profile",
        "params": {"name": "work"}
    });
    send_json_line(&mut write_half, &handshake).await;

    // Read response
    let response = read_json_line(&mut reader).await;
    assert_eq!(response.get("jsonrpc").and_then(Value::as_str), Some("2.0"));
    assert_eq!(response.get("id"), Some(&json!(1)));
    assert!(response.get("result").is_some());
    assert!(response.get("result").unwrap().get("pid").is_some());
}

#[tokio::test]
async fn acp_handshake_invalid_method() {
    let root = tempdir().unwrap();
    let goose_bin = PathBuf::from(env!("CARGO_BIN_EXE_mock-goose"));
    let (_shutdown_tx, _task) = spawn_daemon(root.path(), goose_bin).await;

    create_profile_via_control(root.path(), "work").await;

    let stream = UnixStream::connect(acp_socket_path(root.path()))
        .await
        .unwrap();
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // Send invalid method
    let handshake = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "invalid_method",
        "params": {"name": "work"}
    });
    send_json_line(&mut write_half, &handshake).await;

    let response = read_json_line(&mut reader).await;
    assert!(response.get("error").is_some());
    assert_eq!(
        response.get("error").unwrap().get("code"),
        Some(&json!(-32021))
    );
}

#[tokio::test]
async fn acp_handshake_missing_params() {
    let root = tempdir().unwrap();
    let goose_bin = PathBuf::from(env!("CARGO_BIN_EXE_mock-goose"));
    let (_shutdown_tx, _task) = spawn_daemon(root.path(), goose_bin).await;

    let stream = UnixStream::connect(acp_socket_path(root.path()))
        .await
        .unwrap();
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // Send handshake without params
    let handshake = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "connect_profile",
        "params": {}
    });
    send_json_line(&mut write_half, &handshake).await;

    let response = read_json_line(&mut reader).await;
    assert!(response.get("error").is_some());
    assert_eq!(
        response.get("error").unwrap().get("code"),
        Some(&json!(-32602))
    );
}

#[tokio::test]
async fn acp_handshake_profile_not_found() {
    let root = tempdir().unwrap();
    let goose_bin = PathBuf::from(env!("CARGO_BIN_EXE_mock-goose"));
    let (_shutdown_tx, _task) = spawn_daemon(root.path(), goose_bin).await;

    let stream = UnixStream::connect(acp_socket_path(root.path()))
        .await
        .unwrap();
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // Send handshake for non-existent profile
    let handshake = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "connect_profile",
        "params": {"name": "nonexistent"}
    });
    send_json_line(&mut write_half, &handshake).await;

    let response = read_json_line(&mut reader).await;
    assert!(response.get("error").is_some());
    assert_eq!(
        response.get("error").unwrap().get("code"),
        Some(&json!(-32001))
    );
}

#[tokio::test]
async fn acp_handshake_profile_in_use() {
    let root = tempdir().unwrap();
    let goose_bin = PathBuf::from(env!("CARGO_BIN_EXE_mock-goose"));
    let (_shutdown_tx, _task) = spawn_daemon(root.path(), goose_bin).await;

    create_profile_via_control(root.path(), "work").await;

    // First connection
    let stream1 = UnixStream::connect(acp_socket_path(root.path()))
        .await
        .unwrap();
    let (read_half1, mut write_half1) = stream1.into_split();
    let mut reader1 = BufReader::new(read_half1);

    let handshake = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "connect_profile",
        "params": {"name": "work"}
    });
    send_json_line(&mut write_half1, &handshake).await;
    let response1 = read_json_line(&mut reader1).await;
    assert!(response1.get("result").is_some());

    // Second connection should fail with ProfileInUse
    let stream2 = UnixStream::connect(acp_socket_path(root.path()))
        .await
        .unwrap();
    let (read_half2, mut write_half2) = stream2.into_split();
    let mut reader2 = BufReader::new(read_half2);

    send_json_line(&mut write_half2, &handshake).await;
    let response2 = read_json_line(&mut reader2).await;
    assert!(response2.get("error").is_some());
    assert_eq!(
        response2.get("error").unwrap().get("code"),
        Some(&json!(-32020))
    );
}
