use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use globwalk::{DirEntry, GlobWalkerBuilder};
use rbxcloud::rbx::{
    assets::{AssetCreator, AssetGroupCreator, AssetUserCreator},
    RbxCloud,
};
use roblox_install::RobloxStudio;
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;

use crate::{
    asset_ident::{replace_slashes, AssetIdent},
    cli::SyncOptions,
    config::{Config, ConfigError, TargetConfig, TargetType},
    state::{AssetState, State, StateError, TargetState},
};

#[derive(Debug)]
struct Asset {
    /// A unique identifier for this asset in the project.
    ident: AssetIdent,

    path: PathBuf,

    contents: debug_ignore::DebugIgnore<Vec<u8>>,

    hash: String,

    targets: HashMap<String, TargetState>,
}

struct SyncSession {
    config: Config,
    target: TargetConfig,
    prev_state: State,

    force_sync: bool,

    assets: BTreeMap<AssetIdent, Asset>,

    // Errors encountered and ignored during syncing.
    errors: Vec<anyhow::Error>,
}

pub async fn sync(options: SyncOptions) -> Result<(), SyncError> {
    let config_path = match &options.config {
        Some(c) => c.to_owned(),
        None => std::env::current_dir()?,
    };
    let config = Config::read_from_folder_or_file(config_path)?;

    log::debug!("Loaded config at '{}'", config.file_path.display());

    let Some(target) = config.targets.clone().into_iter().find(|t| t.key == options.target) else {
		return Err(SyncError::UnknownTarget);
	};

    let strategy: Box<dyn SyncStrategy> = match target.r#type {
        TargetType::Local => Box::new(LocalSyncStrategy::new()?),
        TargetType::Roblox => {
            let Some(api_key) = &options.api_key else {
				return Err(SyncError::MissingApiKey);
			};

            let Some(creator) = &options.creator else {
				return Err(SyncError::MissingCreator);
			};

            let creator = if let Some(id) = &creator.user_id {
                AssetCreator::User(AssetUserCreator {
                    user_id: id.clone(),
                })
            } else if let Some(id) = &creator.group_id {
                AssetCreator::Group(AssetGroupCreator {
                    group_id: id.clone(),
                })
            } else {
                unreachable!();
            };

            Box::new(RobloxSyncStrategy {
                api_key: api_key.clone(),
                creator,
            })
        }
    };

    let mut session = SyncSession::new(options, config, target)?;

    session.find_assets()?;
    session.perform_sync(strategy)?;
    session.write_state()?;

    if session.errors.is_empty() {
        Ok(())
    } else {
        Err(SyncError::HadErrors {
            error_count: session.errors.len(),
        })
    }
}

impl SyncSession {
    fn new(options: SyncOptions, config: Config, target: TargetConfig) -> Result<Self, SyncError> {
        log::info!("Starting sync for target '{}'", target.key);

        let prev_state = match State::read_from_config(&config) {
            Ok(m) => m,
            Err(e) => {
                return Err(e.into());
            }
        };

        Ok(SyncSession {
            config,
            prev_state,
            target,
            force_sync: options.force,
            assets: BTreeMap::new(),
            errors: Vec::new(),
        })
    }

    fn raise_error(&mut self, error: impl Into<anyhow::Error>) {
        raise_error(error, &mut self.errors)
    }

    fn find_assets(&mut self) -> Result<(), SyncError> {
        let patterns = self
            .config
            .inputs
            .iter()
            .map(|f| f.glob.clone())
            .collect::<Vec<String>>();

        let walker =
            GlobWalkerBuilder::from_patterns(self.config.root_path().clone(), &patterns).build()?;

        for result in walker {
            match result {
                Ok(file) => {
                    match Self::process_entry(&self.prev_state, &self.config.root_path(), file) {
                        Ok(Some(i)) => {
                            log::trace!("Found asset '{}'", i.ident);

                            self.assets.insert(i.ident.clone(), i);
                        }
                        Ok(None) => {}
                        Err(e) => self.raise_error(e),
                    }
                }
                Err(e) => self.raise_error(e),
            }
        }

        log::debug!("Found {} assets", self.assets.len());

        Ok(())
    }

    fn process_entry(
        prev_state: &State,
        root_path: &Path,
        file: DirEntry,
    ) -> Result<Option<Asset>, SyncError> {
        if file.metadata()?.is_dir() {
            return Ok(None);
        }

        if !matches!(
            file.path()
                .extension()
                .unwrap_or_default()
                .to_str()
                .unwrap(),
            "png" | "jpg" | "jpeg" | "tga"
        ) {
            return Err(SyncError::UnsupportedFile {
                path: file.path().into(),
            });
        }

        let contents = fs::read(file.path())?;

        let ident = AssetIdent::from_paths(root_path, file.path());

        // Read previous target state from file if available
        let targets = {
            if let Some(prev) = prev_state.assets.get(&ident) {
                prev.targets.clone()
            } else {
                HashMap::new()
            }
        };

        Ok(Some(Asset {
            ident,
            path: file.path().to_path_buf(),
            hash: generate_asset_hash(&contents),
            contents: contents.into(),
            targets,
        }))
    }

    fn perform_sync(&mut self, strategy: Box<dyn SyncStrategy>) -> Result<(), SyncError> {
        let (ok_count, err_count) = strategy.perform_sync(self);
        let skip_count = self.assets.len() - ok_count - err_count;
        log::info!(
            "Sync finished with {} synced, {} failed, {} skipped",
            ok_count,
            err_count,
            skip_count,
        );
        Ok(())
    }

    fn iter_needs_sync<'a>(
        force: &'a bool,
        assets: &'a mut BTreeMap<AssetIdent, Asset>,
        prev_state: &'a State,
        target: &'a TargetConfig,
        check_local_path: &'a bool,
    ) -> Box<dyn Iterator<Item = (&'a AssetIdent, &'a mut Asset)> + 'a> {
        Box::new(assets.iter_mut().filter(|(ident, asset)| {
            if *force {
                log::trace!("Asset '{}' will sync (forced)", ident);
                return true;
            }

            if let Some(prev) = prev_state.assets.get(&ident) {
                if let Some(prev_state) = prev.targets.get(&target.key) {
                    // If the hashes differ, sync again
                    if prev_state.hash != asset.hash {
                        log::trace!("Asset '{}' has a different hash, will sync", ident);
                        true
                    } else {
						if *check_local_path {
							if let Some(local_path) = &prev_state.local_path {
								if !local_path.exists() {
									log::trace!("Asset '{}' is unchanged but last known path does not exist, will sync", ident);
									return true
								}
							} else {
								log::trace!("Asset '{}' is does not have last known path, will sync", ident);
								return true
							}
						}

                        log::trace!("Asset '{}' is unchanged, skipping", ident);
                        false
                    }
                } else {
                    // If we don't have a previous state for this target, sync
                    log::trace!("Asset '{}' is new for this target, will sync", ident);
                    true
                }
            } else {
                // This asset hasn't been uploaded before
                log::trace!("Asset '{}' is new, will sync", ident);
                true
            }
        }))
    }

    fn write_state(&self) -> Result<(), SyncError> {
        let mut state = State::default();

        state.assets = self
            .assets
            .iter()
            .map(|(ident, input)| {
                (
                    ident.clone(),
                    AssetState {
                        targets: input.targets.clone(),
                    },
                )
            })
            .collect();

        state.write_for_config(&self.config)?;

        Ok(())
    }
}

fn raise_error(error: impl Into<anyhow::Error>, errors: &mut Vec<anyhow::Error>) {
    let error = error.into();
    log::error!("{:?}", error);
    errors.push(error);
}

trait SyncStrategy {
    fn perform_sync(&self, session: &mut SyncSession) -> (usize, usize);
}

struct LocalSyncStrategy {
    content_path: PathBuf,
}
impl LocalSyncStrategy {
    fn new() -> Result<Self, SyncError> {
        RobloxStudio::locate()
            .map(|studio| LocalSyncStrategy {
                content_path: studio.content_path().into(),
            })
            .map_err(|e| e.into())
    }
}
impl SyncStrategy for LocalSyncStrategy {
    fn perform_sync(&self, session: &mut SyncSession) -> (usize, usize) {
        let target_key = session.target.key.clone();

        log::debug!("Performing local sync for target '{target_key}'");

        // Append the current system time to the filename in Studio's content folder
        // so the new image is always used.
        let system_time = SystemTime::now();
        let timestamp = system_time
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .to_string();

        let mut base_path = PathBuf::from(".runway");
        base_path.push(session.config.name.clone());

        let mut ok_count = 0;
        let mut err_count = 0;

        for (ident, asset) in SyncSession::iter_needs_sync(
            &session.force_sync,
            &mut session.assets,
            &session.prev_state,
            &session.target,
            &true,
        ) {
            let result: Result<(), SyncError> = (|| {
                let asset_path = base_path.join(ident.with_cache_bust(&timestamp));
                let full_path = self.content_path.join(&asset_path);

                log::debug!("Syncing {}", &ident);

                fs::create_dir_all(&full_path.parent().unwrap())?;
                fs::write(&full_path, &asset.contents)?;

                log::info!("Copied {} to {}", &ident, &asset_path.display());

                asset.targets.insert(
                    target_key.clone(),
                    TargetState {
                        hash: asset.hash.clone(),
                        id: format!(
                            "rbxasset://{}",
                            replace_slashes(asset_path.to_string_lossy().to_string())
                        ),
                        local_path: Some(full_path),
                    },
                );

                Ok(())
            })();

            match result {
                Ok(_) => ok_count += 1,
                Err(e) => {
                    raise_error(e, &mut session.errors);
                    err_count += 1;
                }
            }
        }

        (ok_count, err_count)
    }
}

struct RobloxSyncStrategy {
    api_key: SecretString,
    creator: AssetCreator,
}
impl SyncStrategy for RobloxSyncStrategy {
    fn perform_sync<'a>(&self, session: &mut SyncSession) -> (usize, usize) {
        let cloud = RbxCloud::new(self.api_key.expose_secret());
        let assets = cloud.assets();

        for (ident, asset) in SyncSession::iter_needs_sync(
            &session.force_sync,
            &mut session.assets,
            &session.prev_state,
            &session.target,
            &false,
        ) {}

        todo!();

        // let result = assets
        //     .create(&CreateAsset {
        //         asset: AssetCreation {
        //             asset_type: AssetType::DecalPng,
        //             display_name: "test".to_string(),
        //             description: "test123".to_string(),
        //             creation_context: AssetCreationContext {
        //                 creator: AssetCreator::User(AssetUserCreator {
        //                     user_id: user_id.to_owned(),
        //                 }),
        //                 expected_price: None,
        //             },
        //         },
        //         filepath: "./test.png".to_string(),
        //     })
        //     .await;

        // dbg!(result);

        // Ok(())
    }
}

fn generate_asset_hash(content: &[u8]) -> String {
    format!("{}", blake3::hash(content).to_hex())
}

#[derive(Error, Debug)]
pub enum SyncError {
    #[error("Unknown target")]
    UnknownTarget,

    #[error("API key is required for Roblox sync targets")]
    MissingApiKey,

    #[error("User ID or group ID is required for Roblox sync targets")]
    MissingCreator,

    #[error("Matched file at {} is not supported", .path.display())]
    UnsupportedFile { path: PathBuf },

    #[error("Sync finished with {} error(s)", .error_count)]
    HadErrors { error_count: usize },

    #[error(transparent)]
    Config {
        #[from]
        source: ConfigError,
    },

    #[error(transparent)]
    State {
        #[from]
        source: StateError,
    },

    #[error(transparent)]
    Io {
        #[from]
        source: std::io::Error,
    },

    #[error(transparent)]
    GlobError {
        #[from]
        source: globwalk::GlobError,
    },

    #[error(transparent)]
    WalkError {
        #[from]
        source: globwalk::WalkError,
    },

    #[error(transparent)]
    RobloxInstall {
        #[from]
        source: roblox_install::Error,
    },
}
