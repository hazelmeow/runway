use std::process::ExitCode;

use clap::Parser;
use cli::{Cli, Subcommand};
use log;
use pretty_env_logger;

mod asset_ident;
mod cli;
mod commands;
mod config;
mod state;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    pretty_env_logger::formatted_builder()
        .filter_level(cli.options.verbose.log_level_filter())
        .init();

    match cli.command {
        Subcommand::Sync(args) => {
            if let Err(e) = commands::sync(args).await {
                log::error!("{}", e);
                return ExitCode::FAILURE;
            }
        }
    };

    return ExitCode::SUCCESS;
}
