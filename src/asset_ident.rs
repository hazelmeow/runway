use std::{
    fmt,
    path::{Path, PathBuf, MAIN_SEPARATOR},
    sync::Arc,
};

use rbxcloud::rbx::assets::AssetType;
use serde::{Deserialize, Serialize};

/// Represents a path to an asset inside a project.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AssetIdent(Arc<str>);

impl AssetIdent {
    pub fn from_paths(
        root_path: &Path,
        asset_path: &Path,
    ) -> Result<Self, rbxcloud::rbx::error::Error> {
        let relative = asset_path
            .strip_prefix(root_path)
            .expect("AssetIdent::from_paths expects asset_path to have root_path as a prefix.");

        let displayed = format!("{}", relative.display());

        // Change the path separator to always be /
        let displayed = replace_slashes(displayed);

        let ident = AssetIdent(displayed.into());

        // Make sure this file maps to a valid asset type
        AssetType::try_from_extension(&ident.extension().unwrap_or_default())?;

        Ok(ident)
    }

    pub fn with_cache_bust(&self, cb: &str) -> PathBuf {
        let mut p: PathBuf = self.to_string().into();
        let mut file_name = p.file_stem().unwrap_or_default().to_owned();
        file_name.push("-");
        file_name.push(cb);
        file_name.push(".");
        file_name.push(p.extension().unwrap_or_default());
        p.set_file_name(file_name);
        p
    }

    // Used for display name in Roblox uploads
    pub fn last_component(&self) -> &str {
        self.0.split('/').last().unwrap()
    }

    pub fn extension(&self) -> Option<String> {
        let p: PathBuf = self.as_ref().into();
        p.extension().map(|e| e.to_string_lossy().to_string())
    }

    pub fn asset_type(&self) -> AssetType {
        // We can unwrap here because we already checked in new()
        AssetType::try_from_extension(&self.extension().unwrap_or_default()).unwrap()
    }
}

impl AsRef<str> for AssetIdent {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AssetIdent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

pub fn replace_slashes(s: String) -> String {
    match MAIN_SEPARATOR {
        '/' => s,
        sep => s.replace(sep, "/"),
    }
}
