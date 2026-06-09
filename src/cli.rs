use clap::{Parser, Subcommand};

use crate::config::{missing_config_message, Config, LoadedConfig};
use crate::error::{anyhow, bail, Result};
use crate::launcher::{
    display_binary, launch_profile, spawn_foreground, wait_for_children, LaunchOptions,
};
use crate::paths::{profile_paths, resolve_paths};
use crate::stacker::{stack_after_launch, StackOptions};

#[derive(Debug, Parser)]
#[command(author, version, about = "Launch isolated Goose desktop profiles")]
struct Cli {
    #[arg(long, global = true, help = "Alias for launch-all")]
    get_gander: bool,
    #[arg(
        short,
        long,
        global = true,
        help = "Keep geese attached and wait for children"
    )]
    foreground: bool,
    #[arg(
        short,
        long,
        global = true,
        help = "Print resolved paths, commands, and env diffs"
    )]
    verbose: bool,
    #[arg(
        short,
        long,
        global = true,
        help = "Auto-stack launched windows using Super+S after launch (COSMIC / Wayland)"
    )]
    stack: bool,
    #[arg(
        long,
        global = true,
        help = "Disable auto-stacking even when config has stack: true"
    )]
    no_stack: bool,
    #[arg(
        long,
        global = true,
        value_name = "ms",
        help = "Milliseconds to wait after last launch before sending stack keystrokes (default: 3000)"
    )]
    stack_delay: Option<u64>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Clone, Subcommand)]
enum Command {
    LaunchAll,
    Launch { name: String },
    List,
    Paths,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let paths = resolve_paths()?;

    if matches!(cli.command, Some(Command::Paths)) {
        println!("config: {}", paths.config_file.display());
        println!("data_root: {}", paths.data_root.display());
        return Ok(());
    }

    let config = match Config::load(&paths)? {
        LoadedConfig::Missing { expected_path } => {
            println!("{}", missing_config_message(&expected_path));
            return Ok(());
        }
        LoadedConfig::Loaded(config) => config,
    };

    let command = cli.command.clone().unwrap_or(Command::LaunchAll);

    match command {
        Command::LaunchAll => launch_all(&config, &paths, &cli),
        Command::Launch { name } => launch_one(&config, &paths, &cli, &name),
        Command::List => list_profiles(&config, &paths),
        Command::Paths => unreachable!(),
    }
}

fn stack_opts_from_cli(cli: &Cli, config: &Config) -> StackOptions {
    let enabled = if cli.no_stack {
        false
    } else {
        cli.stack || config.stack
    };
    let delay_ms = cli.stack_delay.unwrap_or(config.stack_delay_ms);
    StackOptions {
        enabled,
        delay_ms,
        verbose: cli.verbose,
    }
}

fn list_profiles(config: &Config, paths: &crate::paths::ResolvedPaths) -> Result<()> {
    println!("NAME\tAPP_ID\tDATA_DIR\tBINARY");
    for name in config.profile_names() {
        let effective = config.effective_profile(name)?;
        let profile_paths = profile_paths(paths, name);
        println!(
            "{}\tgoose-{}\t{}\t{}",
            name,
            name,
            profile_paths.root.display(),
            display_binary(&effective.binary)
        );
    }
    Ok(())
}

fn launch_one(
    config: &Config,
    paths: &crate::paths::ResolvedPaths,
    cli: &Cli,
    name: &str,
) -> Result<()> {
    let effective = config.effective_profile(name)?;
    let launch_paths = profile_paths(paths, name);
    let options = LaunchOptions {
        foreground: cli.foreground,
        verbose: cli.verbose,
    };

    // For a single-profile launch, stacking is always a no-op.
    if (cli.stack || config.stack) && !cli.no_stack && cli.verbose {
        eprintln!("stack: single-profile launch, nothing to stack");
    }

    if cli.foreground {
        let (result, child) = spawn_foreground(&effective, &launch_paths, &options)?;
        println!(
            "▸ launching {} (app_id={}, pid={})",
            name, result.app_id, result.pid
        );
        let exit_code = wait_for_children(vec![child])?;
        if exit_code != 0 {
            bail!("profile '{}' exited with status {}", name, exit_code);
        }
        Ok(())
    } else {
        let result = launch_profile(&effective, &launch_paths, &options)?;
        println!(
            "▸ launching {} (app_id={}, pid={})",
            name, result.app_id, result.pid
        );
        Ok(())
    }
}

fn launch_all(config: &Config, paths: &crate::paths::ResolvedPaths, cli: &Cli) -> Result<()> {
    let options = LaunchOptions {
        foreground: cli.foreground,
        verbose: cli.verbose,
    };

    let stack_opts = stack_opts_from_cli(cli, config);
    let mut errors = Vec::new();

    if cli.foreground {
        let mut children = Vec::new();
        let mut successful_count = 0usize;

        for name in config.profile_names() {
            let effective = match config.effective_profile(name) {
                Ok(value) => value,
                Err(error) => {
                    eprintln!("error: {error:#}");
                    errors.push(name.to_owned());
                    continue;
                }
            };
            let launch_paths = profile_paths(paths, name);
            match spawn_foreground(&effective, &launch_paths, &options) {
                Ok((result, child)) => {
                    println!(
                        "▸ launching {} (app_id={}, pid={})",
                        name, result.app_id, result.pid
                    );
                    children.push(child);
                    successful_count += 1;
                }
                Err(error) => {
                    eprintln!("error: failed to launch {}: {error:#}", name);
                    errors.push(name.to_owned());
                }
            }
        }

        // Run stacker in a separate thread so the main thread can block on children.
        let stack_opts_thread = stack_opts.clone();
        let stacker_handle =
            std::thread::spawn(move || stack_after_launch(successful_count, &stack_opts_thread));

        if !children.is_empty() {
            let exit_code = wait_for_children(children)?;
            if exit_code != 0 {
                errors.push(format!("child exit status {exit_code}"));
            }
        }

        if let Err(e) = stacker_handle.join().unwrap_or(Err(anyhow!(
            "stacker thread panicked unexpectedly; re-run with --verbose for more detail"
        ))) {
            eprintln!("geese: stack: {e:#}");
        }
    } else {
        let mut successful_count = 0usize;

        for name in config.profile_names() {
            let effective = match config.effective_profile(name) {
                Ok(value) => value,
                Err(error) => {
                    eprintln!("error: {error:#}");
                    errors.push(name.to_owned());
                    continue;
                }
            };
            let launch_paths = profile_paths(paths, name);
            match launch_profile(&effective, &launch_paths, &options) {
                Ok(result) => {
                    println!(
                        "▸ launching {} (app_id={}, pid={})",
                        name, result.app_id, result.pid
                    );
                    successful_count += 1;
                }
                Err(error) => {
                    eprintln!("error: failed to launch {}: {error:#}", name);
                    errors.push(name.to_owned());
                }
            }
        }

        if let Err(e) = stack_after_launch(successful_count, &stack_opts) {
            eprintln!("geese: stack: {e:#}");
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow!("failed to launch {} profile(s)", errors.len()))
    }
}
