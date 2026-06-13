use std::process;

use geesed::{RunOpts, run};
use tracing::Level;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(Level::INFO)
        .with_target(false)
        .without_time()
        .with_writer(std::io::stderr)
        .init();

    if let Err(error) = run(RunOpts::default()).await {
        eprintln!("{error}");
        process::exit(1);
    }
}
