use std::{collections::HashMap, path::PathBuf};

use crate::{asset_ident::AssetIdent, state::TargetState};

#[derive(Debug)]
pub struct Asset {
    /// A unique identifier for this asset in the project.
    pub ident: AssetIdent,
    pub path: PathBuf,
    pub contents: debug_ignore::DebugIgnore<Vec<u8>>,
    pub hash: String,
    pub targets: HashMap<String, TargetState>,
}
