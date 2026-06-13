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

#[tokio::test(flavor = "current_thread")]
async fn profile_crud_via_daemon() {
    let tempdir = tempdir().unwrap();
    let geese_root = tempdir.path().join("geese-root");
    let (shutdown_tx, task) = spawn_daemon_with_root(tempdir.path(), geese_root.clone()).await;

    let stream = UnixStream::connect(socket_path(tempdir.path()))
        .await
        .unwrap();
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // list — empty initially
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":1,"method":"profile.list"}),
    )
    .await;
    assert_eq!(resp["result"].as_array().unwrap().len(), 0);

    // create
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":2,"method":"profile.create","params":{"name":"work"}}),
    )
    .await;
    assert_eq!(resp["result"]["name"], "work");
    assert_eq!(resp["result"]["locked"], false);
    assert!(resp["result"]["path"].as_str().unwrap().contains("work"));

    // list — one profile
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":3,"method":"profile.list"}),
    )
    .await;
    assert_eq!(resp["result"].as_array().unwrap().len(), 1);
    assert_eq!(resp["result"][0]["name"], "work");

    // get
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":4,"method":"profile.get","params":{"name":"work"}}),
    )
    .await;
    assert_eq!(resp["result"]["name"], "work");

    // lock
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":5,"method":"profile.lock","params":{"name":"work"}}),
    )
    .await;
    assert_eq!(resp["result"]["locked"], true);

    // delete while locked — should fail with -32004
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":6,"method":"profile.delete","params":{"name":"work"}}),
    )
    .await;
    assert_eq!(resp["error"]["code"].as_i64().unwrap(), -32004);

    // unlock
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":7,"method":"profile.unlock","params":{"name":"work"}}),
    )
    .await;
    assert_eq!(resp["result"]["locked"], false);

    // copy
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":8,"method":"profile.copy","params":{"src":"work","dest":"home"}}),
    )
    .await;
    assert_eq!(resp["result"]["name"], "home");
    assert_eq!(resp["result"]["parent"], "work");

    // delete work
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":9,"method":"profile.delete","params":{"name":"work"}}),
    )
    .await;
    assert!(resp.get("error").is_none() || resp["error"].is_null());
    assert!(resp["result"].is_null());

    // get nonexistent — should fail with -32001
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":10,"method":"profile.get","params":{"name":"nonexistent"}}),
    )
    .await;
    assert_eq!(resp["error"]["code"].as_i64().unwrap(), -32001);

    // create with invalid name — should fail with -32003
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":11,"method":"profile.create","params":{"name":"bad.name"}}),
    )
    .await;
    assert_eq!(resp["error"]["code"].as_i64().unwrap(), -32003);

    // missing params — should fail with -32602
    let resp = send_rpc(
        &mut write_half,
        &mut reader,
        json!({"jsonrpc":"2.0","id":12,"method":"profile.create","params":{}}),
    )
    .await;
    assert_eq!(resp["error"]["code"].as_i64().unwrap(), -32602);

    drop(reader);
    drop(write_half);
    shutdown_tx.send(true).unwrap();
    task.await.unwrap().unwrap();
}
