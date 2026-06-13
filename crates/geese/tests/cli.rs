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

#[test]
fn crud_round_trip() {
    let tempdir = tempdir().unwrap();
    let geese_root = tempdir.path().join("geese-root");
    let mut daemon = ProcessCommand::new(assert_cmd::cargo::cargo_bin("geesed"))
        .env("XDG_RUNTIME_DIR", tempdir.path())
        .env("GEESE_ROOT", &geese_root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    wait_for_socket(&socket_path(tempdir.path()));

    // new
    geese()
        .env("XDG_RUNTIME_DIR", tempdir.path())
        .args(["new", "foo"])
        .assert()
        .success();

    // list — should show "foo"
    let out = geese()
        .env("XDG_RUNTIME_DIR", tempdir.path())
        .arg("list")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.lines().any(|l| l == "foo"),
        "expected 'foo' in list output: {stdout}"
    );

    // path — should print an absolute path containing "foo"
    let out = geese()
        .env("XDG_RUNTIME_DIR", tempdir.path())
        .args(["path", "foo"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let path_out = String::from_utf8(out.stdout).unwrap();
    let path_out = path_out.trim();
    assert!(
        path_out.contains("foo"),
        "expected path to contain 'foo': {path_out}"
    );
    assert!(
        path_out.starts_with('/'),
        "expected absolute path: {path_out}"
    );

    // lock
    geese()
        .env("XDG_RUNTIME_DIR", tempdir.path())
        .args(["lock", "foo"])
        .assert()
        .success();

    // list — locked profile should have * prefix
    let out = geese()
        .env("XDG_RUNTIME_DIR", tempdir.path())
        .arg("list")
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.lines().any(|l| l == "*foo"),
        "expected '*foo' in list: {stdout}"
    );

    // delete while locked — should fail
    geese()
        .env("XDG_RUNTIME_DIR", tempdir.path())
        .args(["delete", "foo"])
        .assert()
        .failure();

    // unlock
    geese()
        .env("XDG_RUNTIME_DIR", tempdir.path())
        .args(["unlock", "foo"])
        .assert()
        .success();

    // copy
    geese()
        .env("XDG_RUNTIME_DIR", tempdir.path())
        .args(["copy", "foo", "bar"])
        .assert()
        .success();

    // delete foo
    geese()
        .env("XDG_RUNTIME_DIR", tempdir.path())
        .args(["delete", "foo"])
        .assert()
        .success();

    // list — only bar should remain
    let out = geese()
        .env("XDG_RUNTIME_DIR", tempdir.path())
        .arg("list")
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(!stdout.lines().any(|l| l == "foo"), "foo should be deleted");
    assert!(stdout.lines().any(|l| l == "bar"), "bar should exist");

    kill(Pid::from_raw(daemon.id() as i32), Signal::SIGTERM).unwrap();
    assert!(daemon.wait().unwrap().success());
}
