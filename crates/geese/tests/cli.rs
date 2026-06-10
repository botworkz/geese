use std::fs;

use assert_cmd::Command;
use tempfile::tempdir;

fn geese() -> Command {
    Command::cargo_bin("geese").unwrap()
}

#[test]
fn happy_path_uses_geese_root_override() {
    let tempdir = tempdir().unwrap();
    let root = tempdir.path().join("geese-root");

    geese()
        .env("GEESE_ROOT", &root)
        .args(["new", "source"])
        .assert()
        .success();

    geese()
        .env("GEESE_ROOT", &root)
        .args(["list"])
        .assert()
        .success()
        .stdout("source\n");

    let source_path = root.join("profiles").join("source");
    fs::write(
        source_path.join("config").join("config.yaml"),
        "model = \"gpt\"\n",
    )
    .unwrap();
    fs::write(
        source_path.join("config").join("extra.toml"),
        "ignored = true\n",
    )
    .unwrap();
    fs::write(source_path.join("data").join("state.bin"), "do not copy\n").unwrap();

    geese()
        .env("GEESE_ROOT", &root)
        .args(["copy", "source", "target"])
        .assert()
        .success();

    let target_path = root.join("profiles").join("target");
    assert_eq!(
        fs::read_to_string(target_path.join("config").join("config.yaml")).unwrap(),
        "model = \"gpt\"\n"
    );
    assert!(!target_path.join("config").join("extra.toml").exists());
    assert!(!target_path.join("data").join("state.bin").exists());

    geese()
        .env("GEESE_ROOT", &root)
        .args(["lock", "target"])
        .assert()
        .success();

    geese()
        .env("GEESE_ROOT", &root)
        .args(["delete", "target"])
        .assert()
        .failure()
        .stderr("profile is locked: target\n");

    geese()
        .env("GEESE_ROOT", &root)
        .args(["unlock", "target"])
        .assert()
        .success();

    geese()
        .env("GEESE_ROOT", &root)
        .args(["delete", "target"])
        .assert()
        .success();

    geese()
        .env("GEESE_ROOT", &root)
        .args(["list"])
        .assert()
        .success()
        .stdout("source\n");
}

#[cfg(unix)]
mod launch_bin_tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use assert_cmd::Command;
    use predicates::prelude::*;
    use tempfile::tempdir;

    fn geese() -> Command {
        Command::cargo_bin("geese").unwrap()
    }

    /// Creates an executable shim script that prints its own path and GOOSE_PATH_ROOT.
    fn make_shim(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        fs::write(&path, "#!/bin/sh\necho \"$0 $GOOSE_PATH_ROOT\"\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn setup_profile(root: &std::path::Path, name: &str) {
        geese()
            .env("GEESE_ROOT", root)
            .env_remove("GEESE_GOOSE_BIN")
            .args(["new", name])
            .assert()
            .success();
    }

    #[test]
    fn bin_flag_overrides_default() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("geese-root");
        setup_profile(&root, "work");
        let shim = make_shim(dir.path(), "mygoose");

        geese()
            .env("GEESE_ROOT", &root)
            .env_remove("GEESE_GOOSE_BIN")
            .args(["launch", "--bin", shim.to_str().unwrap(), "work"])
            .assert()
            .success()
            .stdout(predicate::str::contains(shim.to_str().unwrap()))
            .stdout(predicate::str::contains(
                root.join("profiles").join("work").to_str().unwrap(),
            ));
    }

    #[test]
    fn env_var_overrides_default() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("geese-root");
        setup_profile(&root, "work");
        let shim = make_shim(dir.path(), "mygoose");

        geese()
            .env("GEESE_ROOT", &root)
            .env("GEESE_GOOSE_BIN", shim.to_str().unwrap())
            .args(["launch", "work"])
            .assert()
            .success()
            .stdout(predicate::str::contains(shim.to_str().unwrap()))
            .stdout(predicate::str::contains(
                root.join("profiles").join("work").to_str().unwrap(),
            ));
    }

    #[test]
    fn bin_flag_overrides_env_var() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("geese-root");
        setup_profile(&root, "work");
        let shim = make_shim(dir.path(), "mygoose");
        let other_shim = make_shim(dir.path(), "othergoose");

        geese()
            .env("GEESE_ROOT", &root)
            .env("GEESE_GOOSE_BIN", other_shim.to_str().unwrap())
            .args(["launch", "--bin", shim.to_str().unwrap(), "work"])
            .assert()
            .success()
            .stdout(predicate::str::contains(shim.to_str().unwrap()))
            .stdout(predicate::str::contains(
                root.join("profiles").join("work").to_str().unwrap(),
            ));
    }
}
