//! Application-owned paths for Little-Mungbean.
use anyhow::{Context, Result};
use std::path::PathBuf;

pub fn data_root() -> Result<PathBuf> {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .context("无法确定 LOCALAPPDATA")?;
    #[cfg(not(windows))]
    let base = directories::BaseDirs::new()
        .context("Unable to locate the application data directory")?
        .data_local_dir()
        .to_path_buf();
    let root = base.join("Little-Mungbean");
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(&root)?;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(root)
}

#[cfg(target_os = "macos")]
pub fn cache_root() -> Result<PathBuf> {
    Ok(directories::BaseDirs::new()
        .context("Unable to locate the cache directory")?
        .cache_dir()
        .join("Little-Mungbean"))
}
