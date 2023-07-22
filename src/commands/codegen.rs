use tokio::time::Instant;

use crate::{
    cli::CodegenOptions,
    codegen::{self, CodegenError},
    config::{Config, ConfigError},
    state::State,
};

pub async fn codegen(options: CodegenOptions) -> Result<(), CodegenError> {
    let config_path = match &options.project.config {
        Some(c) => c.to_owned(),
        None => std::env::current_dir()?,
    };
    let config = Config::read_from_folder_or_file(config_path)?;

    log::debug!("Loaded config at '{}'", config.file_path.display());

    let Some(target) = config.targets.clone().into_iter().find(|t| t.key == options.project.target) else {
		return Err(ConfigError::UnknownTarget.into());
	};

    let start_time = Instant::now();

    let state = State::read_from_config(&config)?;
    let result = codegen::generate_all(&config, &state, &target);

    let elapsed = start_time.elapsed();
    log::info!("Codegen finished in {:?}", elapsed);

    result
}
