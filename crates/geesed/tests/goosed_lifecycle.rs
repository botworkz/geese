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

fn socket_path(root: &Path) -> PathBuf {
    runtime_dir(root).join("control.sock")
}

fn mock_goose_bin() -> PathBuf {
    assert_cmd::cargo::cargo_bin("mock-goose")
}

fn mock_goose_exit_bin() -> PathBuf {
    assert_cmd::cargo::cargo_bin("mock-goose-exit")
}

async fn spawn_daemon(
    root: &Path,
    geese_root: PathBuf,
    goose_bin: Option<PathBuf>,
) -> (
    watch::Sender<bool>,
    JoinHandle<Result<(), geesed::RunError>>,
) {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut opts = RunOpts::default()
        .with_runtime_dir(runtime_dir(root))
        .with_geese_root(geese_root)
        .with_shutdown(shutdown_rx);
    if let Some(bin) = goose_bin {
        opts = opts.with_goose_bin(bin);
    }
    let task = tokio::spawn(run(opts));
    timeout(Duration::from_secs(5), async {
        loop {
            if socket_path(root).exists() {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    (shutdown_tx, task)
}

async fn connect(
    root: &Path,
) -> (
    BufReader<tokio::net::unix::OwnedReadHalf>,
    tokio::net::unix::OwnedWriteHalf,
) {
    let stream = UnixStream::connect(socket_path(root)).await.unwrap();
    let (r, w) = stream.into_split();
    (BufReader::new(r), w)
}

async fn send_rpc(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    request: Value,
) -> Value {
    let mut line = request.to_string();
    line.push('\n');
    writer.write_all(line.as_bytes()).await.unwrap();
    let mut response_line = String::new();
    reader.read_line(&mut response_line).await.unwrap();
    serde_json::from_str(response_line.trim_end()).unwrap()
}

async fn create_profile(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    name: &str,
) {
    let resp = send_rpc(
        writer,
        reader,
        json!({"jsonrpc":"2.0","id":1,"method":"profile.create","params":{"name":name}}),
    )
    .await;
    assert!(resp.get("error").is_none(), "create_profile failed: {resp}");
}

#[tokio::test(flavor = "current_thread")]
async fn start_returns_pid_and_appears_in_list() {
    let tempdir = tempdir().unwrap();
    let geese_root = tempdir.path().join("geese-root");
    let (shutdown_tx, task) =
        spawn_daemon(tempdir.path(), geese_root, Some(mock_goose_bin())).await;

    let (mut reader, mut writer) = connect(tempdir.path()).await;
    create_profile(&mut writer, &mut reader, "work").await;

    let resp = send_rpc(
        &mut writer,
        &mut reader,
        json!({"jsonrpc":"2.0","id":2,"method":"goosed.start","params":{"name":"work"}}),
    )
    .await;
    assert!(resp.get("error").is_none(), "goosed.start failed: {resp}");
    let pid = resp["result"]["pid"].as_u64().unwrap();
    assert!(pid > 0);

    let resp = send_rpc(
        &mut writer,
        &mut reader,
        json!({"jsonrpc":"2.0","id":3,"method":"goosed.list_running","params":{}}),
    )
    .await;
    let list = resp["result"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["name"], "work");
    assert_eq!(list[0]["pid"].as_u64().unwrap(), pid);
    assert!(!list[0]["started_at"].as_str().unwrap().is_empty());

    // Clean up
    send_rpc(
        &mut writer,
        &mut reader,
        json!({"jsonrpc":"2.0","id":4,"method":"goosed.kill","params":{"name":"work"}}),
    )
    .await;

    shutdown_tx.send(true).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn start_is_idempotent() {
    let tempdir = tempdir().unwrap();
    let geese_root = tempdir.path().join("geese-root");
    let (shutdown_tx, task) =
        spawn_daemon(tempdir.path(), geese_root, Some(mock_goose_bin())).await;

    let (mut reader, mut writer) = connect(tempdir.path()).await;
    create_profile(&mut writer, &mut reader, "work").await;

    let resp1 = send_rpc(
        &mut writer,
        &mut reader,
        json!({"jsonrpc":"2.0","id":2,"method":"goosed.start","params":{"name":"work"}}),
    )
    .await;
    let pid1 = resp1["result"]["pid"].as_u64().unwrap();

    // Second start: must return same pid, not spawn a new process
    let resp2 = send_rpc(
        &mut writer,
        &mut reader,
        json!({"jsonrpc":"2.0","id":3,"method":"goosed.start","params":{"name":"work"}}),
    )
    .await;
    let pid2 = resp2["result"]["pid"].as_u64().unwrap();

    assert_eq!(pid1, pid2, "idempotent start must return same pid");

    // Only one entry in list
    let resp = send_rpc(
        &mut writer,
        &mut reader,
        json!({"jsonrpc":"2.0","id":4,"method":"goosed.list_running","params":{}}),
    )
    .await;
    assert_eq!(resp["result"].as_array().unwrap().len(), 1);

    send_rpc(
        &mut writer,
        &mut reader,
        json!({"jsonrpc":"2.0","id":5,"method":"goosed.kill","params":{"name":"work"}}),
    )
    .await;

    shutdown_tx.send(true).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn stop_removes_from_list_and_exits_process() {
    let tempdir = tempdir().unwrap();
    let geese_root = tempdir.path().join("geese-root");
    let (shutdown_tx, task) =
        spawn_daemon(tempdir.path(), geese_root, Some(mock_goose_bin())).await;

    let (mut reader, mut writer) = connect(tempdir.path()).await;
    create_profile(&mut writer, &mut reader, "work").await;

    let resp = send_rpc(
        &mut writer,
        &mut reader,
        json!({"jsonrpc":"2.0","id":2,"method":"goosed.start","params":{"name":"work"}}),
    )
    .await;
    let pid = resp["result"]["pid"].as_u64().unwrap() as u32;

    // Process should be alive
    assert!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok(),
        "process should be alive before stop"
    );

    let resp = send_rpc(
        &mut writer,
        &mut reader,
        json!({"jsonrpc":"2.0","id":3,"method":"goosed.stop","params":{"name":"work"}}),
    )
    .await;
    assert!(resp.get("error").is_none(), "goosed.stop failed: {resp}");

    // No longer in list
    let resp = send_rpc(
        &mut writer,
        &mut reader,
        json!({"jsonrpc":"2.0","id":4,"method":"goosed.list_running","params":{}}),
    )
    .await;
    assert_eq!(resp["result"].as_array().unwrap().len(), 0);

    // Process should be dead (retry briefly rather than fixed sleep)
    let dead = {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let result = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None);
            if result.is_err() {
                break true;
            }
            if tokio::time::Instant::now() >= deadline {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    assert!(dead, "process should be dead after stop (pid={pid})");

    shutdown_tx.send(true).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn kill_removes_from_list_and_exits_process() {
    let tempdir = tempdir().unwrap();
    let geese_root = tempdir.path().join("geese-root");
    let (shutdown_tx, task) =
        spawn_daemon(tempdir.path(), geese_root, Some(mock_goose_bin())).await;

    let (mut reader, mut writer) = connect(tempdir.path()).await;
    create_profile(&mut writer, &mut reader, "work").await;

    let resp = send_rpc(
        &mut writer,
        &mut reader,
        json!({"jsonrpc":"2.0","id":2,"method":"goosed.start","params":{"name":"work"}}),
    )
    .await;
    let pid = resp["result"]["pid"].as_u64().unwrap() as u32;

    let resp = send_rpc(
        &mut writer,
        &mut reader,
        json!({"jsonrpc":"2.0","id":3,"method":"goosed.kill","params":{"name":"work"}}),
    )
    .await;
    assert!(resp.get("error").is_none(), "goosed.kill failed: {resp}");

    // No longer in list
    let resp = send_rpc(
        &mut writer,
        &mut reader,
        json!({"jsonrpc":"2.0","id":4,"method":"goosed.list_running","params":{}}),
    )
    .await;
    assert_eq!(resp["result"].as_array().unwrap().len(), 0);

    // Process should be dead (retry briefly rather than fixed sleep)
    let dead = {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let result = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None);
            if result.is_err() {
                break true;
            }
            if tokio::time::Instant::now() >= deadline {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    assert!(dead, "process should be dead after kill (pid={pid})");

    shutdown_tx.send(true).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn stop_unknown_profile_is_noop() {
    let tempdir = tempdir().unwrap();
    let geese_root = tempdir.path().join("geese-root");
    let (shutdown_tx, task) =
        spawn_daemon(tempdir.path(), geese_root, Some(mock_goose_bin())).await;

    let (mut reader, mut writer) = connect(tempdir.path()).await;

    // Stop a profile that was never started — should succeed with null result
    let resp = send_rpc(
        &mut writer,
        &mut reader,
        json!({"jsonrpc":"2.0","id":1,"method":"goosed.stop","params":{"name":"nonexistent"}}),
    )
    .await;
    assert!(
        resp.get("error").is_none(),
        "stop of unknown profile should be a no-op: {resp}"
    );
    assert!(resp["result"].is_null());

    shutdown_tx.send(true).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn kill_unknown_profile_is_noop() {
    let tempdir = tempdir().unwrap();
    let geese_root = tempdir.path().join("geese-root");
    let (shutdown_tx, task) =
        spawn_daemon(tempdir.path(), geese_root, Some(mock_goose_bin())).await;

    let (mut reader, mut writer) = connect(tempdir.path()).await;

    let resp = send_rpc(
        &mut writer,
        &mut reader,
        json!({"jsonrpc":"2.0","id":1,"method":"goosed.kill","params":{"name":"nonexistent"}}),
    )
    .await;
    assert!(
        resp.get("error").is_none(),
        "kill of unknown profile should be a no-op: {resp}"
    );
    assert!(resp["result"].is_null());

    shutdown_tx.send(true).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn start_unknown_profile_returns_profile_not_found() {
    let tempdir = tempdir().unwrap();
    let geese_root = tempdir.path().join("geese-root");
    let (shutdown_tx, task) =
        spawn_daemon(tempdir.path(), geese_root, Some(mock_goose_bin())).await;

    let (mut reader, mut writer) = connect(tempdir.path()).await;

    let resp = send_rpc(
        &mut writer,
        &mut reader,
        json!({"jsonrpc":"2.0","id":1,"method":"goosed.start","params":{"name":"nosuchprofile"}}),
    )
    .await;
    assert_eq!(
        resp["error"]["code"].as_i64().unwrap(),
        -32001,
        "expected ProfileNotFound (-32001): {resp}"
    );

    shutdown_tx.send(true).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn goose_binary_missing_returns_clear_error() {
    let tempdir = tempdir().unwrap();
    let profile_path = tempdir.path().join("profiles").join("work");
    std::fs::create_dir_all(&profile_path).unwrap();

    // ProcessMap with no binary: start must return GooseBinaryUnavailable
    let mut pm = geesed::processes::ProcessMap::new(None);
    let err = pm
        .start("work", &profile_path, tempdir.path())
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            geesed::processes::ProcessError::GooseBinaryUnavailable(_)
        ),
        "expected GooseBinaryUnavailable, got: {err}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn spawn_failure_surfaces_as_spawn_failed() {
    let tempdir = tempdir().unwrap();
    let geese_root = tempdir.path().join("geese-root");
    let (shutdown_tx, task) =
        spawn_daemon(tempdir.path(), geese_root, Some(mock_goose_exit_bin())).await;

    let (mut reader, mut writer) = connect(tempdir.path()).await;
    create_profile(&mut writer, &mut reader, "work").await;

    let resp = send_rpc(
        &mut writer,
        &mut reader,
        json!({"jsonrpc":"2.0","id":2,"method":"goosed.start","params":{"name":"work"}}),
    )
    .await;
    assert_eq!(
        resp["error"]["code"].as_i64().unwrap(),
        -32011,
        "expected SpawnFailed (-32011): {resp}"
    );

    shutdown_tx.send(true).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn list_running_empty_initially() {
    let tempdir = tempdir().unwrap();
    let geese_root = tempdir.path().join("geese-root");
    let (shutdown_tx, task) =
        spawn_daemon(tempdir.path(), geese_root, Some(mock_goose_bin())).await;

    let (mut reader, mut writer) = connect(tempdir.path()).await;

    let resp = send_rpc(
        &mut writer,
        &mut reader,
        json!({"jsonrpc":"2.0","id":1,"method":"goosed.list_running","params":{}}),
    )
    .await;
    assert!(resp.get("error").is_none());
    assert_eq!(resp["result"].as_array().unwrap().len(), 0);

    shutdown_tx.send(true).unwrap();
    task.await.unwrap().unwrap();
}
