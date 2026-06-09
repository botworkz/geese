use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

use which::which;

use crate::error::{anyhow, Result};

#[derive(Clone)]
pub struct StackOptions {
    pub enabled: bool,
    pub delay_ms: u64,
    pub verbose: bool,
}

pub fn stack_after_launch(profile_count: usize, opts: &StackOptions) -> Result<()> {
    if !opts.enabled {
        return Ok(());
    }

    if profile_count < 2 {
        if opts.verbose {
            eprintln!("stack: only one profile launched; nothing to stack");
        }
        return Ok(());
    }

    if !is_wayland_session() {
        eprintln!(
            "geese: stack: not a Wayland session; skipping keystroke automation (--stack is a no-op on X11)"
        );
        return Ok(());
    }

    which("wtype").map_err(|_| {
        anyhow!(
            "--stack requires 'wtype' on $PATH (install via your distro's package manager; \
             needs the virtual-keyboard-unstable-v1 Wayland protocol which COSMIC supports)"
        )
    })?;

    if opts.verbose {
        eprintln!("stack: waiting {}ms for windows to appear…", opts.delay_ms);
    }
    sleep(Duration::from_millis(opts.delay_ms));

    for i in 0..profile_count {
        if i > 0 {
            if opts.verbose {
                eprintln!("stack: pressing Super+Tab to focus next window");
            }
            run_wtype(&["-M", "logo", "-k", "Tab", "-m", "logo"])?;
            sleep(Duration::from_millis(200));
        }
        if opts.verbose {
            eprintln!("▸ stack: pressing Super+S ({}/{})", i + 1, profile_count);
        }
        run_wtype(&["-M", "logo", "-k", "s", "-m", "logo"])?;
        sleep(Duration::from_millis(200));
    }

    Ok(())
}

fn run_wtype(args: &[&str]) -> Result<()> {
    let status = Command::new("wtype")
        .args(args)
        .status()
        .map_err(|e| anyhow!("failed to run wtype: {e}"))?;

    if !status.success() {
        return Err(anyhow!(
            "wtype {} exited with status {}",
            args.join(" "),
            status
        ));
    }
    Ok(())
}

fn is_wayland_session() -> bool {
    let has_wayland_display = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let session_type = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    has_wayland_display && (session_type.is_empty() || session_type == "wayland")
}
