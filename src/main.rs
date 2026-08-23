mod auth;
mod config;
mod mcp;
mod native;
mod oauth;
mod registry;
mod transport;

use std::env;
use std::process::ExitCode;

const STATUS_FLAG: &str = "--rust-migration-status";

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("workspace-mcp-rs: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|arg| arg == STATUS_FLAG) {
        if args.len() != 1 {
            return Err(format!(
                "{STATUS_FLAG} cannot be combined with server arguments"
            ));
        }
        print!("{}", registry::migration_status());
        return Ok(());
    }

    let config = config::Config::from_env()?;
    registry::validate_native_selection(&config.requested_native_tools)?;
    transport::run(&config, &args).await
}
