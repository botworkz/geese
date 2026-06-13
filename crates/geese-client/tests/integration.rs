use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use geese_client::{ClientError, GeesedClient};
use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use tempfile::tempdir;

fn spawn_geesed(runtime_root: &Path) -> Child {
    Command::new(assert_cmd::cargo::cargo_bin("geesed"))
        .env("XDG_RUNTIME_DIR", runtime_root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn socket_path(runtime_root: &Path) -> PathBuf {
    runtime_root.join("geese").join("control.sock")
}

fn wait_for_socket(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }

    panic!("socket did not appear at {}", path.display());
}

#[tokio::test(flavor = "current_thread")]
async fn status_reads_from_real_daemon() {
    let tempdir = tempdir().unwrap();
    let socket = socket_path(tempdir.path());
    let mut daemon = spawn_geesed(tempdir.path());
    wait_for_socket(&socket);

    let mut client = GeesedClient::connect_at(&socket).await.unwrap();
    let status = client.status().await.unwrap();

    assert_eq!(status.pid, daemon.id());
    assert_eq!(status.version, env!("CARGO_PKG_VERSION"));
    assert!(!status.started_at.is_empty());

    kill(Pid::from_raw(daemon.id() as i32), Signal::SIGTERM).unwrap();
    assert!(daemon.wait().unwrap().success());
}

#[tokio::test(flavor = "current_thread")]
async fn connect_at_reports_not_running() {
    let path = PathBuf::from("/nonexistent/path");
    let error = GeesedClient::connect_at(&path).await.unwrap_err();

    assert!(matches!(error, ClientError::NotRunning(actual) if actual == path));
}
