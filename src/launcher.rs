use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread::sleep;
use std::time::Duration;

use nix::sys::signal::{kill, signal, SigHandler, Signal};
use nix::unistd::{setsid, Pid};
use which::which;

use crate::config::EffectiveProfile;
use crate::error::{anyhow, bail, Context, Result};
use crate::paths::ProfilePaths;

#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub foreground: bool,
    pub verbose: bool,
}

#[derive(Debug)]
pub struct LaunchResult {
    pub pid: u32,
    pub app_id: String,
}

static FORWARDED_SIGNAL: AtomicI32 = AtomicI32::new(0);

pub fn display_binary(binary: &str) -> String {
    if Path::new(binary).is_absolute() {
        return binary.to_owned();
    }

    which(binary)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| format!("{binary} (not found in $PATH)"))
}

pub fn launch_profile(
    profile: &EffectiveProfile,
    paths: &ProfilePaths,
    options: &LaunchOptions,
) -> Result<LaunchResult> {
    paths.ensure_dirs()?;
    let real_binary = resolve_binary(&profile.binary)?;
    ensure_symlink(&paths.symlink_path, &real_binary)?;

    let app_id = paths.app_id(&profile.name);
    let mut args = profile.args.clone();
    if is_x11_session() {
        args.push(format!("--class={app_id}"));
    }

    let env = build_child_env(profile, paths);
    if options.verbose {
        eprintln!(
            "resolved binary for {}: {}",
            profile.name,
            real_binary.display()
        );
        eprintln!(
            "command: {} {}",
            paths.symlink_path.display(),
            shell_join(&args)
        );
        for (key, value) in diff_env(&env) {
            eprintln!("env {key}={}", value.to_string_lossy());
        }
    }

    let mut command = Command::new(&paths.symlink_path);
    command.args(&args);
    command.env_clear();
    command.envs(&env);

    if !options.foreground {
        unsafe {
            command.pre_exec(|| {
                setsid().map_err(std::io::Error::other)?;
                Ok(())
            });
        }
    }

    let child = command.spawn().with_context(|| {
        format!(
            "failed to launch profile '{}' using {}",
            profile.name,
            paths.symlink_path.display()
        )
    })?;

    Ok(LaunchResult {
        pid: child.id(),
        app_id,
    })
}

// Children are polled with `try_wait`, which reaps them before we remove them from the vector.
// Clippy cannot see that `Some(status)` means the process is already waited on.
#[allow(clippy::zombie_processes)]
pub fn wait_for_children(children: Vec<Child>) -> Result<i32> {
    FORWARDED_SIGNAL.store(0, Ordering::SeqCst);
    let previous_sigint = unsafe { signal(Signal::SIGINT, SigHandler::Handler(record_signal)) }
        .context("failed to install SIGINT handler")?;
    let previous_sigterm = unsafe { signal(Signal::SIGTERM, SigHandler::Handler(record_signal)) }
        .context("failed to install SIGTERM handler")?;

    let mut children = children;
    let mut exit_code = 0;
    while !children.is_empty() {
        let signal = FORWARDED_SIGNAL.swap(0, Ordering::SeqCst);
        if signal != 0 {
            let signal = Signal::try_from(signal).context("received unsupported signal")?;
            for child in &children {
                let _ = kill(Pid::from_raw(child.id() as i32), signal);
            }
        }

        let mut index = 0;
        while index < children.len() {
            match children[index]
                .try_wait()
                .context("failed waiting for child process")?
            {
                Some(status) => {
                    if let Some(code) = status.code() {
                        if code != 0 {
                            exit_code = code;
                        }
                    } else {
                        exit_code = 1;
                    }
                    children.remove(index);
                }
                None => {
                    index += 1;
                }
            }
        }

        if !children.is_empty() {
            sleep(Duration::from_millis(100));
        }
    }

    unsafe {
        signal(Signal::SIGINT, previous_sigint).context("failed to restore SIGINT handler")?;
        signal(Signal::SIGTERM, previous_sigterm).context("failed to restore SIGTERM handler")?;
    }

    Ok(exit_code)
}

pub fn spawn_foreground(
    profile: &EffectiveProfile,
    paths: &ProfilePaths,
    options: &LaunchOptions,
) -> Result<(LaunchResult, Child)> {
    paths.ensure_dirs()?;
    let real_binary = resolve_binary(&profile.binary)?;
    ensure_symlink(&paths.symlink_path, &real_binary)?;

    let app_id = paths.app_id(&profile.name);
    let mut args = profile.args.clone();
    if is_x11_session() {
        args.push(format!("--class={app_id}"));
    }

    let env = build_child_env(profile, paths);
    if options.verbose {
        eprintln!(
            "resolved binary for {}: {}",
            profile.name,
            real_binary.display()
        );
        eprintln!(
            "command: {} {}",
            paths.symlink_path.display(),
            shell_join(&args)
        );
        for (key, value) in diff_env(&env) {
            eprintln!("env {key}={}", value.to_string_lossy());
        }
    }

    let mut command = Command::new(&paths.symlink_path);
    command.args(&args);
    command.env_clear();
    command.envs(&env);
    let child = command.spawn().with_context(|| {
        format!(
            "failed to launch profile '{}' using {}",
            profile.name,
            paths.symlink_path.display()
        )
    })?;

    Ok((
        LaunchResult {
            pid: child.id(),
            app_id,
        },
        child,
    ))
}

fn resolve_binary(binary: &str) -> Result<PathBuf> {
    let path = if Path::new(binary).is_absolute() {
        PathBuf::from(binary)
    } else {
        which(binary).map_err(|_| {
            anyhow!(
                "goose binary '{}' not found in $PATH; install Goose or set 'binary:' in config",
                binary
            )
        })?
    };

    let metadata = std::fs::metadata(&path)
        .with_context(|| format!("failed to inspect goose binary {}", path.display()))?;
    if !metadata.is_file() {
        bail!("goose binary {} is not a file", path.display());
    }
    if metadata.permissions().mode() & 0o111 == 0 {
        bail!("goose binary {} is not executable", path.display());
    }

    Ok(path)
}

fn ensure_symlink(link_path: &Path, target: &Path) -> Result<()> {
    if let Ok(metadata) = std::fs::symlink_metadata(link_path) {
        if metadata.file_type().is_symlink() {
            let current = std::fs::read_link(link_path)
                .with_context(|| format!("failed to read symlink {}", link_path.display()))?;
            if current == target {
                return Ok(());
            }
            std::fs::remove_file(link_path)
                .with_context(|| format!("failed to replace {}", link_path.display()))?;
        } else {
            bail!("refusing to replace non-symlink at {}", link_path.display());
        }
    }

    symlink(target, link_path).with_context(|| {
        format!(
            "failed to create symlink {} -> {}",
            link_path.display(),
            target.display()
        )
    })?;
    Ok(())
}

fn build_child_env(
    profile: &EffectiveProfile,
    paths: &ProfilePaths,
) -> BTreeMap<OsString, OsString> {
    let mut env = std::env::vars_os().collect::<BTreeMap<_, _>>();
    env.insert(
        OsString::from("XDG_CONFIG_HOME"),
        paths.xdg_config_home.clone().into(),
    );
    env.insert(
        OsString::from("XDG_DATA_HOME"),
        paths.xdg_data_home.clone().into(),
    );
    env.insert(
        OsString::from("XDG_STATE_HOME"),
        paths.xdg_state_home.clone().into(),
    );
    env.insert(
        OsString::from("XDG_CACHE_HOME"),
        paths.xdg_cache_home.clone().into(),
    );
    env.insert(
        OsString::from("GOOSE_CONFIG_DIR"),
        paths.goose_config_dir.clone().into(),
    );

    for (key, value) in &profile.env {
        env.insert(OsString::from(key), OsString::from(value));
    }

    env
}

fn diff_env(env: &BTreeMap<OsString, OsString>) -> Vec<(String, OsString)> {
    let current = std::env::vars_os().collect::<BTreeMap<_, _>>();
    env.iter()
        .filter_map(|(key, value)| {
            if current.get(key) == Some(value) {
                None
            } else {
                Some((key.to_string_lossy().into_owned(), value.clone()))
            }
        })
        .collect()
}

fn is_x11_session() -> bool {
    // `geese` only needs to distinguish Wayland from the common fallback path described in the
    // project requirements: explicit X11 sessions or environments without Wayland.
    matches!(std::env::var("XDG_SESSION_TYPE"), Ok(value) if value == "x11")
        || std::env::var_os("WAYLAND_DISPLAY").is_none()
}

fn shell_join(args: &[String]) -> String {
    args.iter()
        .map(|arg| format!("{arg:?}"))
        .collect::<Vec<_>>()
        .join(" ")
}

extern "C" fn record_signal(signal: i32) {
    FORWARDED_SIGNAL.store(signal, Ordering::SeqCst);
}
