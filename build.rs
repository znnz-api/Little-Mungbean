fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rerun-if-env-changed=LITTLE_MUNGBEAN_MACOS_VERSION");
        println!("cargo:rerun-if-changed=packaging/macos/release-version.txt");
        let version = std::env::var("LITTLE_MUNGBEAN_MACOS_VERSION").unwrap_or_else(|_| {
            std::fs::read_to_string("packaging/macos/release-version.txt")
                .expect("Missing macOS release version")
                .trim()
                .to_owned()
        });
        let parts: Vec<_> = version.split('.').collect();
        assert!(
            (2..=3).contains(&parts.len())
                && parts
                    .iter()
                    .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())),
            "macOS version must have two or three numeric components"
        );
        println!("cargo:rustc-env=LITTLE_MUNGBEAN_MACOS_VERSION={version}");
    }
    // The build script runs on the host, which may differ from the target.
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-env-changed=LITTLE_MUNGBEAN_WINDOWS_VERSION");
        let version =
            std::env::var("LITTLE_MUNGBEAN_WINDOWS_VERSION").unwrap_or_else(|_| "v1.1".to_owned());
        let numeric = version
            .strip_prefix("Beta-v")
            .or_else(|| version.strip_prefix('v'))
            .unwrap_or(&version);
        let parts = numeric.split('.').collect::<Vec<_>>();
        assert!(
            (2..=3).contains(&parts.len())
                && parts
                    .iter()
                    .all(|part| !part.is_empty() && part.parse::<u16>().is_ok()),
            "Windows version must have two or three numeric components, optionally prefixed with v or Beta-v"
        );
        println!("cargo:rustc-env=LITTLE_MUNGBEAN_WINDOWS_VERSION={version}");
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/gui/logo.ico");
        resource.set("ProductName", "Little-Mungbean");
        resource.set("FileDescription", "Little-Mungbean");
        resource.set("ProductVersion", &version);
        resource.set("FileVersion", numeric);
        let number = parts
            .iter()
            .chain(std::iter::repeat(&"0"))
            .take(4)
            .fold(0u64, |value, part| {
                (value << 16) | u64::from(part.parse::<u16>().unwrap())
            });
        resource.set_version_info(winresource::VersionInfo::FILEVERSION, number);
        resource.set_version_info(winresource::VersionInfo::PRODUCTVERSION, number);
        resource
            .compile()
            .expect("无法将 assets/gui/logo.ico 写入 Windows EXE 资源");
    }
}
