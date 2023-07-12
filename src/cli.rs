use std::path::PathBuf;

use clap::{Args, Parser};
use secrecy::SecretString;

#[derive(Parser, Debug)]
#[command(version)]
#[command(propagate_version = true)]
pub struct Cli {
    #[command(flatten)]
    pub options: GlobalOptions,

    #[command(subcommand)]
    pub command: Subcommand,
}

#[derive(Args, Debug)]
pub struct GlobalOptions {
    #[command(flatten)]
    pub verbose: clap_verbosity_flag::Verbosity<clap_verbosity_flag::InfoLevel>,
}

#[derive(clap::Subcommand, Debug)]
pub enum Subcommand {
    Sync(SyncOptions),
    // Watch {},
}

#[derive(Args, Debug)]
pub struct SyncOptions {
    /// The Roblox Open Cloud API key to use. Not required for local sync targets.
    #[arg(short, long, env = "RUNWAY_API_KEY")]
    pub api_key: Option<SecretString>,

    /// The user ID or group ID to use when syncing to Roblox targets.
    #[command(flatten)]
    pub creator: Option<Creator>,

    /// A path to a config file or directory containing a `runway.toml` file.
    /// If unspecified, Runway looks for a config file in the current directory.
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// The `key` of the target to sync to.
    #[arg(short, long)]
    pub target: String,

    /// Ignore previous manifest and resync everything.
    #[arg(short, long)]
    pub force: bool,
}

#[derive(Args, Debug)]
#[group(required = true, multiple = false)]
pub struct Creator {
    #[arg(short, long, group = "creator", env = "RUNWAY_USER_ID")]
    pub user_id: Option<String>,

    #[arg(short, long, group = "creator", env = "RUNWAY_GROUP_ID")]
    pub group_id: Option<String>,
}
