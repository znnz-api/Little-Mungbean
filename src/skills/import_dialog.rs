use anyhow::Result;
use std::path::PathBuf;

#[cfg(windows)]
pub fn pick_import(folder: bool) -> Result<Option<PathBuf>> {
    use windows::{
        Win32::{
            System::Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemFree, CoUninitialize,
            },
            UI::Shell::{
                FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog,
                SIGDN_FILESYSPATH,
            },
        },
        core::HSTRING,
    };
    // Every picker runs on a fresh worker thread and owns its COM apartment.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        struct Apartment;
        impl Drop for Apartment {
            fn drop(&mut self) {
                unsafe {
                    CoUninitialize();
                }
            }
        }
        let _apartment = Apartment;
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        let mut options = dialog.GetOptions()? | FOS_FORCEFILESYSTEM;
        if folder {
            options |= FOS_PICKFOLDERS;
        }
        dialog.SetOptions(options)?;
        dialog.SetTitle(&HSTRING::from(if folder {
            crate::i18n::tr(
                "选择包含 SKILL.md 的目录",
                "Choose a directory containing SKILL.md",
            )
        } else {
            crate::i18n::tr("选择 Skill ZIP 文件", "Choose a Skill ZIP file")
        }))?;
        if let Err(error) = dialog.Show(None) {
            if error.code().0 as u32 == 0x800704c7 {
                return Ok(None);
            }
            return Err(error.into());
        }
        let raw = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
        let result = raw.to_string();
        CoTaskMemFree(Some(raw.0.cast()));
        Ok(Some(PathBuf::from(result?)))
    }
}

#[cfg(target_os = "macos")]
pub fn pick_import(folder: bool) -> Result<Option<PathBuf>> {
    let script = if folder {
        "try\nPOSIX path of (choose folder with prompt \"Choose a directory containing SKILL.md\")\non error number -128\nreturn \"\"\nend try"
    } else {
        "try\nPOSIX path of (choose file with prompt \"Choose a Skill ZIP file\" of type {\"zip\"})\non error number -128\nreturn \"\"\nend try"
    };
    let result = std::process::Command::new("osascript")
        .args(["-e", script])
        .output()?;
    anyhow::ensure!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let path = String::from_utf8(result.stdout)?.trim().to_owned();
    Ok((!path.is_empty()).then(|| PathBuf::from(path)))
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn pick_import(_folder: bool) -> Result<Option<PathBuf>> {
    anyhow::bail!("Paste the Skill directory or ZIP path in the path field")
}
