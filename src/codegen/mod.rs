use std::{collections::HashMap, fs, path::PathBuf, str::FromStr};

use serde::Deserialize;
use thiserror::Error;

use crate::{
    asset_ident::AssetIdent,
    codegen::typescript::generate_typescript_declaration,
    config::{CodegenConfig, Config, ConfigError, TargetConfig},
    state::{State, StateError},
};

use self::json::generate_json;
use self::luau::generate_luau;
use self::typescript::generate_typescript;

mod json;
mod luau;
mod typescript;

#[derive(Debug, Clone)]
enum Value {
    Object(Object),
    Id(String),
}

#[derive(Debug, Clone, Default)]
struct Object(HashMap<String, Value>);

fn transform_ident(ident: &AssetIdent, config: &CodegenConfig) -> String {
    let mut path = PathBuf::from_str(ident.as_ref()).unwrap();
    path = if let Some(prefix) = &config.strip_prefix {
        if let Ok(t) = path.strip_prefix(prefix) {
            t.to_path_buf()
        } else {
            log::warn!(
                "Failed to strip prefix '{}' from '{}'",
                prefix.display(),
                ident
            );
            path
        }
    } else {
        path
    };
    if config.strip_extension {
        path.set_extension("");
    }
    path.to_string_lossy().to_string()
}

fn generate_tree(
    state: &State,
    config: &CodegenConfig,
    target: &TargetConfig,
) -> Result<Value, CodegenError> {
    let mut root = Value::Object(Object::default());

    for (ident, asset) in &state.assets {
        let target_state =
            asset
                .targets
                .get(&target.key)
                .ok_or_else(|| CodegenError::MissingAsset {
                    ident: ident.clone(),
                })?;

        let mut head = &mut root;

        let ident_string = transform_ident(ident, config);

        let key = if config.flatten {
            ident_string
        } else {
            let mut parts = ident_string.split('/').collect::<Vec<_>>();
            let last_part = parts.pop().ok_or_else(|| CodegenError::TreeStructure)?;

            for part in parts {
                match head {
                    Value::Object(obj) => {
                        if !obj.0.contains_key(part) {
                            obj.0
                                .insert(part.to_string(), Value::Object(Object::default()));
                        }

                        head = obj.0.get_mut(part).unwrap();
                    }
                    Value::Id(_) => return Err(CodegenError::TreeStructure),
                }
            }

            last_part.into()
        };

        match head {
            Value::Object(obj) => obj
                .0
                .insert(key.to_string(), Value::Id(target_state.id.clone())),
            Value::Id(_) => return Err(CodegenError::TreeStructure),
        };
    }

    Ok(root)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodegenFormat {
    Json,
    #[serde(alias = "lua")]
    Luau,
    #[serde(alias = "ts")]
    Typescript,
    #[serde(alias = "d.ts")]
    TypescriptDeclaration,
}

pub fn generate_all(
    config: &Config,
    state: &State,
    target: &TargetConfig,
) -> Result<(), CodegenError> {
    let mut failed = 0;

    log::info!("Generating {} outputs", config.codegens.len());

    for codegen in &config.codegens {
        match generate(state, codegen, target) {
            Ok(_) => {}
            Err(e) => {
                log::error!("{}", e);
                failed += 1;
            }
        }
    }

    if failed > 0 {
        Err(CodegenError::SomeFailed {
            failed,
            total: config.codegens.len(),
        })
    } else {
        Ok(())
    }
}

fn generate(
    state: &State,
    config: &CodegenConfig,
    target: &TargetConfig,
) -> Result<(), CodegenError> {
    log::debug!(
        "Generating {:?} output at {}",
        config.format,
        config.path.display()
    );

    let tree = generate_tree(state, config, target)?;

    let contents = match config.format {
        CodegenFormat::Json => generate_json(&tree),
        CodegenFormat::Luau => generate_luau(&tree),
        CodegenFormat::Typescript => generate_typescript(&tree),
        CodegenFormat::TypescriptDeclaration => generate_typescript_declaration(&tree),
    }?;

    fs::write(&config.path, contents)?;

    Ok(())
}

#[derive(Debug, Error)]
pub enum CodegenError {
    #[error("Codegen finished but {} of {} output(s) failed to generate", .failed, .total)]
    SomeFailed { failed: usize, total: usize },

    #[error("Asset '{}' has not been uploaded for the codegen target", .ident)]
    MissingAsset { ident: AssetIdent },

    #[error("File structure cannot be serialized")]
    TreeStructure,

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
}
