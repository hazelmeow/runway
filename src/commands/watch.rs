use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use ignore::overrides::{Override, OverrideBuilder};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use thiserror::Error;
use tokio::{
    sync::mpsc::{unbounded_channel, UnboundedSender},
    task::JoinHandle,
};

use crate::{
    cli::{SyncOptions, WatchOptions},
    commands,
    config::{Config, ConfigError, InputConfig},
};

use super::sync::configure_walker;
use super::SyncError;

fn descendant_matches(path: &PathBuf, overrides: Override) -> bool {
    // Check if any descendants match our glob
    configure_walker(path, overrides).build().next().is_some()
}

fn build_watcher(
    config: &Config,
    input_config: &InputConfig,
    tx: UnboundedSender<Result<(), WatchError>>,
) -> Result<RecommendedWatcher, WatchError> {
    let root = config.root_path();

    let mut builder = OverrideBuilder::new(root);
    builder.add(&input_config.glob)?;
    let glob = builder.build()?;

    let mut watcher = RecommendedWatcher::new(
        move |res: Result<notify::Event, notify::Error>| match res {
            Ok(ev) => {
                // Check if any path related to this event matches the glob
                let something_matched = ev.paths.iter().any(|event_path| {
                    if event_path.is_dir() {
                        // Check if any descendant of this path matches the glob
                        // We need this to detect changes to inputs when moving a parent folder
                        descendant_matches(event_path, glob.clone())
                    } else {
                        // Check if the event path matches the glob
                        matches!(
                            glob.matched(event_path, event_path.is_dir()),
                            ignore::Match::Whitelist(_)
                        )
                    }
                });

                // Trigger a sync if something could have changed
                if something_matched {
                    tx.send(Ok(())).unwrap();
                }
            }
            Err(e) => {
                // Forward the error
                tx.send(Err(e.into())).unwrap();
            }
        },
        notify::Config::default(),
    )?;

    // Add inputs to watcher
    for input in &config.inputs {
        let p = PathBuf::from(&input.glob);

        // Optimization to not watch the entire project with every watcher
        let prefix = get_non_pattern_prefix(&p);

        watcher.watch(&prefix, RecursiveMode::Recursive)?;
    }

    Ok(watcher)
}

type JoinResult<T> = Result<T, tokio::task::JoinError>;
async fn maybe_join_result<T>(maybe_handle: &mut Option<JoinHandle<T>>) -> Option<JoinResult<T>> {
    match maybe_handle {
        Some(h) => Some(h.await),
        None => None,
    }
}

pub async fn watch(options: WatchOptions) -> Result<(), WatchError> {
    let config_path = match &options.project.config {
        Some(c) => c.to_owned(),
        None => std::env::current_dir()?,
    };
    let config = Arc::new(Config::read_from_folder_or_file(config_path)?);

    log::debug!("Loaded config at '{}'", config.file_path.display());

    let Some(target) = config.targets.clone().into_iter().find(|t| t.key == options.project.target) else {
		return Err(ConfigError::UnknownTarget.into());
	};
    let target = Arc::new(target);

    let sync_options = Arc::new(SyncOptions {
        force: false,
        upload: options.upload.clone(),
        project: options.project.clone(),
    });

    log::info!("Starting watcher for target '{}'", target.key);

    let (notify_tx, mut notify_rx) = unbounded_channel::<Result<(), WatchError>>();
    let (debounced_tx, mut debounced_rx) = unbounded_channel::<Result<(), WatchError>>();

    // Sync once when watch mode is started
    debounced_tx.send(Ok(())).unwrap();

    // Spawn task to receive all file notifications and debounce them
    tokio::task::spawn(async move {
        // TODO: make this configurable
        let duration = tokio::time::Duration::from_millis(50);

        // Track whether we need to trigger a sync
        let mut changed = false;

        loop {
            match tokio::time::timeout(duration, notify_rx.recv()).await {
                Ok(Some(notification)) => {
                    match notification {
                        Ok(_) => {
                            // File was changed but don't trigger the sync yet
                            changed = true;
                        }
                        Err(e) => {
                            // Forward the error immediately
                            debounced_tx.send(Err(e)).expect("debounced_rx is closed");
                        }
                    }
                }
                Ok(None) => {
                    // All watchers/notify_tx's were dropped so notify_rx closed (exiting watch mode)
                    break;
                }
                Err(_) => {
                    // Nothing has changed for `duration`, sync if needed
                    if changed {
                        changed = false;
                        debounced_tx.send(Ok(())).expect("debounced_rx is closed");
                    }
                }
            };
        }
    });

    // Create a watcher for each input glob and keep them in scope
    let _watchers = config
        .inputs
        .iter()
        .map(|input_config| build_watcher(&config, input_config, notify_tx.clone()))
        .collect::<Result<Vec<RecommendedWatcher>, WatchError>>()?;

    // The join handle of the sync task if a sync is running
    let mut sync_task: Option<JoinHandle<Result<(), SyncError>>> = None;

    // If another sync is triggered while we're still syncing, sync again immediately after finishing
    let mut sync_again = false;

    // Helper
    let start_sync = || {
        let sync_options2 = sync_options.clone();
        let config2 = config.clone();
        let target2 = target.clone();
        Some(tokio::spawn(async move {
            commands::sync_with_config(&sync_options2, &config2, &target2).await
        }))
    };

    loop {
        tokio::select! {
            res = debounced_rx.recv() => {
                if let Some(notification) = res {
                    match notification {
                        Ok(_) => {
                            if sync_task.is_some() {
                                // We're already syncing
                                sync_again = true;
                            } else {
                                sync_again = false;
                                sync_task = start_sync();
                            }
                        }
                        Err(e) => {
                            log::error!("{}", e);
                        }
                    }
                } else {
                    // rx.recv() was None, the channel is closed and empty
                    break;
                }
            }
            Some(join_result) = maybe_join_result(&mut sync_task) => {
                sync_task = None;

                match join_result {
                    Ok(sync_result) => {
                        match sync_result {
                            Ok(_) => {
                                if sync_again {
                                    sync_again = false;
                                    sync_task = start_sync();
                                }
                            },
                            Err(e) => log::error!("{}", e),
                        }
                    }
                    Err(e) => log::error!("{}", e)
                }
            }
            _ = tokio::signal::ctrl_c() => {
                log::info!("Shutting down");
                debounced_rx.close();
            }
        }
    }

    Ok(())
}

const GLOB_PATTERN_CHARACTERS: &str = "*?{}[]";

fn get_non_pattern_prefix(glob_path: &Path) -> PathBuf {
    let mut prefix = PathBuf::new();

    for component in glob_path.iter() {
        let component_str = component.to_str().unwrap();

        if GLOB_PATTERN_CHARACTERS
            .chars()
            .any(|special_char| component_str.contains(special_char))
        {
            break;
        }

        prefix.push(component);
    }

    prefix
}

#[derive(Error, Debug)]
pub enum WatchError {
    #[error("Unknown target")]
    UnknownTarget,

    #[error(transparent)]
    Sync {
        #[from]
        source: SyncError,
    },

    #[error(transparent)]
    Config {
        #[from]
        source: ConfigError,
    },

    #[error(transparent)]
    Io {
        #[from]
        source: std::io::Error,
    },

    #[error(transparent)]
    Ignore {
        #[from]
        source: ignore::Error,
    },

    #[error(transparent)]
    Notify {
        #[from]
        source: notify::Error,
    },
}
