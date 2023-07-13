use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use thiserror::Error;

use crate::codegen::CodegenFormat;

static CONFIG_FILENAME: &str = "runway.toml";

/// Configuration for Runway, contained in a `runway.toml` file.
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// A name for this project. This is used as a prefix during local syncs.
    pub name: String,

    /// A list of targets to choose from when syncing.
    #[serde(default, rename = "target")]
    pub targets: Vec<TargetConfig>,

    /// A list of inputs that define searches for assets to sync.
    #[serde(default, rename = "input")]
    pub inputs: Vec<InputConfig>,

    /// A list of codegen outputs to generate.
    #[serde(default, rename = "codegen")]
    pub codegens: Vec<CodegenConfig>,

    /// The path that this config came from. Paths in this config
    /// should be relative to the folder containing the config file.
    #[serde(skip)]
    pub file_path: PathBuf,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TargetType {
    Local,
    Roblox,
}

impl TargetType {
    pub fn to_key(&self) -> String {
        match self {
            TargetType::Local => "local".to_string(),
            TargetType::Roblox => "roblox".to_string(),
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(from = "IntermediateTarget", deny_unknown_fields)]
pub struct TargetConfig {
    /// Unique identifier for this target used in the CLI and state files. If omitted,
    /// it defaults to the value of `target`. If the same target is used more than once,
    /// keys will need to be manually assigned (e.g. `staging` and `production`).
    ///
    /// Changing a target's `key` without manually updating the state files would cause
    /// all previous upload results to that target to be lost.
    pub key: String,

    /// The sync target type.
    pub r#type: TargetType,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IntermediateTarget {
    key: Option<String>,
    r#type: TargetType,
}

// When reading a config, default target keys to their types
impl From<IntermediateTarget> for TargetConfig {
    fn from(other: IntermediateTarget) -> Self {
        TargetConfig {
            key: other.key.unwrap_or_else(|| other.r#type.to_key()),
            r#type: other.r#type,
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct InputConfig {
    /// A glob matching files containing assets to be upload.
    ///
    /// Glob matching uses [`globwalk`](https://docs.rs/globwalk/0.8.1/globwalk/index.html)
    /// which supports [`gitignore`'s glob syntax](https://git-scm.com/docs/gitignore#_pattern_format).
    pub glob: String,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct CodegenConfig {
    /// The path for this codegen output to write to, relative to this config file.
    pub path: PathBuf,

    /// The format to generate.
    pub format: CodegenFormat,

    /// Removes file extensions from the output.
    #[serde(default = "default_strip_extension")]
    pub strip_extension: bool,

    /// Flattens the output.
    #[serde(default)]
    pub flatten: bool,
}

fn default_strip_extension() -> bool {
    true
}

impl Config {
    pub fn read_from_folder_or_file<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let metadata = fs::metadata(path)?;

        if metadata.is_file() {
            Self::read_from_file(path)
        } else {
            Self::read_from_folder(path)
        }
    }

    pub fn read_from_folder<P: AsRef<Path>>(folder_path: P) -> Result<Self, ConfigError> {
        let folder_path = folder_path.as_ref();
        let file_path = &folder_path.join(CONFIG_FILENAME);

        Self::read_from_file(file_path)
    }

    pub fn read_from_file<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let contents = fs::read(path)?;

        let mut config =
            toml::from_str::<Self>(&String::from_utf8_lossy(&contents)).map_err(|s| {
                ConfigError::Toml {
                    source: s,
                    path: path.to_owned(),
                }
            })?;

        config.file_path = path.to_owned();

        // Make codegen paths absolute
        let base_path = path.parent().unwrap();
        for codegen in config.codegens.iter_mut() {
            make_absolute(&mut codegen.path, base_path);
        }

        // Check for duplicate target keys
        let unique_keys_len = config
            .targets
            .iter()
            .map(|t| t.key.clone())
            .collect::<HashSet<String>>()
            .len();
        if unique_keys_len < config.targets.len() {
            return Err(ConfigError::DuplicateKeys);
        }

        Ok(config)
    }

    pub fn root_path(&self) -> &Path {
        &self.file_path.parent().unwrap()
    }
}

fn make_absolute(path: &mut PathBuf, base: &Path) {
    if path.is_relative() {
        let new_path = base.join(&*path);
        *path = new_path;
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Targets have duplicate keys")]
    DuplicateKeys,

    #[error("Error deserializing TOML from path {}", .path.display())]
    Toml {
        path: PathBuf,
        source: toml::de::Error,
    },

    #[error(transparent)]
    Io {
        #[from]
        source: io::Error,
    },
}
