use anyhow::{Context, Result, bail};
use std::fs;
use std::io::Write;
use std::path::Path;

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("目标文件没有父目录: {}", path.display()))?;
    fs::create_dir_all(parent)?;

    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("little-mungbean.tmp");
    let nonce: u64 = rand::random();
    let temp = parent.join(format!(
        ".{file_name}.little-mungbean-tmp-{}-{nonce:016x}",
        std::process::id()
    ));
    let write_result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temp)
            .with_context(|| format!("无法创建临时文件 {}", temp.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("无法写入临时文件 {}", temp.display()))?;
        file.sync_all()
            .with_context(|| format!("无法同步临时文件 {}", temp.display()))?;
        replace_file(&temp, path)
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    write_result
}

#[cfg(windows)]
fn replace_file(temp: &Path, target: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    use windows::core::PCWSTR;

    let temp_wide: Vec<u16> = temp
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let target_wide: Vec<u16> = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        MoveFileExW(
            PCWSTR(temp_wide.as_ptr()),
            PCWSTR(target_wide.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
        .with_context(|| format!("无法原子替换 {} -> {}", temp.display(), target.display()))?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(temp: &Path, target: &Path) -> Result<()> {
    fs::rename(temp, target)
        .with_context(|| format!("无法原子替换 {} -> {}", temp.display(), target.display()))
}

pub fn remove_file_if_exists(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    if path.is_dir() {
        bail!("拒绝删除目录，预期应为缓存文件: {}", path.display());
    }
    fs::remove_file(path).with_context(|| format!("无法删除缓存 {}", path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_root(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "little-mungbean-client-{label}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn atomic_write_replaces_contents_without_leaving_temp_files() {
        let root = test_root("atomic-write");
        let target = root.join("nested/config.toml");
        atomic_write(&target, b"old").unwrap();
        atomic_write(&target, b"new contents").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new contents");
        let leftovers = fs::read_dir(target.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains("little-mungbean-tmp")
            })
            .count();
        assert_eq!(leftovers, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_removal_refuses_directories() {
        let root = test_root("remove-directory");
        let directory = root.join("models_cache.json");
        fs::create_dir_all(&directory).unwrap();
        assert!(remove_file_if_exists(&directory).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
