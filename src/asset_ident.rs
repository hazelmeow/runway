use std::{
    fmt,
    path::{Path, MAIN_SEPARATOR},
    sync::Arc,
};

use serde::{Deserialize, Serialize};

/// Represents a path to an asset inside a project.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AssetIdent(Arc<str>);

impl AssetIdent {
    pub fn from_paths(root_path: &Path, asset_path: &Path) -> Self {
        let relative = asset_path
            .strip_prefix(root_path)
            .expect("AssetIdent::from_paths expects asset_path to have root_path as a prefix.");

        let displayed = format!("{}", relative.display());

        // Change the path separator to always be /
        let displayed = match MAIN_SEPARATOR {
            '/' => displayed,
            sep => displayed.replace(sep, "/"),
        };

        AssetIdent(displayed.into())
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
