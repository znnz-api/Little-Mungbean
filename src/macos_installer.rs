//! macOS beta installation uses official sources and a per-user destination.
use crate::install_policy::{
    CONNECT_TIMEOUT, DOWNLOAD_IDLE_TIMEOUT, MIRROR_INSTALL_TIMEOUT, OFFICIAL_INSTALL_TIMEOUT,
    SOURCE_PROBE_TIMEOUT,
};
use crate::{gui_worker::ClientTarget, i18n, platform};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::process::Command;

const NPM_OFFICIAL_REGISTRY: &str = "https://registry.npmjs.org";
const NPM_MIRROR_REGISTRY: &str = "https://registry.npmmirror.com";
const NODE_OFFICIAL_INDEX: &str = "https://nodejs.org/dist/index.json";
const NODE_MIRROR_INDEX: &str = "https://npmmirror.com/mirrors/node/index.json";
const NODE_OFFICIAL_BASE: &str = "https://nodejs.org/dist";
const NODE_MIRROR_BASE: &str = "https://npmmirror.com/mirrors/node";
const NODE_ARCHIVE_LIMIT: usize = 96 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct NodeRelease {
    version: String,
    lts: serde_json::Value,
    #[serde(default)]
    files: Vec<String>,
}

fn node_archive_platform(architecture: &str, files: &[String]) -> bool {
    // index.json uses osx-*-tar; the downloadable file uses darwin-*.
    let legacy = format!("darwin-{architecture}");
    let current = format!("osx-{architecture}-tar");
    files.is_empty() || files.iter().any(|file| file == &current || file == &legacy)
}

fn select_node_lts<'a>(releases: &'a [NodeRelease], architecture: &str) -> Option<&'a NodeRelease> {
    releases.iter().find(|release| {
        !release.lts.is_null()
            && release.lts != serde_json::Value::Bool(false)
            && node_archive_platform(architecture, &release.files)
    })
}

struct NodeRuntime {
    node: PathBuf,
    npm: PathBuf,
}

pub async fn install(target: ClientTarget) -> Result<()> {
    let installed = match target {
        ClientTarget::CodexCli => platform::find_executable("codex").is_some(),
        ClientTarget::ClaudeCode => platform::find_executable("claude").is_some(),
        ClientTarget::CodexDesktop => platform::store_codex_installed(),
        ClientTarget::ClaudeDesktop => platform::store_claude_installed(),
    };
    if installed {
        if matches!(target, ClientTarget::CodexCli | ClientTarget::ClaudeCode) {
            configure_terminal_path_for_runtime()?;
        }
        println!(
            "{} • {}",
            target.title(),
            i18n::tr("已安装", "Already installed")
        );
        return Ok(());
    }
    match target {
        ClientTarget::CodexCli => install_npm_client(target, "@openai/codex").await?,
        ClientTarget::ClaudeCode => install_npm_client(target, "@anthropic-ai/claude-code").await?,
        ClientTarget::CodexDesktop | ClientTarget::ClaudeDesktop => {
            let url = if target == ClientTarget::CodexDesktop {
                "https://developers.openai.com/codex/app"
            } else {
                "https://claude.ai/download"
            };
            platform::open_external_url(url)?;
            if i18n::language() == i18n::Language::ZhCn {
                bail!("{} • 自动安装仅支持 Windows 平台", target.title());
            } else {
                bail!(
                    "{} • Automatic installation is supported on Windows only",
                    target.title()
                );
            }
        }
    }
    let command = if target == ClientTarget::CodexCli {
        "codex"
    } else {
        "claude"
    };
    if platform::find_executable(command).is_none() {
        bail!("Installation finished, but the executable was not found")
    }
    println!(
        "{} • {}",
        i18n::tr("安装完成", "Installation complete"),
        target.title()
    );
    Ok(())
}

async fn install_npm_client(target: ClientTarget, package: &str) -> Result<()> {
    let runtime = match (
        platform::find_executable("npm"),
        platform::find_executable("node"),
    ) {
        (Some(npm), Some(node)) => NodeRuntime { node, npm },
        _ if find_managed_node_runtime().is_some() => find_managed_node_runtime().unwrap(),
        _ => {
            println!(
                "{}",
                i18n::tr(
                    "未检测到 Node.js/npm，正在安装用户级 Node.js LTS……",
                    "Node.js/npm was not detected; installing a per-user Node.js LTS runtime…"
                )
            );
            install_node_lts().await?
        }
    };
    let home = directories::UserDirs::new().context("Unable to locate user home")?;
    let prefix = home.home_dir().join(".local");
    let mut errors = Vec::new();
    for (label, registry, install_timeout) in [
        (
            i18n::tr("npm 官方源", "official npm registry"),
            NPM_OFFICIAL_REGISTRY,
            OFFICIAL_INSTALL_TIMEOUT,
        ),
        (
            i18n::tr("npmmirror 国内镜像", "npmmirror registry"),
            NPM_MIRROR_REGISTRY,
            MIRROR_INSTALL_TIMEOUT,
        ),
    ] {
        if let Err(error) = probe_npm_registry(registry).await {
            println!("{label}: {}", i18n::runtime_error(&error));
            errors.push(format!("{label}: {error:#}"));
            continue;
        }
        println!(
            "{} {package}，{} {label}…",
            i18n::tr("正在安装", "Installing"),
            i18n::tr("来源", "source")
        );
        match run_npm_install(&runtime, &prefix, package, registry, install_timeout).await {
            Ok(()) => {
                configure_terminal_path(&runtime)?;
                return Ok(());
            }
            Err(error) => {
                println!(
                    "{label} {}: {}",
                    i18n::tr("安装失败，将尝试下一来源", "failed; trying the next source"),
                    i18n::runtime_error(&error)
                );
                errors.push(format!("{label}: {error:#}"));
            }
        }
    }
    bail!("{} 安装失败：{}", target.title(), errors.join("；"))
}

fn configure_terminal_path_for_runtime() -> Result<()> {
    if let Some(node) = platform::find_executable("node") {
        let npm = platform::find_executable("npm").context("npm executable was not found")?;
        configure_terminal_path(&NodeRuntime { node, npm })?;
    }
    Ok(())
}

fn configure_terminal_path(runtime: &NodeRuntime) -> Result<()> {
    let home = directories::UserDirs::new().context("Unable to locate user home")?;
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/zsh".into());
    let zsh_root = std::env::var_os("ZDOTDIR").map(PathBuf::from);
    configure_terminal_path_at(
        home.home_dir(),
        Path::new(&shell),
        zsh_root.as_deref(),
        runtime,
    )?;
    println!(
        "{}",
        i18n::tr(
            "终端命令路径已配置；请新开终端窗口后运行 codex 或 claude。",
            "Terminal command paths configured. Open a new terminal window to run codex or claude."
        )
    );
    Ok(())
}

fn quote_shell_path(path: &Path) -> Result<String> {
    let text = path
        .to_str()
        .context("Terminal configuration path must be UTF-8")?;
    if text.contains(['\n', '\r']) {
        bail!("Terminal configuration path contains a newline");
    }
    Ok(format!("'{}'", text.replace('\'', "'\\''")))
}

fn configure_terminal_path_at(
    home: &Path,
    shell: &Path,
    zsh_root: Option<&Path>,
    runtime: &NodeRuntime,
) -> Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let profiles = match shell.file_name().and_then(|name| name.to_str()) {
        Some("zsh") => vec![zsh_root.unwrap_or(home).join(".zshrc")],
        Some("bash") => {
            let login = [".bash_profile", ".bash_login", ".profile"]
                .into_iter()
                .map(|name| home.join(name))
                .find(|path| path.exists())
                .unwrap_or_else(|| home.join(".bash_profile"));
            vec![login, home.join(".bashrc")]
        }
        _ => bail!("Automatic terminal PATH setup currently supports zsh and bash"),
    };
    let env_path = home.join(".local/share/Little-Mungbean/terminal-path.sh");
    let mut content = String::from("# Managed by Little-Mungbean: terminal command paths.\n");
    // Prepending in this order leaves the installed CLI directory first.
    let mut dirs = vec![
        runtime
            .node
            .parent()
            .context("Node.js executable has no parent")?
            .to_path_buf(),
        runtime
            .npm
            .parent()
            .context("npm executable has no parent")?
            .to_path_buf(),
        home.join(".local/bin"),
    ];
    dirs.dedup();
    for dir in dirs {
        let quoted = quote_shell_path(&dir)?;
        content.push_str(&format!("case \":${{PATH}}:\" in\n  *:{quoted}:*) ;;\n  *) export PATH={quoted}:\"${{PATH}}\" ;;\nesac\n"));
    }
    // Match the --prefix used for installation so ordinary npm -g commands
    // manage the same packages. Do not rewrite the user's .npmrc.
    content.push_str(&format!(
        "export npm_config_prefix={}\n",
        quote_shell_path(&home.join(".local"))?
    ));
    crate::backup::atomic_write(&env_path, content.as_bytes())?;
    let reference = format!("[ -r {0} ] && . {0}", quote_shell_path(&env_path)?);
    for profile in profiles {
        let original = match fs::read_to_string(&profile) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error).context("Cannot read terminal startup configuration"),
        };
        if original.lines().any(|line| line == reference) {
            continue;
        }
        if let Some(parent) = profile.parent() {
            fs::create_dir_all(parent)?;
        }
        // Append rather than replace: preserve existing content, modes and symlinks.
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&profile)
            .context("Cannot update terminal startup configuration")?;
        if !original.is_empty() && !original.ends_with('\n') {
            file.write_all(b"\n")?;
        }
        writeln!(
            file,
            "\n# Little-Mungbean terminal command paths\n{reference}"
        )?;
        file.sync_all()?;
    }
    Ok(())
}

fn find_managed_node_runtime() -> Option<NodeRuntime> {
    let home = directories::UserDirs::new()?;
    let root = home.home_dir().join(".local/share/Little-Mungbean/node");
    let mut candidates = fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.into_iter().rev().find_map(|directory| {
        let node = directory.join("bin/node");
        let npm = directory.join("bin/npm");
        (node.is_file() && npm.is_file()).then_some(NodeRuntime { node, npm })
    })
}

async fn run_npm_install(
    runtime: &NodeRuntime,
    prefix: &Path,
    package: &str,
    registry: &str,
    install_timeout: Duration,
) -> Result<()> {
    let mut command = Command::new(&runtime.npm);
    let registry_argument = format!("--registry={registry}");
    command
        .args(["install", "--global", "--prefix"])
        .arg(prefix)
        .args([
            package,
            registry_argument.as_str(),
            "--no-audit",
            "--no-fund",
        ]);
    if package == "@anthropic-ai/claude-code" {
        command.arg("--allow-scripts=@anthropic-ai/claude-code");
    }
    command.env("PATH", npm_runtime_path(runtime)?);
    command.envs(crate::install_policy::npm_network_environment());
    for (name, value) in crate::network_proxy::child_proxy_environment(registry)? {
        command.env(name, value);
    }
    sanitize(&mut command);
    run(&mut command, install_timeout).await
}

fn npm_runtime_path(runtime: &NodeRuntime) -> Result<std::ffi::OsString> {
    let mut paths = vec![
        runtime
            .node
            .parent()
            .context("Node.js executable has no parent directory")?
            .to_path_buf(),
        runtime
            .npm
            .parent()
            .context("npm executable has no parent directory")?
            .to_path_buf(),
    ];
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path));
    }
    std::env::join_paths(paths).context("Unable to construct npm PATH")
}

async fn install_node_lts() -> Result<NodeRuntime> {
    let home = directories::UserDirs::new().context("Unable to locate user home")?;
    let root = home.home_dir().join(".local/share/Little-Mungbean/node");
    fs::create_dir_all(&root)?;
    let architecture = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => bail!("Unsupported macOS architecture: {other}"),
    };
    let mut errors = Vec::new();
    for (label, index_url, base_url, download_timeout) in [
        (
            i18n::tr("Node.js 官方源", "official Node.js source"),
            NODE_OFFICIAL_INDEX,
            NODE_OFFICIAL_BASE,
            OFFICIAL_INSTALL_TIMEOUT,
        ),
        (
            i18n::tr("npmmirror Node 镜像", "npmmirror Node.js source"),
            NODE_MIRROR_INDEX,
            NODE_MIRROR_BASE,
            MIRROR_INSTALL_TIMEOUT,
        ),
    ] {
        let attempt = async {
            let index_path = root.join(format!("index-{}.json", std::process::id()));
            download(
                index_url,
                &index_path,
                8 * 1024 * 1024,
                SOURCE_PROBE_TIMEOUT,
            )
            .await?;
            let releases: Vec<NodeRelease> = serde_json::from_slice(&fs::read(&index_path)?)?;
            let _ = fs::remove_file(&index_path);
            let release = select_node_lts(&releases, architecture)
                .context("版本目录中没有适用于当前架构的 Node.js LTS")?;
            let version = release.version.trim_start_matches('v');
            if version.is_empty()
                || !version
                    .chars()
                    .all(|character| character.is_ascii_digit() || character == '.')
            {
                bail!("Node 版本号格式异常: {}", release.version);
            }
            let archive_name = format!("node-v{version}-darwin-{architecture}.tar.gz");
            let archive = root.join(&archive_name);
            download(
                &format!("{base_url}/v{version}/{archive_name}"),
                &archive,
                NODE_ARCHIVE_LIMIT,
                download_timeout,
            )
            .await?;
            let extract_root = root.join(format!("extract-{}", std::process::id()));
            let _ = fs::remove_dir_all(&extract_root);
            fs::create_dir_all(&extract_root)?;
            let mut tar = Command::new("/usr/bin/tar");
            tar.args(["-xzf"])
                .arg(&archive)
                .args(["-C"])
                .arg(&extract_root);
            sanitize(&mut tar);
            run(&mut tar, OFFICIAL_INSTALL_TIMEOUT).await?;
            let extracted = fs::read_dir(&extract_root)?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .find(|path| path.is_dir())
                .context("Node.js archive did not contain a directory")?;
            let destination = root.join(format!("v{version}-darwin-{architecture}"));
            let _ = fs::remove_dir_all(&destination);
            fs::rename(extracted, &destination)?;
            let _ = fs::remove_dir_all(&extract_root);
            let _ = fs::remove_file(&archive);
            let node = destination.join("bin/node");
            let npm = destination.join("bin/npm");
            if !node.is_file() || !npm.is_file() {
                bail!("Node.js archive is missing node or npm");
            }
            Ok::<NodeRuntime, anyhow::Error>(NodeRuntime { node, npm })
        }
        .await;
        match attempt {
            Ok(runtime) => return Ok(runtime),
            Err(error) => {
                println!(
                    "{label} {}: {}",
                    i18n::tr("安装失败，将尝试下一来源", "failed; trying the next source"),
                    i18n::runtime_error(&error)
                );
                errors.push(format!("{label}: {error:#}"));
            }
        }
    }
    bail!("Node.js 自动安装失败：{}", errors.join("；"))
}

fn sanitize(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // Keep this install attempt in its own group so timeout cleanup also
    // stops npm's postinstall children before a mirror attempt starts.
    command.as_std_mut().process_group(0);
    for name in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "LITTLE_MUNGBEAN_API_KEY",
        "LITEAPI_API_KEY",
    ] {
        command.env_remove(name);
    }
    command.kill_on_drop(true);
}
async fn run(command: &mut Command, limit: Duration) -> Result<()> {
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = command.spawn()?;
    let pid = child.id().context("Installer has no process ID")?;
    let output_future = child.wait_with_output();
    tokio::pin!(output_future);
    let output = match tokio::time::timeout(limit, &mut output_future).await {
        Ok(result) => result?,
        Err(_) => {
            // Only the process group created for this installation is killed.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
            let _ = tokio::time::timeout(Duration::from_secs(5), &mut output_future).await;
            bail!(
                "{} ({}s)",
                i18n::tr(
                    "安装超时，已停止本次安装",
                    "Installation timed out; this attempt was stopped"
                ),
                limit.as_secs()
            );
        }
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for line in text.lines() {
        println!("{line}");
    }
    if !output.status.success() {
        bail!("Installer exited with {}", output.status)
    }
    Ok(())
}
async fn probe_npm_registry(registry: &str) -> Result<()> {
    let url = format!("{registry}/-/ping");
    let client = crate::network_proxy::Client::new(|| {
        reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(SOURCE_PROBE_TIMEOUT)
    })?
    .total_timeout(SOURCE_PROBE_TIMEOUT);
    client.get(url).send().await?.error_for_status()?;
    Ok(())
}

async fn download(url: &str, path: &Path, limit: usize, total_timeout: Duration) -> Result<()> {
    let client = crate::network_proxy::Client::new(move || {
        reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(DOWNLOAD_IDLE_TIMEOUT)
            .timeout(total_timeout)
    })?
    .total_timeout(total_timeout);
    let response = client.get(url).send().await?.error_for_status()?;
    if response.url().scheme() != "https"
        || response.content_length().is_some_and(|n| n > limit as u64)
    {
        bail!("Invalid installer download")
    }
    use futures_util::StreamExt;
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if bytes.len() + chunk.len() > limit {
            bail!("Installer download exceeds the allowed size")
        }
        bytes.extend_from_slice(&chunk);
    }
    crate::backup::atomic_write(path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn installation_timeout_stops_postinstall_children_before_fallback() {
        let mut command = Command::new("/bin/sh");
        // A child holds the stdout/stderr pipes open. Killing only the parent
        // would leave wait_with_output blocked until the 20-second sleep ends.
        command.args(["-c", "sleep 20 & wait"]);
        sanitize(&mut command);
        let started = std::time::Instant::now();
        let error = run(&mut command, Duration::from_millis(100))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("安装超时"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn failed_installation_is_returned_for_source_fallback() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 17"]);
        sanitize(&mut command);
        let error = run(&mut command, Duration::from_secs(2)).await.unwrap_err();
        assert!(error.to_string().contains("17"));
    }

    #[test]
    fn node_lts_accepts_current_and_legacy_macos_architecture_names() {
        let current = vec!["linux-x64".into(), "osx-arm64-tar".into()];
        let legacy = vec!["darwin-arm64".into()];
        let wrong = vec!["osx-x64-tar".into()];
        assert!(node_archive_platform("arm64", &current));
        assert!(node_archive_platform("arm64", &legacy));
        assert!(!node_archive_platform("arm64", &wrong));
        assert!(node_archive_platform("arm64", &[]));
    }

    #[test]
    fn node_index_selects_lts_with_a_matching_macos_tarball() {
        let releases: Vec<NodeRelease> = serde_json::from_str(r#"[
            {"version":"v26.0.0","lts":false,"files":["osx-arm64-tar","osx-x64-tar"]},
            {"version":"v24.21.0","lts":"Krypton","files":["linux-x64","osx-arm64-tar","osx-x64-pkg","osx-x64-tar"]},
            {"version":"v22.0.0","lts":"Jod","files":["darwin-arm64","darwin-x64"]}
        ]"#).unwrap();
        for architecture in ["arm64", "x64"] {
            assert_eq!(
                select_node_lts(&releases, architecture).unwrap().version,
                "v24.21.0"
            );
        }
        let unsupported: Vec<NodeRelease> = serde_json::from_str(
            r#"[
            {"version":"v24.21.0","lts":"Krypton","files":["linux-arm64","osx-x64-pkg"]}
        ]"#,
        )
        .unwrap();
        assert!(select_node_lts(&unsupported, "arm64").is_none());
        assert!(
            select_node_lts(&unsupported, "x64").is_none(),
            "A pkg entry does not promise a tarball"
        );
    }

    #[test]
    fn terminal_path_setup_preserves_profiles_and_updates_without_duplicates() {
        use std::os::unix::fs::PermissionsExt;
        let home = std::env::temp_dir().join(format!(
            "little-mungbean-path-test-{}",
            rand::random::<u64>()
        ));
        let home = home.join("home space ' quote");
        fs::create_dir_all(&home).unwrap();
        let profile = home.join(".zshrc");
        let original = "# User configuration\nexport ORIGINAL_SETTING=kept";
        fs::write(&profile, original).unwrap();
        fs::set_permissions(&profile, fs::Permissions::from_mode(0o640)).unwrap();
        let runtime = NodeRuntime {
            node: home.join("node v1/bin/node"),
            npm: home.join("node v1/bin/npm"),
        };
        configure_terminal_path_at(&home, Path::new("/bin/zsh"), None, &runtime).unwrap();
        let first = fs::read_to_string(&profile).unwrap();
        assert!(first.starts_with(original));
        assert_eq!(
            fs::metadata(&profile).unwrap().permissions().mode() & 0o777,
            0o640
        );
        configure_terminal_path_at(&home, Path::new("/bin/zsh"), None, &runtime).unwrap();
        assert_eq!(fs::read_to_string(&profile).unwrap(), first);
        let updated = NodeRuntime {
            node: home.join("node v2/bin/node"),
            npm: home.join("node v2/bin/npm"),
        };
        configure_terminal_path_at(&home, Path::new("/bin/zsh"), None, &updated).unwrap();
        assert_eq!(fs::read_to_string(&profile).unwrap(), first);
        let output = std::process::Command::new("/bin/zsh")
            .args([
                "-fc",
                "source \"$1\"; source \"$1\"; printf '%s' \"$PATH\"",
                "test",
            ])
            .arg(&profile)
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let paths = std::env::split_paths(&std::ffi::OsString::from(
            String::from_utf8(output.stdout).unwrap(),
        ))
        .collect::<Vec<_>>();
        assert_eq!(paths[0], home.join(".local/bin"));
        assert_eq!(paths[1], home.join("node v2/bin"));
        assert_eq!(
            paths
                .iter()
                .filter(|p| **p == home.join(".local/bin"))
                .count(),
            1
        );
        assert!(!paths.contains(&home.join("node v1/bin")));
        let output = std::process::Command::new("/bin/zsh")
            .args([
                "-fc",
                "source \"$1\"; printf '%s' \"$npm_config_prefix\"",
                "test",
            ])
            .arg(&profile)
            .env("npm_config_prefix", "/previous/npm/prefix")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            home.join(".local").to_string_lossy()
        );
        fs::remove_dir_all(home.parent().unwrap()).unwrap();
    }

    #[test]
    fn terminal_path_setup_honors_zdotdir_and_bash_existing_login_file() {
        let home = std::env::temp_dir().join(format!(
            "little-mungbean-shell-test-{}",
            rand::random::<u64>()
        ));
        fs::create_dir_all(&home).unwrap();
        let runtime = NodeRuntime {
            node: home.join("node/bin/node"),
            npm: home.join("node/bin/npm"),
        };
        let zdir = home.join("zsh-config");
        configure_terminal_path_at(&home, Path::new("/bin/zsh"), Some(&zdir), &runtime).unwrap();
        assert!(zdir.join(".zshrc").is_file());
        assert!(!home.join(".zshrc").exists());
        fs::write(home.join(".profile"), "# existing login file\n").unwrap();
        configure_terminal_path_at(&home, Path::new("/bin/bash"), None, &runtime).unwrap();
        assert!(!home.join(".bash_profile").exists());
        assert!(
            fs::read_to_string(home.join(".profile"))
                .unwrap()
                .starts_with("# existing login file\n")
        );
        assert!(home.join(".bashrc").is_file());
        let output = std::process::Command::new("/bin/bash")
            .args(["-c", ". \"$1\"; command -v bash", "test"])
            .arg(home.join(".bashrc"))
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        assert!(output.status.success());
        fs::remove_dir_all(home).unwrap();
    }
}
