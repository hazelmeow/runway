use std::fs;

use serde::Deserialize;
use thiserror::Error;

use crate::{
    asset_ident::AssetIdent,
    config::{CodegenConfig, Config, TargetConfig},
    state::State,
};

use self::json::generate_json;

mod json;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodegenFormat {
    Json,
    // Luau,
    // Typescript,
}

pub fn generate_all(
    config: &Config,
    state: &State,
    target: &TargetConfig,
) -> Result<(), CodegenError> {
    let mut failed = 0;

    log::info!("Generating {} outputs", config.codegens.len());

    for codegen in &config.codegens {
        match generate(&codegen, &state, &target) {
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

pub fn generate(
    config: &CodegenConfig,
    state: &State,
    target: &TargetConfig,
) -> Result<(), CodegenError> {
    log::debug!(
        "Generating {:?} output at {}",
        config.format,
        config.path.display()
    );

    let contents = match config.format {
        CodegenFormat::Json => generate_json(state, target),
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

    #[error(transparent)]
    SerdeJson {
        #[from]
        source: serde_json::Error,
    },

    #[error(transparent)]
    Io {
        #[from]
        source: std::io::Error,
    },
}
