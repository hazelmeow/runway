use std::process::ExitCode;

use clap::Parser;
use log;
use pretty_env_logger;

mod api;
mod asset_ident;
mod cli;
mod codegen;
mod commands;
mod config;
mod state;
mod symlink;

use crate::cli::{Cli, Subcommand};

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
        Subcommand::Watch(args) => {
            if let Err(e) = commands::watch(args).await {
                log::error!("{}", e);
                return ExitCode::FAILURE;
            }
        }
    };

    return ExitCode::SUCCESS;
}
