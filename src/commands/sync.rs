use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
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
    manifest::{AssetState, Manifest, ManifestError, TargetState},
};

#[derive(Debug)]
struct Asset {
    // A unique identifier for this asset in the project.
    ident: AssetIdent,

    path: PathBuf,

    contents: debug_ignore::DebugIgnore<Vec<u8>>,

    hash: String,

    targets: HashMap<String, TargetState>,
}

struct SyncSession {
    config: Config,
    target: TargetConfig,
    prev_manifest: Manifest,

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
            let Some(api_key) = options.api_key else {
				return Err(SyncError::MissingApiKey);
			};

            let Some(creator) = options.creator else {
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

            Box::new(RobloxSyncStrategy { api_key, creator })
        }
    };

    let mut session = SyncSession::new(config, target)?;

    session.find_assets()?;
    session.perform_sync(strategy)?;
    session.write_manifest()?;

    if session.errors.is_empty() {
        Ok(())
    } else {
        Err(SyncError::HadErrors {
            error_count: session.errors.len(),
        })
    }
}

impl SyncSession {
    fn new(config: Config, target: TargetConfig) -> Result<Self, SyncError> {
        log::info!("Starting sync for target '{}'", target.key);

        let prev_manifest = match Manifest::read_from_folder(config.root_path()) {
            Ok(m) => m,
            Err(e) => {
                if e.is_not_found() {
                    log::info!("Manifest not found, creating new");
                    Manifest::default()
                } else {
                    return Err(e.into());
                }
            }
        };

        Ok(SyncSession {
            config,
            prev_manifest,
            target,
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
                    match Self::process_entry(&self.prev_manifest, &self.config.root_path(), file) {
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
        prev_manifest: &Manifest,
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

        // Read previous state from manifest if available
        let targets = {
            if let Some(prev) = prev_manifest.assets.get(&ident) {
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
        assets: &'a mut BTreeMap<AssetIdent, Asset>,
        prev_manifest: &'a Manifest,
        target: &'a TargetConfig,
    ) -> Box<dyn Iterator<Item = (&'a AssetIdent, &'a mut Asset)> + 'a> {
        Box::new(assets.iter_mut().filter(|(ident, asset)| {
            if let Some(prev) = prev_manifest.assets.get(&ident) {
                if let Some(prev_state) = prev.targets.get(&target.key) {
                    // If the hashes differ, sync again
                    if prev_state.hash != asset.hash {
                        log::trace!("Asset '{}' has a different hash, will sync", ident);
                        true
                    } else {
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

    fn write_manifest(&self) -> Result<(), SyncError> {
        let mut manifest = Manifest::default();

        manifest.assets = self
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

        manifest.write_to_folder(self.config.root_path())?;

        log::debug!("Wrote manifest to {}", self.config.root_path().display());

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

        let mut base_path = PathBuf::from("runway");
        base_path.push(session.config.name.clone());

        let mut ok_count = 0;
        let mut err_count = 0;

        for (ident, asset) in SyncSession::iter_needs_sync(
            &mut session.assets,
            &session.prev_manifest,
            &session.target,
        ) {
            let result: Result<(), SyncError> = (|| {
                let asset_path = base_path.join(ident.to_string());
                let full_path = self.content_path.join(&asset_path);

                log::debug!("Syncing {}", &ident);

                fs::create_dir_all(&full_path.parent().unwrap())?;
                fs::write(&full_path, &asset.contents)?;

                log::info!("Copied {} to {}", &ident, &full_path.display());

                asset.targets.insert(
                    target_key.clone(),
                    TargetState {
                        hash: asset.hash.clone(),
                        id: format!(
                            "rbxasset://{}",
                            replace_slashes(asset_path.to_string_lossy().to_string())
                        ),
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
            &mut session.assets,
            &session.prev_manifest,
            &session.target,
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
    Manifest {
        #[from]
        source: ManifestError,
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
