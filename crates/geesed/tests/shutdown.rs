use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

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

fn runtime_dir(runtime_root: &Path) -> PathBuf {
    runtime_root.join("geese")
}

fn socket_path(runtime_root: &Path) -> PathBuf {
    runtime_dir(runtime_root).join("control.sock")
}

fn lockfile_path(runtime_root: &Path) -> PathBuf {
    runtime_dir(runtime_root).join("geesed.pid")
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
fn daemon_removes_files_on_sigterm() {
    let tempdir = tempdir().unwrap();
    let socket = socket_path(tempdir.path());
    let lockfile = lockfile_path(tempdir.path());
    let mut daemon = spawn_geesed(tempdir.path());
    wait_for_socket(&socket);
    assert!(lockfile.exists());

    kill(Pid::from_raw(daemon.id() as i32), Signal::SIGTERM).unwrap();

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(status) = daemon.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "daemon did not exit within 2 seconds"
        );
        thread::sleep(Duration::from_millis(50));
    }

    assert!(!socket.exists());
    assert!(!lockfile.exists());
}
