use serde_json::{Map, Value};

use super::CodegenError;

use crate::{config::TargetConfig, state::State};

pub fn generate_json(state: &State, target: &TargetConfig) -> Result<String, CodegenError> {
    let mut root = Value::Object(Map::new());

    state
        .assets
        .iter()
        .map(|(ident, asset)| {
            let Some(target_state) = asset.targets.get(&target.key) else {
				return Err(CodegenError::MissingAsset { ident: ident.clone() });
			};

            let ptr = jsonptr::Pointer::try_from("/".to_string() + ident.as_ref()).unwrap();
            ptr.assign(&mut root, target_state.id.clone()).unwrap();

            Ok(())
        })
        .collect::<Result<(), CodegenError>>()?;

    Ok(serde_json::to_string_pretty(&root)?)
}
