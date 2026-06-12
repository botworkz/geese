use std::{
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Stdio},
    thread,
    time::{Duration, Instant},
};

use assert_cmd::Command;
use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use tempfile::tempdir;

fn geese() -> Command {
    Command::cargo_bin("geese").unwrap()
}

fn spawn_geesed(runtime_root: &Path) -> Child {
    ProcessCommand::new(assert_cmd::cargo::cargo_bin("geesed"))
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

#[test]
fn status_reports_running_daemon() {
    let tempdir = tempdir().unwrap();
    let socket = socket_path(tempdir.path());
    let mut daemon = spawn_geesed(tempdir.path());
    wait_for_socket(&socket);

    let output = geese()
        .env("GEESED_SOCKET", &socket)
        .arg("status")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("geesed: running"));
    assert!(stdout.contains(&format!("  pid:        {}", daemon.id())));

    kill(Pid::from_raw(daemon.id() as i32), Signal::SIGTERM).unwrap();
    assert!(daemon.wait().unwrap().success());
}

#[test]
fn status_reports_missing_daemon() {
    let tempdir = tempdir().unwrap();
    let socket = socket_path(tempdir.path());

    let output = geese()
        .env("GEESED_SOCKET", &socket)
        .arg("status")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.starts_with(&format!(
        "geesed: not running (no socket at {})",
        socket.display()
    )));
}
