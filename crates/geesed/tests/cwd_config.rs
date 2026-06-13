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

async fn spawn_daemon_with_root(
    root: &Path,
    geese_root: PathBuf,
) -> (
    watch::Sender<bool>,
    JoinHandle<Result<(), geesed::RunError>>,
) {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let task = tokio::spawn(run(RunOpts::default()
        .with_runtime_dir(runtime_dir(root))
        .with_geese_root(geese_root)
        .with_shutdown(shutdown_rx)));
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

// Serialize tests that modify process-wide environment variables.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// profile.get returns resolved_cwd; set/unset round-trips correctly.
#[tokio::test(flavor = "current_thread")]
async fn profile_get_includes_resolved_cwd() {
    let _env_guard = ENV_LOCK.lock().await;
    let tempdir = tempdir().unwrap();
    let geese_root = tempdir.path().join("geese-root");
    let config_dir = tempdir.path().join("xdg-config");
    // Use an isolated XDG_CONFIG_HOME so this test doesn't read the real
    // global config file.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", &config_dir) };
    unsafe { std::env::remove_var("GEESE_CWD") };
    unsafe { std::env::remove_var("GEESE_PROFILE_CWD_WORK") };

    let (shutdown_tx, task) = spawn_daemon_with_root(tempdir.path(), geese_root.clone()).await;

    let stream = UnixStream::connect(socket_path(tempdir.path()))
        .await
        .unwrap();
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // Create profile
    send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":1,"method":"profile.create","params":{"name":"work"}}),
    )
    .await;

    // profile.get should include resolved_cwd (falls back to home dir since nothing set)
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":2,"method":"profile.get","params":{"name":"work"}}),
    )
    .await;
    assert!(resp["result"]["resolved_cwd"].as_str().is_some());
    assert_eq!(resp["result"]["cwd"], Value::Null);

    // Set per-profile cwd
    let profile_cwd = tempdir.path().join("my-work-dir");
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":3,"method":"profile.set_cwd","params":{
            "name":"work","cwd": profile_cwd.to_string_lossy().as_ref()
        }}),
    )
    .await;
    assert_eq!(
        resp["result"]["cwd"].as_str().unwrap(),
        profile_cwd.to_string_lossy().as_ref()
    );
    assert_eq!(
        resp["result"]["resolved_cwd"].as_str().unwrap(),
        profile_cwd.to_string_lossy().as_ref()
    );

    // profile.get now reflects the per-profile cwd
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":4,"method":"profile.get","params":{"name":"work"}}),
    )
    .await;
    assert_eq!(
        resp["result"]["resolved_cwd"].as_str().unwrap(),
        profile_cwd.to_string_lossy().as_ref()
    );

    // Unset per-profile cwd
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":5,"method":"profile.unset_cwd","params":{"name":"work"}}),
    )
    .await;
    assert_eq!(resp["result"]["cwd"], Value::Null);
    // resolved_cwd should now fall back (home dir or global config)
    assert!(resp["result"]["resolved_cwd"].as_str().is_some());

    shutdown_tx.send(true).unwrap();
    task.await.unwrap().unwrap();
}

/// config.get_global / config.set_global round-trip.
#[tokio::test(flavor = "current_thread")]
async fn global_config_get_set_round_trip() {
    let _env_guard = ENV_LOCK.lock().await;
    let tempdir = tempdir().unwrap();
    let geese_root = tempdir.path().join("geese-root");
    let config_dir = tempdir.path().join("xdg-config");
    unsafe { std::env::set_var("XDG_CONFIG_HOME", &config_dir) };
    unsafe { std::env::remove_var("GEESE_CWD") };

    let (shutdown_tx, task) = spawn_daemon_with_root(tempdir.path(), geese_root).await;

    let stream = UnixStream::connect(socket_path(tempdir.path()))
        .await
        .unwrap();
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // get_global — initially no cwd set
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":1,"method":"config.get_global","params":{}}),
    )
    .await;
    assert!(resp.get("error").is_none());
    assert_eq!(resp["result"]["cwd"], Value::Null);

    // set_global cwd
    let global_cwd = tempdir.path().join("global-work");
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":2,"method":"config.set_global","params":{
            "cwd": global_cwd.to_string_lossy().as_ref()
        }}),
    )
    .await;
    assert!(resp.get("error").is_none());
    assert_eq!(
        resp["result"]["cwd"].as_str().unwrap(),
        global_cwd.to_string_lossy().as_ref()
    );

    // get_global should now return the set value
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":3,"method":"config.get_global","params":{}}),
    )
    .await;
    assert_eq!(
        resp["result"]["cwd"].as_str().unwrap(),
        global_cwd.to_string_lossy().as_ref()
    );

    // Clear global cwd by setting to null
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":4,"method":"config.set_global","params":{"cwd": null}}),
    )
    .await;
    assert!(resp.get("error").is_none());
    assert_eq!(resp["result"]["cwd"], Value::Null);

    shutdown_tx.send(true).unwrap();
    task.await.unwrap().unwrap();
}

/// profile.get resolved_cwd falls back through the chain:
/// per-profile → global config → home dir.
#[tokio::test(flavor = "current_thread")]
async fn resolved_cwd_falls_back_to_global_then_home() {
    let _env_guard = ENV_LOCK.lock().await;
    let tempdir = tempdir().unwrap();
    let geese_root = tempdir.path().join("geese-root");
    let config_dir = tempdir.path().join("xdg-config");
    unsafe { std::env::set_var("XDG_CONFIG_HOME", &config_dir) };
    unsafe { std::env::remove_var("GEESE_CWD") };
    unsafe { std::env::remove_var("GEESE_PROFILE_CWD_WORK") };

    let (shutdown_tx, task) = spawn_daemon_with_root(tempdir.path(), geese_root).await;

    let stream = UnixStream::connect(socket_path(tempdir.path()))
        .await
        .unwrap();
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // Create profile
    send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":1,"method":"profile.create","params":{"name":"work"}}),
    )
    .await;

    // Set a per-profile cwd
    let profile_cwd = tempdir.path().join("profile-dir");
    send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":2,"method":"profile.set_cwd","params":{
            "name":"work","cwd": profile_cwd.to_string_lossy().as_ref()
        }}),
    )
    .await;

    // Set a global cwd
    let global_cwd = tempdir.path().join("global-dir");
    send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":3,"method":"config.set_global","params":{
            "cwd": global_cwd.to_string_lossy().as_ref()
        }}),
    )
    .await;

    // resolved_cwd is the per-profile value
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":4,"method":"profile.get","params":{"name":"work"}}),
    )
    .await;
    assert_eq!(
        resp["result"]["resolved_cwd"].as_str().unwrap(),
        profile_cwd.to_string_lossy().as_ref()
    );

    // Unset per-profile cwd → falls back to global
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":5,"method":"profile.unset_cwd","params":{"name":"work"}}),
    )
    .await;
    assert_eq!(
        resp["result"]["resolved_cwd"].as_str().unwrap(),
        global_cwd.to_string_lossy().as_ref()
    );

    // Clear global → falls back to home dir
    send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":6,"method":"config.set_global","params":{"cwd": null}}),
    )
    .await;
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":7,"method":"profile.get","params":{"name":"work"}}),
    )
    .await;
    let resolved = resp["result"]["resolved_cwd"].as_str().unwrap();
    let home = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .to_string_lossy()
        .into_owned();
    assert_eq!(resolved, home);

    shutdown_tx.send(true).unwrap();
    task.await.unwrap().unwrap();
}

/// A non-existent configured cwd must not cause spawn to fail (ENOENT guard).
/// The process should start successfully, falling back to geesed's own cwd.
#[tokio::test(flavor = "current_thread")]
async fn start_with_nonexistent_cwd_succeeds() {
    let tempdir = tempdir().unwrap();
    let profile_path = tempdir.path().join("profiles").join("work");
    std::fs::create_dir_all(&profile_path).unwrap();

    let mock_bin = assert_cmd::cargo::cargo_bin("mock-goose");
    let nonexistent = std::path::PathBuf::from("/does/not/exist/whatsoever");

    let mut pm = geesed::processes::ProcessMap::new(Some(mock_bin));
    // Must succeed despite the cwd not existing on disk
    let result = pm.start("work", &profile_path, &nonexistent).await;
    assert!(
        result.is_ok(),
        "start should succeed with a non-existent cwd, got: {result:?}"
    );
    // Clean up
    pm.kill("work").await.unwrap();
}

/// `config unset cwd` (CLI) clears the global cwd; `config get cwd` then shows `<not set>`.
#[tokio::test(flavor = "current_thread")]
async fn config_unset_cwd_clears_global_config() {
    let _env_guard = ENV_LOCK.lock().await;
    let tempdir = tempdir().unwrap();
    let geese_root = tempdir.path().join("geese-root");
    let config_dir = tempdir.path().join("xdg-config");
    unsafe { std::env::set_var("XDG_CONFIG_HOME", &config_dir) };
    unsafe { std::env::remove_var("GEESE_CWD") };

    let (shutdown_tx, task) = spawn_daemon_with_root(tempdir.path(), geese_root).await;

    let stream = UnixStream::connect(socket_path(tempdir.path()))
        .await
        .unwrap();
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // Set a global cwd via RPC
    let global_cwd = tempdir.path().join("some-dir");
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":1,"method":"config.set_global","params":{
            "cwd": global_cwd.to_string_lossy().as_ref()
        }}),
    )
    .await;
    assert!(resp.get("error").is_none(), "set_global failed: {resp}");
    assert_eq!(
        resp["result"]["cwd"].as_str().unwrap(),
        global_cwd.to_string_lossy().as_ref()
    );

    // Unset via null (mirrors what `geese config unset cwd` does via config.set_global RPC with cwd: null)
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":2,"method":"config.set_global","params":{"cwd": null}}),
    )
    .await;
    assert!(resp.get("error").is_none(), "unset failed: {resp}");
    assert_eq!(resp["result"]["cwd"], Value::Null);

    // get_global should now show no cwd
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":3,"method":"config.get_global","params":{}}),
    )
    .await;
    assert!(resp.get("error").is_none());
    assert_eq!(
        resp["result"]["cwd"],
        Value::Null,
        "cwd should be null after unset"
    );

    shutdown_tx.send(true).unwrap();
    task.await.unwrap().unwrap();
}
