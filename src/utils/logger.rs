use tracing_subscriber::{EnvFilter, fmt};
use std::fs;

pub fn init() {
    // Create logs directory
    fs::create_dir_all("logs").ok();

    // Write logs to a file so the TUI dashboard has a clean terminal
    let log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("logs/poly-apex.log")
        .expect("Failed to open log file");

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,poly_apex_copy=debug"));
        
    let subscriber = fmt::Subscriber::builder()
        .with_env_filter(filter)
        .with_target(false)
        .with_thread_ids(true)
        .with_ansi(false)          // No ANSI colors in log file
        .with_writer(log_file)     // Write to file, not stdout
        .finish();

    tracing::subscriber::set_global_default(subscriber)
        .expect("setting default subscriber failed");
}

