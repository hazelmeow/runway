use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use arl::RateLimiter;
use async_trait::async_trait;
use futures::{stream::FuturesUnordered, StreamExt};
use ignore::{
    overrides::{Override, OverrideBuilder},
    DirEntry, WalkBuilder,
};
use once_cell::sync::Lazy;
use rbxcloud::rbx::{
    assets::{
        AssetCreation, AssetCreationContext, AssetCreator, AssetGroupCreator, AssetType,
        AssetUserCreator,
    },
    CreateAsset, GetAsset, RbxAssets, RbxCloud,
};
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use tokio::time::Instant;

use crate::{
    api::AssetDelivery,
    asset::Asset,
    asset_ident::{replace_slashes, AssetIdent},
    cli::SyncOptions,
    codegen,
    config::{Config, ConfigError, TargetConfig, TargetType},
    preprocess::{preprocess, PreprocessError},
    state::{AssetState, State, StateError, TargetState},
    symlink::{symlink_content_folders, SymlinkError},
};

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
    let config_path = match &options.project.config {
        Some(c) => c.to_owned(),
        None => std::env::current_dir()?,
    };
    let config = Config::read_from_folder_or_file(config_path)?;

    log::debug!("Loaded config at '{}'", config.file_path.display());

    let target = config
        .targets
        .clone()
        .into_iter()
        .find(|t| t.key == options.project.target)
        .ok_or(ConfigError::UnknownTarget)?;

    sync_with_config(&options, &config, &target).await
}

pub async fn sync_with_config(
    options: &SyncOptions,
    config: &Config,
    target: &TargetConfig,
) -> Result<(), SyncError> {
    let start_time = Instant::now();

    let strategy: Box<dyn SyncStrategy + Send> = match target.r#type {
        TargetType::Local => {
            let local_path = config.root_path().join(".runway");

            symlink_content_folders(config, &local_path)?;

            Box::new(LocalSyncStrategy::new(local_path))
        }
        TargetType::Roblox => {
            let Some(api_key) = &options.upload.api_key else {
				return Err(SyncError::MissingApiKey);
			};

            let Some(creator) = &options.upload.creator else {
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

            Box::new(RobloxSyncStrategy::new(api_key, creator))
        }
    };

    let mut session = SyncSession::new(options, config, target)?;

    session.find_assets()?;
    session.perform_sync(strategy).await?;

    let state = session.write_state()?;

    if let Err(e) = codegen::generate_all(config, &state, target) {
        session.raise_error(e);
    }

    let elapsed = start_time.elapsed();
    log::info!("Sync finished in {:?}", elapsed);

    if session.errors.is_empty() {
        Ok(())
    } else {
        Err(SyncError::HadErrors {
            error_count: session.errors.len(),
        })
    }
}

pub fn configure_walker(root: &PathBuf, overrides: Override) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);

    builder
        // Only match the InputConfig's glob
        .overrides(overrides)
        // Don't check ignore files
        .parents(false)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false);

    builder
}

impl SyncSession {
    fn new(
        options: &SyncOptions,
        config: &Config,
        target: &TargetConfig,
    ) -> Result<Self, SyncError> {
        log::info!("Starting sync for target '{}'", target.key);

        let prev_state = match State::read_from_config(config) {
            Ok(m) => m,
            Err(e) => {
                return Err(e.into());
            }
        };

        Ok(SyncSession {
            // TODO: make this suck less
            config: config.clone(),
            prev_state,
            target: target.clone(),
            force_sync: options.force,
            assets: BTreeMap::new(),
            errors: Vec::new(),
        })
    }

    fn raise_error(&mut self, error: impl Into<anyhow::Error>) {
        raise_error(error, &mut self.errors)
    }

    fn find_assets(&mut self) -> Result<(), SyncError> {
        let root = self.config.root_path().to_path_buf();

        let mut builder = OverrideBuilder::new(&root);
        for input in &self.config.inputs {
            builder.add(&input.glob)?;
        }
        let overrides = builder.build()?;

        let walker = configure_walker(&root, overrides).build();

        for result in walker {
            match result {
                Ok(file) => {
                    match Self::process_entry(&self.prev_state, self.config.root_path(), file) {
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

        let ident = AssetIdent::from_paths(root_path, file.path()).map_err(|source| {
            SyncError::Unsupported {
                path: file.path().to_owned(),
                source,
            }
        })?;

        let contents = fs::read(file.path())?;

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

    async fn perform_sync(&mut self, strategy: Box<dyn SyncStrategy>) -> Result<(), SyncError> {
        let fut = strategy.perform_sync(self);
        let (ok_count, err_count) = fut.await;
        let skip_count = self.assets.len() - ok_count - err_count;
        log::info!(
            "Finished with {} synced, {} failed, {} skipped",
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
    ) -> Box<dyn Iterator<Item = (&'a AssetIdent, &'a mut Asset)> + 'a + Send> {
        Box::new(assets.iter_mut().filter(|(ident, asset)| {
            if *force {
                log::trace!("Asset '{}' will sync (forced)", ident);
                return true;
            }

            if let Some(prev) = prev_state.assets.get(ident) {
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
								log::trace!("Asset '{}' is unchanged but does not have last known path, will sync", ident);
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

    fn write_state(&self) -> Result<State, SyncError> {
        let state = State {
            assets: self
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
                .collect(),

            ..Default::default()
        };

        state.write_for_config(&self.config)?;

        Ok(state)
    }
}

fn raise_error(error: impl Into<anyhow::Error>, errors: &mut Vec<anyhow::Error>) {
    let error = error.into();
    log::error!("{:?}", error);
    errors.push(error);
}

#[async_trait]
trait SyncStrategy: Send {
    async fn perform_sync(&self, session: &mut SyncSession) -> (usize, usize);
}
struct LocalSyncStrategy {
    local_path: PathBuf,
}
impl LocalSyncStrategy {
    fn new(local_path: PathBuf) -> Self {
        LocalSyncStrategy { local_path }
    }
}
#[async_trait]
impl SyncStrategy for LocalSyncStrategy {
    async fn perform_sync(&self, session: &mut SyncSession) -> (usize, usize) {
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

        let mut base_content_path = PathBuf::from(".runway");
        base_content_path.push(session.config.name.clone());

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
                let filename = ident.with_cache_bust(&timestamp);
                let content_path = base_content_path.join(&filename);
                let local_file_path = self.local_path.join(&filename);

                log::debug!("Syncing {}", &ident);

                // Apply preprocessing
                preprocess(asset)?;

                fs::create_dir_all(local_file_path.parent().unwrap())?;
                fs::write(&local_file_path, &asset.contents)?;

                log::info!("Copied {} to {}", &ident, &content_path.display());

                asset.targets.insert(
                    target_key.clone(),
                    TargetState {
                        hash: asset.hash.clone(),
                        id: format!(
                            "rbxasset://{}",
                            replace_slashes(content_path.to_string_lossy().to_string())
                        ),
                        local_path: Some(local_file_path),
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
    assets: RbxAssets,
    creator: AssetCreator,
    asset_delivery: Lazy<AssetDelivery>,
}
impl RobloxSyncStrategy {
    fn new(api_key: &SecretString, creator: AssetCreator) -> Self {
        let cloud = RbxCloud::new(api_key.expose_secret());
        let assets = cloud.assets();
        let asset_delivery: Lazy<AssetDelivery> = Lazy::new(AssetDelivery::new);

        Self {
            assets,
            creator,
            asset_delivery,
        }
    }
}
#[async_trait]
impl SyncStrategy for RobloxSyncStrategy {
    async fn perform_sync(&self, session: &mut SyncSession) -> (usize, usize) {
        let target_key = Arc::new(session.target.key.clone());

        log::debug!("Performing Roblox sync for target '{target_key}'");

        let mut ok_count = 0;
        let mut err_count = 0;

        let max_create_failures = 3;
        let max_get_failures = 3;
        let max_textureid_failures = 3;

        let create_ratelimit = Arc::new(RateLimiter::new(60, Duration::from_secs(60)));
        let get_ratelimit = Arc::new(RateLimiter::new(60, Duration::from_secs(60)));

        let mut futures: FuturesUnordered<_> = SyncSession::iter_needs_sync(
            &session.force_sync,
            &mut session.assets,
            &session.prev_state,
            &session.target,
            &false,
        )
        .map(|(ident, asset)| {
            let create_ratelimit = create_ratelimit.clone();
            let get_ratelimit = get_ratelimit.clone();
            let target_key = target_key.clone();

            // Map the needs_sync iterator to a collection of futures
            async move {
				// Apply preprocessing
				preprocess(asset)?;

				// Loop until we've had too many errors
                for create_idx in 0..max_create_failures {
                    // If we're retrying, wait a bit first
                    if create_idx > 0 {
                        tokio::time::sleep(Duration::from_secs(3)).await;
                    }

                    log::debug!("CreateAsset {}: starting attempt {}", ident, create_idx + 1);

                    match roblox_create_asset(self, ident, asset, create_ratelimit.clone()).await {
                        Ok(operation_id) => {
                            log::trace!("CreateAsset {ident}: returned operation {operation_id}");

                            let operation_id = Arc::new(operation_id);

                            let mut get_idx = 0;
                            let mut get_failures = 0;

                            // Loop until the asset finishes with an ID or we fail too much
                            loop {
                                get_idx += 1;

                                let wait = 2_u64.pow(get_idx);

                                log::debug!(
                                    "GetAsset {}: starting attempt {} in {}s",
                                    ident,
                                    get_idx,
                                    wait,
                                );

                                tokio::time::sleep(Duration::from_secs(wait)).await;

                                match roblox_get_asset(
                                    self,
                                    ident,
                                    operation_id.clone(),
                                    get_ratelimit.clone(),
                                )
                                .await
                                {
                                    Ok(asset_id) => {
                                        let mut final_id = asset_id;

                                        if matches!(
                                            asset.ident.asset_type(),
                                            AssetType::DecalBmp
                                                | AssetType::DecalPng
                                                | AssetType::DecalJpeg
                                                | AssetType::DecalTga
                                        ) {
											log::debug!("Uploaded {} as rbxassetid://{}, mapping to texture ID", &ident, &final_id);

											let image_id = get_texture_with_retry(max_textureid_failures, &self.asset_delivery, &final_id).await?;

                                            final_id = image_id;
                                        }

										log::info!(
                                            "Uploaded {} as rbxassetid://{}",
                                            ident,
                                            final_id
                                        );

                                        asset.targets.insert(
                                            target_key.to_string(),
                                            TargetState {
                                                hash: asset.hash.clone(),
                                                id: format!("rbxassetid://{}", final_id),
                                                local_path: None,
                                            },
                                        );

                                        return Ok(());
                                    }
                                    Err(e) => {
                                        // Don't consider unfinished uploads to be errors
                                        if matches!(e, SyncError::UploadNotDone) {
                                            log::trace!("GetAsset {}: not done yet", ident);
                                        } else {
                                            log::error!("GetAsset {}: error: {}", ident, e);

                                            get_failures += 1;

                                            // API failed too many times, give up
                                            if get_failures >= max_get_failures {
                                                log::error!(
                                                    "GetAsset {}: failed too many times",
                                                    ident
                                                );
                                                return Err(SyncError::UploadFailed);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            log::error!("CreateAsset {}: error: {}", ident, e);
                        }
                    }
                }

                log::error!("CreateAsset {}: failed too many times", &ident);
                Err(SyncError::UploadFailed)
            }
        })
        .collect();

        // Wait for all futures to finish and log errors
        while let Some(result) = futures.next().await {
            match result {
                Ok(()) => {
                    ok_count += 1;
                }
                Err(e) => {
                    raise_error(e, &mut session.errors);
                    err_count += 1;
                }
            }
        }

        (ok_count, err_count)
    }
}
async fn roblox_create_asset(
    strategy: &RobloxSyncStrategy,
    ident: &AssetIdent,
    asset: &Asset,
    create_ratelimit: Arc<RateLimiter>,
) -> Result<String, SyncError> {
    create_ratelimit.wait().await;

    log::trace!("CreateAsset {ident}: sending request");

    let result = strategy
        .assets
        .create(&CreateAsset {
            asset: AssetCreation {
                asset_type: ident.asset_type(),
                display_name: ident.last_component().to_string(),
                description: "Uploaded by Runway.".to_string(),
                creation_context: AssetCreationContext {
                    creator: strategy.creator.clone(),
                    expected_price: Some(0),
                },
            },
            filepath: asset.path.to_string_lossy().to_string(),
        })
        .await?;

    let operation_path = result.path.ok_or_else(|| SyncError::RobloxApi)?;

    let operation_id = operation_path
        .strip_prefix("operations/")
        .expect("Roblox API returned unexpected value");

    let operation_id = operation_id.to_string();

    Ok(operation_id)
}
async fn roblox_get_asset(
    strategy: &RobloxSyncStrategy,
    ident: &AssetIdent,
    operation_id: Arc<String>,
    get_ratelimit: Arc<RateLimiter>,
) -> Result<String, SyncError> {
    get_ratelimit.wait().await;

    log::trace!("GetAsset {ident}: sending request");

    let response = strategy
        .assets
        .get(&GetAsset {
            operation_id: operation_id.to_string(),
        })
        .await?;

    // TODO: rbxcloud doesn't wrap AssetGetOperation.response in Option
    // If/when https://github.com/Sleitnick/rbxcloud/pull/32 is merged,
    // this code should handle errors properly

    // if let Some(r) = &response.response {
    //     Ok(r.asset_id.clone())
    // } else {
    //     let done = response.done.unwrap_or(false);

    //     if !done {
    //         Err(SyncError::UploadNotDone)
    //     } else {
    //         log::warn!("GetAsset {ident}: unexpected response: {:#?}", response);
    //         Err(SyncError::UploadFailed)
    //     }
    // }

    Ok(response.response.asset_id)
}
async fn get_texture_with_retry(
    max_textureid_failures: usize,
    asset_delivery: &AssetDelivery,
    asset_id: &String,
) -> Result<String, SyncError> {
    for _ in 0..max_textureid_failures {
        match asset_delivery.get_texture(asset_id).await {
            Ok(image_id) => return Ok(image_id),
            Err(e) => {
                log::error!("Error mapping decal ID to texture ID: {}", e)
            }
        }
    }
    // Failed all attempts
    Err(SyncError::RobloxApi)
}

fn generate_asset_hash(content: &[u8]) -> String {
    format!("{}", blake3::hash(content).to_hex())
}

#[derive(Error, Debug)]
pub enum SyncError {
    #[error("API key is required for Roblox sync targets")]
    MissingApiKey,

    #[error("User ID or group ID is required for Roblox sync targets")]
    MissingCreator,

    #[error("Matched file at {} is not supported", .path.display())]
    Unsupported {
        path: PathBuf,
        source: rbxcloud::rbx::error::Error,
    },

    #[error("Failed to upload file")]
    UploadFailed,

    #[error("Upload not finished")]
    UploadNotDone,

    #[error("Sync finished with {} error(s)", .error_count)]
    HadErrors { error_count: usize },

    #[error(transparent)]
    Preprocess {
        #[from]
        source: PreprocessError,
    },

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
    SymlinkError {
        #[from]
        source: SymlinkError,
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
    RbxCloud {
        #[from]
        source: rbxcloud::rbx::error::Error,
    },

    #[error("Roblox API error")]
    RobloxApi,
}
