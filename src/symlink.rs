use std::{
    fs,
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::config::Config;

pub fn symlink_content_folders<P: AsRef<Path>>(
    config: &Config,
    local_path: P,
) -> Result<(), SymlinkError> {
    log::debug!(
        "Symlinking content folders to {}",
        local_path.as_ref().display()
    );

    let content_folders = get_content_folders()?;

    fs::create_dir_all(&local_path.as_ref())?;

    for content_folder in content_folders {
        let mut link_path = content_folder.join(".runway");
        link_path.push(&config.name);

        if link_path.exists() {
            log::trace!(
                "Skipping symlinking {}, already exists",
                link_path.display(),
            );
            continue;
        }

        log::trace!("Symlinking {}", link_path.display());

        fs::create_dir_all(&link_path.parent().unwrap())?;

        symlink_dir(&local_path, link_path)?;
    }

    Ok(())
}

#[cfg(target_os = "windows")]
fn get_content_folders() -> Result<Vec<PathBuf>, SymlinkError> {
    use known_folders::{get_known_folder_path, KnownFolder};

    let roblox_versions_path = get_known_folder_path(KnownFolder::LocalAppData)
        .unwrap()
        .join("Roblox")
        .join("Versions");

    Ok(std::fs::read_dir(roblox_versions_path)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.path().join("content"))
        .collect::<Vec<_>>())
}

#[cfg(target_os = "macos")]
fn get_content_folders() -> Result<Vec<PathBuf>, SymlinkError> {
    let mut root = PathBuf::from("/Applications");
    root.push("RobloxStudio.app");
    root.push("Contents");
    root.push("Resources");
    root.push("content");
    Ok(vec![root])
}

/// Creates a symlink pointing to `target` at `link`.
///
/// On Windows, this method creates an NTFS junction point instead of a symlink
/// because creating a symlink requires additional privileges.
pub fn symlink_dir<P: AsRef<Path>, Q: AsRef<Path>>(target: P, link: Q) -> Result<(), SymlinkError> {
    symlink_dir_inner(target, link)
}

#[cfg(windows)]
fn symlink_dir_inner<P: AsRef<Path>, Q: AsRef<Path>>(
    target: P,
    link: Q,
) -> Result<(), SymlinkError> {
    junction::create(target, link)?;
    Ok(())
}

#[cfg(unix)]
fn symlink_dir_inner<P: AsRef<Path>, Q: AsRef<Path>>(
    target: P,
    link: Q,
) -> Result<(), SymlinkError> {
    std::os::unix::fs::symlink(target, link)?;
    Ok(())
}

#[derive(Debug, Error)]
pub enum SymlinkError {
    #[error(transparent)]
    Io {
        #[from]
        source: std::io::Error,
    },
}
