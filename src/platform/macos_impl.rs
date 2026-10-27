//! macOS system adapter. Never change the user's shell profile or kill Terminal.
use crate::{gui_worker::ClientTarget, i18n};
use anyhow::{Context, Result, bail};
use chacha20poly1305::{
    ChaCha20Poly1305, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use tokio::process::Command as TokioCommand;
use zeroize::Zeroizing;

pub struct NamedMutexGuard(File);
impl Drop for NamedMutexGuard {
    fn drop(&mut self) {
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}
pub fn try_acquire_named_mutex(name: &str) -> Result<Option<NamedMutexGuard>> {
    try_acquire_named_mutex_at(&crate::platform_paths::data_root()?, name)
}

fn mutex_file_name(name: &str) -> String {
    // Keep a stable GUI lock filename within the current application data root.
    if name == r"Local\Little-Mungbean-gui-v1" {
        "gui.lock".to_owned()
    } else {
        use sha2::{Digest, Sha256};
        format!("mutex-{:x}.lock", Sha256::digest(name.as_bytes()))
    }
}

pub(crate) fn try_acquire_named_mutex_at(
    root: &Path,
    name: &str,
) -> Result<Option<NamedMutexGuard>> {
    fs::create_dir_all(root)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(root.join(mutex_file_name(name)))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        Ok(Some(NamedMutexGuard(file)))
    } else {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(error.into())
        }
    }
}
pub fn activate_window_by_title(_title: &str) -> bool {
    crate::platform_paths::data_root()
        .and_then(|root| Ok(fs::write(root.join("show-window"), b"show")?))
        .is_ok()
}
pub fn take_show_request() -> bool {
    crate::platform_paths::data_root()
        .is_ok_and(|root| fs::remove_file(root.join("show-window")).is_ok())
}

const SECRET_MAGIC: &[u8] = b"Little-Mungbean-Keychain-v1\0";
pub fn protect_or_unprotect(input: &[u8], encrypt: bool) -> Result<Vec<u8>> {
    static KEY: OnceLock<Mutex<Option<Zeroizing<Vec<u8>>>>> = OnceLock::new();
    let mut cached = KEY
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| anyhow::anyhow!("Keychain state is unavailable"))?;
    if cached.is_none() {
        let service = "Little-Mungbean";
        let account = "configuration-encryption-v1";
        let key = match security_framework::passwords::get_generic_password(service, account) {
            Ok(key) => Zeroizing::new(key),
            Err(error) if error.code() == -25300 && encrypt => {
                let key = Zeroizing::new(rand::random::<[u8; 32]>().to_vec());
                security_framework::passwords::set_generic_password(service, account, &key)
                    .context("Unable to save the encryption key in macOS Keychain")?;
                key
            }
            Err(error) => {
                return Err(error).context("Unable to unlock the macOS configuration key");
            }
        };
        if key.len() != 32 {
            bail!("The Keychain configuration key has an invalid length")
        }
        *cached = Some(key);
    }
    let cipher = ChaCha20Poly1305::new_from_slice(cached.as_ref().unwrap())
        .map_err(|_| anyhow::anyhow!("Invalid configuration encryption key"))?;
    if encrypt {
        let nonce = rand::random::<[u8; 12]>();
        let encrypted = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: input,
                    aad: SECRET_MAGIC,
                },
            )
            .map_err(|_| anyhow::anyhow!("Unable to encrypt configuration"))?;
        let mut output = SECRET_MAGIC.to_vec();
        output.extend_from_slice(&nonce);
        output.extend(encrypted);
        Ok(output)
    } else {
        let payload = input
            .strip_prefix(SECRET_MAGIC)
            .filter(|bytes| bytes.len() >= 28)
            .context("Invalid macOS encrypted configuration")?;
        cipher
            .decrypt(
                Nonce::from_slice(&payload[..12]),
                Payload {
                    msg: &payload[12..],
                    aad: SECRET_MAGIC,
                },
            )
            .map_err(|_| {
                anyhow::anyhow!("Configuration authentication failed; no settings were overwritten")
            })
    }
}

pub fn user_prefers_chinese() -> bool {
    Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleLanguages"])
        .output()
        .ok()
        .filter(|r| r.status.success())
        .and_then(|r| String::from_utf8(r.stdout).ok())
        .and_then(|text| {
            text.lines()
                .map(str::trim)
                .find(|line| {
                    line.starts_with('"') || line.starts_with("zh") || line.starts_with("en")
                })
                .map(str::to_owned)
        })
        .is_some_and(|language| language.trim_start_matches('"').starts_with("zh"))
}
pub fn open_external_url(url: &str) -> Result<()> {
    let parsed = url::Url::parse(url)?;
    if !matches!(parsed.scheme(), "https" | "http") {
        bail!("Unsupported external URL")
    }
    let status = Command::new("/usr/bin/open").arg(url).status()?;
    if !status.success() {
        bail!("Unable to open the external URL")
    }
    Ok(())
}
pub fn attach_parent_console() {}
pub fn hide_current_console() {}
pub fn report_gui_startup_failure(message: &str) {
    eprintln!("{message}");
    if let Ok(root) = crate::platform_paths::data_root() {
        let _ = fs::create_dir_all(root.join("logs"));
        let _ = fs::write(root.join("logs/gui-startup-error.log"), message);
    }
}
pub fn install_gui_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        report_gui_startup_failure(&info.to_string());
        previous(info);
    }));
}

pub fn find_executable(name: &str) -> Option<PathBuf> {
    if !matches!(name, "codex" | "claude" | "npm" | "node" | "brew") {
        return None;
    }
    executable_search_dirs()
        .into_iter()
        .map(|p| p.join(name))
        .find(|p| {
            cli_executable_candidate(p, name)
                && (!matches!(name, "codex" | "claude") || cli_version_works(p, name))
        })
}

#[derive(PartialEq, Eq)]
struct ExecutableIdentity {
    path: PathBuf,
    device: u64,
    inode: u64,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

fn executable_identity(path: &Path) -> Option<ExecutableIdentity> {
    use std::os::unix::fs::MetadataExt;
    let path = path.canonicalize().ok()?;
    let metadata = path.metadata().ok()?;
    Some(ExecutableIdentity {
        path,
        device: metadata.dev(),
        inode: metadata.ino(),
        size: metadata.len(),
        modified: (metadata.mtime(), metadata.mtime_nsec()),
        changed: (metadata.ctime(), metadata.ctime_nsec()),
    })
}

struct CliVersionProbe {
    identities: Vec<ExecutableIdentity>,
    checked: Instant,
    valid: bool,
}

fn cli_version_works(executable: &Path, name: &str) -> bool {
    static PROBES: OnceLock<Mutex<std::collections::HashMap<(PathBuf, String), CliVersionProbe>>> =
        OnceLock::new();
    let Some(identity) = executable_identity(executable) else {
        return false;
    };
    let mut identities = vec![identity];
    if let Some(node) = executable
        .parent()
        .map(|parent| parent.join("node"))
        .filter(|path| executable_file(path))
        .or_else(|| find_executable("node"))
        .and_then(|p| executable_identity(&p))
    {
        identities.push(node);
    }
    let cache = PROBES.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let key = (executable.to_path_buf(), name.to_owned());
    if let Ok(probes) = cache.lock()
        && let Some(probe) = probes.get(&key)
        && probe.identities == identities
        && probe.checked.elapsed() < Duration::from_secs(if probe.valid { 30 } else { 3 })
    {
        return probe.valid;
    }
    let valid = probe_cli_version(executable, name, Duration::from_secs(3));
    if let Ok(mut probes) = cache.lock() {
        probes.retain(|_, probe| probe.checked.elapsed() < Duration::from_secs(30));
        probes.insert(
            key,
            CliVersionProbe {
                identities,
                checked: Instant::now(),
                valid,
            },
        );
    }
    valid
}

fn probe_cli_version(executable: &Path, name: &str, timeout: Duration) -> bool {
    let Ok(search_path) = executable_search_path(executable) else {
        return false;
    };
    let mut command = Command::new(executable);
    command.arg("--version").env("PATH", search_path);
    bounded_probe_output(&mut command, timeout)
        .is_some_and(|bytes| valid_cli_version(name, &String::from_utf8_lossy(&bytes)))
}

fn bounded_probe_output(command: &mut Command, timeout: Duration) -> Option<Vec<u8>> {
    use std::{
        io::{Read, Seek, SeekFrom},
        os::unix::process::CommandExt,
        process::Stdio,
    };
    let path = std::env::temp_dir().join(format!(
        "little-mungbean-cli-probe-{:016x}",
        rand::random::<u64>()
    ));
    let mut output = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .ok()?;
    let _ = fs::remove_file(&path);
    let stdout = output.try_clone().ok()?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                output.seek(SeekFrom::Start(0)).ok()?;
                let mut bytes = Vec::new();
                output.take(65536).read_to_end(&mut bytes).ok()?;
                return Some(bytes);
            }
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => {
                // Only the private detection process group is terminated.
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn valid_cli_version(name: &str, output: &str) -> bool {
    let output = output.trim();
    let version = match name {
        "codex" => output.strip_prefix("codex-cli "),
        "claude" => output.strip_suffix(" (Claude Code)"),
        _ => None,
    };
    version.is_some_and(|version| {
        let numeric = version.split(['-', '+']).next().unwrap_or_default();
        let parts = numeric.split('.').collect::<Vec<_>>();
        parts.len() == 3
            && parts
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
    })
}

fn executable_search_dirs() -> Vec<PathBuf> {
    let mut dirs = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    if let Some(home) = directories::UserDirs::new() {
        for relative in [
            ".local/bin",
            ".npm-global/bin",
            ".npm/bin",
            ".volta/bin",
            ".bun/bin",
            ".cargo/bin",
        ] {
            dirs.push(home.home_dir().join(relative));
        }
        let nvm = home.home_dir().join(".nvm/versions/node");
        if let Ok(entries) = fs::read_dir(nvm) {
            let mut versions = entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .collect::<Vec<_>>();
            versions.sort();
            dirs.extend(versions.into_iter().rev().map(|p| p.join("bin")));
        }
        // The macOS installer keeps its password-free Node.js runtime under
        // the user's data directory. Include the newest managed runtime in
        // normal discovery so a later CLI install/reconnect sees the same
        // node/npm as the installer did, even when the GUI was launched from
        // Finder or Dock with a minimal PATH.
        let managed_node = home.home_dir().join(".local/share/Little-Mungbean/node");
        if let Ok(entries) = fs::read_dir(managed_node) {
            let mut versions = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.is_dir())
                .collect::<Vec<_>>();
            versions.sort();
            dirs.extend(versions.into_iter().rev().map(|p| p.join("bin")));
        }
    }
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].map(PathBuf::from));
    dirs
}

fn executable_search_path(executable: &Path) -> Result<OsString> {
    let dirs = executable
        .parent()
        .map(Path::to_path_buf)
        .into_iter()
        .chain(executable_search_dirs());
    std::env::join_paths(dirs).context("Unable to construct macOS executable PATH")
}

fn executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn outer_app_bundle(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .filter(|p| p.extension().is_some_and(|ext| ext == "app"))
        .last()
        .map(Path::to_path_buf)
}

fn cli_executable_candidate(path: &Path, name: &str) -> bool {
    if !executable_file(path) {
        return false;
    }
    // A desktop app's private CLI/server is not a standalone terminal install,
    // even when the host injects it into PATH or a symlink points to it.
    !matches!(name, "codex" | "claude")
        || path
            .canonicalize()
            .is_ok_and(|p| outer_app_bundle(&p).is_none())
}

pub fn terminal_client_installed(command: &str) -> bool {
    if !matches!(command, "codex" | "claude") {
        return false;
    }
    // GUI polling must not wait on a slow/broken CLI's --version command.
    // Launch/install paths use find_executable directly for a fresh answer.
    type Scan = (Instant, bool, Option<PathBuf>);
    static SCANS: OnceLock<Mutex<std::collections::HashMap<String, Scan>>> = OnceLock::new();
    let scans = SCANS.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let Ok(mut entries) = scans.lock() else {
        return false;
    };
    let entry = entries
        .entry(command.to_owned())
        .or_insert_with(|| (Instant::now() - Duration::from_secs(3), false, None));
    let installed = entry
        .2
        .as_ref()
        .is_some_and(|path| cli_executable_candidate(path, command));
    if !entry.1 && entry.0.elapsed() >= Duration::from_secs(3) {
        entry.1 = true;
        let name = command.to_owned();
        let spawned = std::thread::Builder::new()
            .name(format!("cli-detect-{name}"))
            .spawn(move || {
                let executable = find_executable(&name);
                if let Ok(mut entries) = scans.lock() {
                    entries.insert(name, (Instant::now(), false, executable));
                }
            });
        if spawned.is_err() {
            entry.1 = false;
        }
    }
    installed
}

fn desktop_bundle_id(target: ClientTarget) -> Result<&'static str> {
    match target {
        ClientTarget::CodexDesktop => Ok("com.openai.codex"),
        ClientTarget::ClaudeDesktop => Ok("com.anthropic.claudefordesktop"),
        _ => bail!("Not a desktop client"),
    }
}

fn bundle_metadata(bundle: &Path) -> Option<serde_json::Value> {
    let output = Command::new("/usr/bin/plutil")
        .args(["-convert", "json", "-o", "-"])
        .arg(bundle.join("Contents/Info.plist"))
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| serde_json::from_slice(&output.stdout).ok())
        .flatten()
}

fn bundle_target(bundle: &Path) -> Option<ClientTarget> {
    let metadata = bundle_metadata(bundle)?;
    match metadata.get("CFBundleIdentifier")?.as_str()? {
        "com.openai.codex" => Some(ClientTarget::CodexDesktop),
        "com.anthropic.claudefordesktop" => Some(ClientTarget::ClaudeDesktop),
        _ => None,
    }
}

fn bundle_executable(bundle: &Path) -> Option<PathBuf> {
    let metadata = bundle_metadata(bundle)?;
    let name = metadata.get("CFBundleExecutable")?.as_str()?;
    if name.is_empty() || name.contains('/') || matches!(name, "." | "..") {
        return None;
    }
    let executable = bundle.join("Contents/MacOS").join(name);
    executable_file(&executable).then_some(executable)
}

// LaunchServices finds registered applications regardless of their filename or
// location. Validate each result because stale registrations can survive removal.
fn registered_applications(identifier: &str) -> Vec<PathBuf> {
    use std::{
        ffi::{CString, OsString, c_void},
        os::unix::ffi::OsStringExt,
    };
    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        fn LSCopyApplicationURLsForBundleIdentifier(
            id: *const c_void,
            error: *mut *const c_void,
        ) -> *const c_void;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            allocator: *const c_void,
            text: *const libc::c_char,
            encoding: u32,
        ) -> *const c_void;
        fn CFArrayGetCount(array: *const c_void) -> isize;
        fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
        fn CFURLGetFileSystemRepresentation(
            url: *const c_void,
            resolve: u8,
            buffer: *mut u8,
            length: isize,
        ) -> u8;
        fn CFRelease(value: *const c_void);
    }
    let Ok(identifier) = CString::new(identifier) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    unsafe {
        let id = CFStringCreateWithCString(std::ptr::null(), identifier.as_ptr(), 0x08000100);
        if id.is_null() {
            return paths;
        }
        let urls = LSCopyApplicationURLsForBundleIdentifier(id, std::ptr::null_mut());
        CFRelease(id);
        if urls.is_null() {
            return paths;
        }
        for index in 0..CFArrayGetCount(urls) {
            let url = CFArrayGetValueAtIndex(urls, index);
            let mut buffer = vec![0u8; 4096];
            if CFURLGetFileSystemRepresentation(url, 1, buffer.as_mut_ptr(), buffer.len() as isize)
                != 0
            {
                if let Some(end) = buffer.iter().position(|b| *b == 0) {
                    buffer.truncate(end);
                    paths.push(PathBuf::from(OsString::from_vec(buffer)));
                }
            }
        }
        CFRelease(urls);
    }
    paths
}

pub fn desktop_bundle(target: ClientTarget) -> Result<PathBuf> {
    desktop_bundle_candidates(target)?
        .into_iter()
        .find(|p| bundle_target(p) == Some(target) && bundle_executable(p).is_some())
        .with_context(|| {
            format!(
                "{} • {}",
                target.title(),
                i18n::tr("未安装 • 请先安装", "Not installed • Install first")
            )
        })
}

fn desktop_bundle_candidates(target: ClientTarget) -> Result<Vec<PathBuf>> {
    use objc2_app_kit::NSRunningApplication;
    use objc2_foundation::NSString;

    let identifier = desktop_bundle_id(target)?;
    // Query the OS by exact client identity. Do not open every running app's
    // Info.plist, including unrelated apps inside privacy-protected directories.
    let mut candidates = objc2::rc::autoreleasepool(|_| {
        NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(
            identifier,
        ))
        .iter()
        .filter_map(|app| {
            app.bundleURL()?
                .path()
                .map(|p| PathBuf::from(p.to_string()))
        })
        .collect::<Vec<_>>()
    });
    candidates.extend(registered_applications(identifier));
    // Retain conventional unregistered installs without enumerating all apps.
    let names: &[&str] = match target {
        ClientTarget::CodexDesktop => &["Codex.app", "ChatGPT.app"],
        ClientTarget::ClaudeDesktop => &["Claude.app"],
        _ => unreachable!(),
    };
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Some(home) = directories::UserDirs::new() {
        roots.push(home.home_dir().join("Applications"));
    }
    for root in roots {
        candidates.extend(names.iter().map(|name| root.join(name)));
    }
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|path| seen.insert(path.clone()));
    Ok(candidates)
}

pub fn desktop_executable(target: ClientTarget) -> Result<PathBuf> {
    bundle_executable(&desktop_bundle(target)?)
        .context("Application executable is missing or invalid")
}
pub fn store_codex_installed() -> bool {
    desktop_bundle(ClientTarget::CodexDesktop).is_ok()
}
pub fn store_claude_installed() -> bool {
    desktop_bundle(ClientTarget::ClaudeDesktop).is_ok()
}

#[derive(Clone)]
struct Process {
    pid: u32,
    parent: u32,
    target: Option<ClientTarget>,
}
fn processes() -> Result<Vec<Process>> {
    let mut known_bundles = std::collections::HashMap::new();
    for target in [ClientTarget::CodexDesktop, ClientTarget::ClaudeDesktop] {
        for path in desktop_bundle_candidates(target)? {
            known_bundles.insert(path, target);
        }
    }
    processes_for_bundles(&known_bundles)
}

fn processes_for_bundles(
    known_bundles: &std::collections::HashMap<PathBuf, ClientTarget>,
) -> Result<Vec<Process>> {
    let output = Command::new("/bin/ps")
        .args(["-axo", "pid=,ppid=,uid=,stat=,args="])
        .output()?;
    if !output.status.success() {
        bail!("Unable to inspect running processes")
    }
    let uid = unsafe { libc::getuid() };
    let mut bundles = std::collections::HashMap::new();
    let mut active_bundles = std::collections::HashSet::new();
    let mut found = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some((pid, parent, owner, command)) = live_process_row(line) else {
            continue;
        };
        if owner != uid || pid == std::process::id() {
            continue;
        }
        let image = process_image_path(pid).ok().map(PathBuf::from);
        let bundle = image.as_deref().and_then(outer_app_bundle);
        // Use the actual image, not arbitrary arguments (including user prompts).
        let target = image.as_deref().and_then(|image| {
            classify_known_process(image, command, known_bundles, |bundle| {
                bundles
                    .entry(bundle.to_path_buf())
                    .or_insert_with(|| bundle_target(bundle).zip(bundle_executable(bundle)))
                    .as_ref()
                    .map(|(target, _)| *target)
            })
        });
        if let Some(bundle) = &bundle
            && let Some(Some((_, executable))) = bundles.get(bundle)
            && image.as_ref() == Some(executable)
        {
            active_bundles.insert(bundle.clone());
        }
        found.push((
            Process {
                pid,
                parent,
                target,
            },
            bundle,
        ));
    }
    Ok(found
        .into_iter()
        .map(|(mut process, bundle)| {
            // Crash handlers or embedded servers can outlive the desktop app.
            // Count its helpers only while that bundle's main executable is running.
            if let Some(bundle) = bundle
                && !active_bundles.contains(&bundle)
            {
                process.target = None;
            }
            process
        })
        .collect())
}

fn live_process_row(mut raw: &str) -> Option<(u32, u32, u32, &str)> {
    fn column<'a>(raw: &mut &'a str) -> Option<&'a str> {
        *raw = raw.trim_start();
        if raw.is_empty() {
            return None;
        }
        let end = raw.find(char::is_whitespace).unwrap_or(raw.len());
        let value = &raw[..end];
        *raw = &raw[end..];
        Some(value)
    }
    let pid = column(&mut raw)?.parse().ok()?;
    let parent = column(&mut raw)?.parse().ok()?;
    let owner = column(&mut raw)?.parse().ok()?;
    let state = column(&mut raw)?;
    // Match identity(): a zombie is an exited process awaiting collection,
    // even when Terminal still retains its process-table entry.
    if state.starts_with('Z') {
        return None;
    }
    Some((pid, parent, owner, raw.trim_start()))
}

fn classify_known_process(
    image: &Path,
    command: &str,
    known_bundles: &std::collections::HashMap<PathBuf, ClientTarget>,
    mut identify_bundle: impl FnMut(&Path) -> Option<ClientTarget>,
) -> Option<ClientTarget> {
    classify_process(image, command, |bundle| {
        let expected = known_bundles.get(bundle)?;
        // Still validate exact identity before permitting restart/termination.
        identify_bundle(bundle).filter(|actual| actual == expected)
    })
}

fn classify_process(
    image: &Path,
    command: &str,
    mut identify_bundle: impl FnMut(&Path) -> Option<ClientTarget>,
) -> Option<ClientTarget> {
    // Resolve the outer application first, including its CLI, renderer and
    // crash-handler helpers. Other apps' embedded tools are not standalone CLIs.
    if let Some(bundle) = outer_app_bundle(image) {
        return identify_bundle(&bundle);
    }
    let name = image.file_name()?.to_str()?;
    match name {
        // Codex CLI installs an app-server daemon alongside the interactive
        // command. The daemon is intentionally detached from the terminal and
        // must not keep the CLI card in the GUI marked as running after its
        // Terminal window has been closed.
        "codex"
            if image
                .to_str()
                .is_some_and(|path| path.contains("/app-server-daemon/")) =>
        {
            None
        }
        "codex" => Some(ClientTarget::CodexCli),
        "claude" => Some(ClientTarget::ClaudeCode),
        // Recent npm installs resolve the `claude` symlink to a native Mach-O
        // executable named claude.exe. Match its package path, not arbitrary
        // command arguments or other applications' embedded executables.
        "claude.exe"
            if image.to_str().is_some_and(|path| {
                path.ends_with("/@anthropic-ai/claude-code/bin/claude.exe")
            }) =>
        {
            Some(ClientTarget::ClaudeCode)
        }
        "node" | "nodejs" | "bun" => {
            // Only inspect the script argument, never arbitrary argument text.
            let arguments = command.strip_prefix(image.to_str()?).or_else(|| {
                command
                    .split_once(char::is_whitespace)
                    .map(|(_, args)| args)
            })?;
            let mut arguments = arguments.split_whitespace();
            let script = loop {
                let argument = arguments.next()?;
                if matches!(argument, "-e" | "--eval" | "-p" | "--print")
                    || argument.starts_with("--eval=")
                    || argument.starts_with("--print=")
                {
                    return None;
                }
                if argument == "--" {
                    break arguments.next()?;
                }
                if matches!(
                    argument,
                    "-r" | "--require"
                        | "--import"
                        | "--loader"
                        | "--experimental-loader"
                        | "--conditions"
                        | "-C"
                ) {
                    arguments.next()?;
                    continue;
                }
                if !argument.starts_with('-') {
                    break argument;
                }
            };
            if outer_app_bundle(Path::new(script)).is_some() {
                return None;
            }
            if script.contains("/@openai/codex/") && script.ends_with("/bin/codex.js") {
                Some(ClientTarget::CodexCli)
            } else if script.contains("/@anthropic-ai/claude-code/") && script.ends_with("/cli.js")
            {
                Some(ClientTarget::ClaudeCode)
            } else {
                None
            }
        }
        _ if image
            .to_str()
            .is_some_and(|p| p.contains("/.local/share/claude/versions/")) =>
        {
            Some(ClientTarget::ClaudeCode)
        }
        _ => None,
    }
}
pub fn running_client_processes() -> Result<Vec<(ClientTarget, u32)>> {
    Ok(processes()?
        .into_iter()
        .filter_map(|p| p.target.map(|t| (t, p.pid)))
        .collect())
}
fn ids(target: ClientTarget) -> Result<Vec<u32>> {
    Ok(running_client_processes()?
        .into_iter()
        .filter_map(|(t, p)| (t == target).then_some(p))
        .collect())
}
pub fn running_codex_desktop_process_ids() -> Result<Vec<u32>> {
    ids(ClientTarget::CodexDesktop)
}
pub fn running_claude_desktop_process_ids() -> Result<Vec<u32>> {
    ids(ClientTarget::ClaudeDesktop)
}
pub fn running_codex_processes() -> Result<Vec<String>> {
    descriptions(ClientTarget::CodexDesktop)
}
pub fn running_claude_desktop_processes() -> Result<Vec<String>> {
    descriptions(ClientTarget::ClaudeDesktop)
}
fn descriptions(target: ClientTarget) -> Result<Vec<String>> {
    Ok(ids(target)?
        .iter()
        .map(|pid| format!("{} PID {pid}", target.title()))
        .collect())
}
pub fn running_codex_companion_processes() -> Result<Vec<String>> {
    Ok(Vec::new())
}
pub fn process_image_path(pid: u32) -> Result<String> {
    #[link(name = "proc")]
    unsafe extern "C" {
        fn proc_pidpath(pid: libc::c_int, buffer: *mut libc::c_void, size: u32) -> libc::c_int;
    }
    let mut bytes = vec![0u8; 4096];
    let count = unsafe { proc_pidpath(pid as i32, bytes.as_mut_ptr().cast(), bytes.len() as u32) };
    if count <= 0 {
        bail!("Unable to inspect the process executable")
    }
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    Ok(String::from_utf8(bytes[..end].to_vec())?)
}
pub fn process_matches_image(pid: u32, expected: &Path) -> bool {
    let Ok(expected) = expected.canonicalize() else {
        return false;
    };
    process_image_path(pid).is_ok_and(|p| Path::new(&p).canonicalize().is_ok_and(|p| p == expected))
}
pub fn running_any_process_ids(pids: &[u32]) -> Result<Vec<u32>> {
    let all = processes()?;
    Ok(pids
        .iter()
        .copied()
        .filter(|pid| all.iter().any(|p| p.pid == *pid))
        .collect())
}
pub fn running_process_ids(pids: &[u32]) -> Result<Vec<u32>> {
    running_any_process_ids(pids)
}
fn identity(pid: u32) -> Result<String> {
    let output = Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "lstart=,stat="])
        .output()?;
    if !output.status.success() {
        bail!("The recorded process has exited")
    }
    live_process_stamp(&String::from_utf8(output.stdout)?)
}

fn live_process_stamp(raw: &str) -> Result<String> {
    let (stamp, state) = raw
        .trim()
        .rsplit_once(char::is_whitespace)
        .context("The recorded process has exited")?;
    if stamp.trim().is_empty() || state.starts_with('Z') {
        bail!("The recorded process has exited")
    }
    Ok(stamp.trim().to_owned())
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TerminalRestartSession {
    pid: u32,
    started: String,
    target: ClientTarget,
    window: OwnedTerminalWindow,
    image: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct OwnedTerminalWindow {
    id: u64,
    tty: String,
}

#[derive(Clone)]
struct LaunchedTerminal {
    started: String,
    window: OwnedTerminalWindow,
}

fn launched_terminal_windows() -> &'static Mutex<std::collections::HashMap<u32, LaunchedTerminal>> {
    static WINDOWS: OnceLock<Mutex<std::collections::HashMap<u32, LaunchedTerminal>>> =
        OnceLock::new();
    WINDOWS.get_or_init(Mutex::default)
}

// Terminal's scripting dictionary exposes close on windows. Create a dedicated
// window and close it only while it still contains our single, finished tab.
// CLI programs can change custom title via OSC, so it is not an ownership key.
const LAUNCH_TERMINAL_WINDOW: &str = r#"
on run argv
    tell application "Terminal"
        set ownedTab to do script (item 1 of argv)
        set custom title of ownedTab to (item 2 of argv)
        set ownedTTY to tty of ownedTab
        activate
        repeat with candidate in windows
            repeat with candidateTab in tabs of candidate
                if tty of candidateTab is ownedTTY then
                    return (id of candidate as text) & linefeed & ownedTTY
                end if
            end repeat
        end repeat
        error "Could not identify the launched Terminal window"
    end tell
end run
"#;

const CLOSE_TERMINAL_WINDOW: &str = r#"
on run argv
    if application "Terminal" is not running then return "missing"
    tell application "Terminal"
        repeat with candidate in windows
            if (id of candidate as text) is (item 1 of argv) then
                if (count of tabs of candidate) is not 1 then return "retained-tabs"
                set ownedTab to tab 1 of candidate
                if (tty of ownedTab) is not (item 2 of argv) then return "retained-tty"
                if busy of ownedTab then return "busy"
                close candidate
                return "closed"
            end if
        end repeat
        return "missing"
    end tell
end run
"#;

fn terminal_script(script: &str, arguments: &[&str]) -> Result<String> {
    let output = Command::new("/usr/bin/osascript")
        .args(["-e", script, "--"])
        .args(arguments)
        .output()?;
    if !output.status.success() {
        bail!(
            "{}",
            i18n::tr(
                "无法管理 Little-Mungbean 的终端窗口；请允许控制 Terminal 的自动化权限，或手动关闭旧窗口",
                "Unable to manage the Little-Mungbean terminal window; allow Terminal automation or close the old window manually",
            )
        );
    }
    String::from_utf8(output.stdout).context("Invalid Terminal automation response")
}

impl OwnedTerminalWindow {
    fn from_launch(raw: &str) -> Result<Self> {
        let mut lines = raw.lines();
        let id = lines
            .next()
            .context("Missing Terminal window ID")?
            .trim()
            .parse::<u64>()?;
        let tty = lines
            .next()
            .context("Missing Terminal TTY")?
            .trim()
            .to_owned();
        let terminal_number = tty.strip_prefix("/dev/ttys");
        if id == 0
            || !terminal_number.is_some_and(|number| {
                !number.is_empty() && number.bytes().all(|b| b.is_ascii_alphanumeric())
            })
            || lines.next().is_some()
        {
            bail!("Invalid Terminal window identity");
        }
        Ok(Self { id, tty })
    }

    fn close_finished(&self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let result =
                terminal_script(CLOSE_TERMINAL_WINDOW, &[&self.id.to_string(), &self.tty])?;
            match result.trim() {
                "closed" | "missing" => return Ok(()),
                "retained-tabs" | "retained-tty" => {
                    println!(
                        "{}",
                        i18n::tr(
                            "旧终端窗口的标签页数量或 TTY 已改变，为保护其他会话已保留窗口",
                            "The old terminal window's tab count or TTY changed; it was retained to protect other sessions",
                        )
                    );
                    return Ok(());
                }
                "busy" if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100))
                }
                _ => bail!(
                    "{}",
                    i18n::tr(
                        "旧终端窗口仍忙碌，已保留，请手动关闭",
                        "The old terminal window is still busy; close it manually"
                    )
                ),
            }
        }
    }
}

impl TerminalRestartSession {
    pub fn close_finished_if_exited(&self) -> Result<()> {
        if identity(self.pid).ok().as_ref() == Some(&self.started) {
            bail!("The owned terminal process is still running; its window was retained")
        }
        self.window.close_finished()?;
        self.forget_saved()
    }

    pub fn record(pid: u32, target: ClientTarget) -> Result<Self> {
        let launched = launched_terminal_windows()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&pid)
            .cloned()
            .context("The terminal window was not launched by Little-Mungbean")?;
        if identity(pid)? != launched.started {
            launched_terminal_windows()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&pid);
            bail!("The launched terminal process identity changed")
        }
        let session = Self {
            pid,
            started: launched.started,
            target,
            window: launched.window,
            image: PathBuf::from(process_image_path(pid)?),
        };
        session.verify_client_sessions()?;
        session.save()?;
        launched_terminal_windows()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&pid);
        Ok(session)
    }
    pub fn verify_client_sessions(&self) -> Result<()> {
        if !self.matches_live_process()? {
            bail!("The terminal process identity changed")
        }
        self.verify_client_processes(&processes()?)
    }

    fn verify_client_processes(&self, all: &[Process]) -> Result<()> {
        let mut owned = vec![self.pid];
        loop {
            let before = owned.len();
            for p in all {
                if owned.contains(&p.parent) && !owned.contains(&p.pid) {
                    owned.push(p.pid)
                }
            }
            if before == owned.len() {
                break;
            }
        }
        if !all
            .iter()
            .any(|p| owned.contains(&p.pid) && p.target == Some(self.target))
        {
            bail!("The owned terminal client is no longer running")
        }
        if all
            .iter()
            .any(|p| p.target == Some(self.target) && !owned.contains(&p.pid))
        {
            bail!("Other client sessions are running; close them before reconnecting")
        }
        Ok(())
    }
    pub fn close(&self) -> Result<()> {
        self.verify_client_sessions()?;
        let all = processes()?;
        let mut owned = vec![self.pid];
        loop {
            let before = owned.len();
            for p in &all {
                if owned.contains(&p.parent) && !owned.contains(&p.pid) {
                    owned.push(p.pid)
                }
            }
            if before == owned.len() {
                break;
            }
        }
        let recorded = owned
            .into_iter()
            .filter_map(|pid| identity(pid).ok().map(|stamp| (pid, stamp)))
            .collect::<Vec<_>>();
        for (pid, stamp) in recorded.iter().rev() {
            if identity(*pid).ok().as_ref() == Some(stamp) {
                unsafe { libc::kill(*pid as i32, libc::SIGTERM) };
            }
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while recorded
            .iter()
            .any(|(pid, stamp)| identity(*pid).ok().as_ref() == Some(stamp))
        {
            if Instant::now() >= deadline {
                bail!("The terminal client did not exit; no unrelated terminal was closed")
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        self.window.close_finished()?;
        self.forget_saved()
    }
}

#[path = "macos_terminal_state.rs"]
mod terminal_state;

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn terminal_exec_command(script: &Path) -> String {
    format!("exec /bin/sh {}", shell_quote(&script.to_string_lossy()))
}

fn app_server_is_running(status: &[u8]) -> Result<bool> {
    let status: serde_json::Value = serde_json::from_slice(status)
        .context("Codex CLI returned an invalid app-server status")?;
    Ok(status.get("status").and_then(serde_json::Value::as_str) == Some("running"))
}

fn codex_daemon_command(
    executable: &Path,
    codex_home: &Path,
    search_path: &OsString,
    environment: &[(&str, &str)],
    arguments: &[&str],
) -> TokioCommand {
    let mut command = TokioCommand::new(executable);
    command
        .args(arguments)
        .env("CODEX_HOME", codex_home)
        .env("PATH", search_path)
        .envs(environment.iter().copied());
    for name in [
        "OPENAI_BASE_URL",
        "OPENAI_API_KEY",
        "CODEX_APP_SERVER_OPENAI_BASE_URL",
        "CODEX_APP_SERVER_CHATGPT_BASE_URL",
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_NO_MODEL_FALLBACK",
        "CLAUDE_CODE_DISABLE_MODEL_ACCESS_FALLBACK",
        "CLAUDE_CODE_DISABLE_REFUSAL_FALLBACK",
    ] {
        command.env_remove(name);
    }
    command.kill_on_drop(true);
    command
}

// The managed daemon outlives Terminal and keeps the model catalog loaded from
// its previous startup. Refresh it after writing this launch's selected list.
pub async fn refresh_codex_cli_app_server(
    codex_home: &Path,
    environment: &[(&str, &str)],
) -> Result<bool> {
    let control_socket = codex_home.join("app-server-control/app-server-control.sock");
    if !control_socket.exists() {
        return Ok(false);
    }

    let executable = find_executable("codex").context("Codex CLI is not installed")?;
    let search_path = executable_search_path(&executable)?;
    let mut version_command = codex_daemon_command(
        &executable,
        codex_home,
        &search_path,
        environment,
        &["app-server", "daemon", "version"],
    );
    let version = tokio::time::timeout(Duration::from_secs(5), version_command.output())
        .await
        .context("Timed out while checking the Codex CLI app-server")??;
    if !version.status.success() {
        bail!("Unable to inspect the Codex CLI app-server before reconnecting")
    }
    if !app_server_is_running(&version.stdout)? {
        return Ok(false);
    }

    let mut restart_command = codex_daemon_command(
        &executable,
        codex_home,
        &search_path,
        environment,
        &["app-server", "daemon", "restart"],
    );
    let restarted = tokio::time::timeout(Duration::from_secs(25), restart_command.output())
        .await
        .context("Timed out while refreshing the Codex CLI app-server")??;
    if !restarted.status.success() {
        bail!("Unable to refresh the Codex CLI app-server for the selected model list")
    }
    Ok(true)
}

pub fn launch_terminal_client_with_environment(
    title: &str,
    command: &str,
    environment: &[(&str, &str)],
) -> Result<u32> {
    let home = directories::UserDirs::new().context("Unable to locate user home")?;
    launch_terminal_client_with_environment_in_directory(
        title,
        command,
        environment,
        home.home_dir(),
    )
}
pub fn launch_terminal_client_with_environment_in_directory(
    title: &str,
    command: &str,
    environment: &[(&str, &str)],
    directory: &Path,
) -> Result<u32> {
    let executable = find_executable(command).context("Client is not installed")?;
    let root = crate::platform_paths::data_root()?.join("terminal-sessions");
    fs::create_dir_all(&root)?;
    let nonce = format!("{:016x}", rand::random::<u64>());
    let script = root.join(format!("{nonce}.command"));
    let pid_file = root.join(format!("{nonce}.pid"));
    let marker = format!("Little-Mungbean {nonce} • {title}");
    let mut content = format!(
        "#!/bin/sh\numask 077\ncd {} || exit 1\n",
        shell_quote(&directory.to_string_lossy())
    );
    for (name, value) in environment {
        if name.is_empty()
            || name.as_bytes()[0].is_ascii_digit()
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            bail!("Invalid environment variable name")
        }
        if value.is_empty() {
            content.push_str(&format!("unset {name}\n"))
        } else {
            content.push_str(&format!("export {name}={}\n", shell_quote(value)))
        }
    }
    // Never inherit an unrelated account/gateway route. Values explicitly
    // supplied above win; the terminal receives only the local gateway token.
    for name in [
        "OPENAI_BASE_URL",
        "OPENAI_API_KEY",
        "CODEX_APP_SERVER_OPENAI_BASE_URL",
        "CODEX_APP_SERVER_CHATGPT_BASE_URL",
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_NO_MODEL_FALLBACK",
        "CLAUDE_CODE_DISABLE_MODEL_ACCESS_FALLBACK",
        "CLAUDE_CODE_DISABLE_REFUSAL_FALLBACK",
    ] {
        if !environment.iter().any(|(key, _)| *key == name) {
            content.push_str(&format!("unset {name}\n"))
        }
    }
    let search_path = executable_search_path(&executable)?;
    content.push_str(&format!(
        "export PATH={}\n{{ echo $$; /usr/bin/tty; }} > {}\nrm -f {}\nexec {}\n",
        shell_quote(&search_path.to_string_lossy()),
        shell_quote(&pid_file.to_string_lossy()),
        shell_quote(&script.to_string_lossy()),
        shell_quote(&executable.to_string_lossy())
    ));
    use std::io::Write;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(&script)?
        .write_all(content.as_bytes())?;
    let result = (|| {
        // exec replaces the login shell so Terminal becomes idle after the CLI
        // exits. Pass strings as argv, never interpolate them into AppleScript.
        let shell_command = terminal_exec_command(&script);
        let raw = terminal_script(LAUNCH_TERMINAL_WINDOW, &[&shell_command, &marker])?;
        let window = OwnedTerminalWindow::from_launch(&raw)?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(raw) = fs::read_to_string(&pid_file) {
                let mut lines = raw.lines();
                if let (Some(pid), Some(tty)) = (lines.next(), lines.next())
                    && let Ok(pid) = pid.trim().parse::<u32>()
                    && pid != 0
                    && tty.trim() == window.tty
                {
                    let started = identity(pid)?;
                    launched_terminal_windows()
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(pid, LaunchedTerminal { started, window });
                    return Ok(pid);
                }
            }
            if Instant::now() >= deadline {
                bail!("Terminal did not start the client within 30 seconds")
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })();
    let _ = fs::remove_file(&pid_file);
    let _ = fs::remove_file(&script);
    result
}

pub struct DesktopRestartSession {
    target: ClientTarget,
}
impl DesktopRestartSession {
    pub fn begin(target: ClientTarget) -> Result<Self> {
        let identifier = desktop_bundle_id(target)?;
        // Request a normal application quit. Never force-terminate an app that
        // refuses to close or is waiting for a save confirmation.
        let output = Command::new("/usr/bin/osascript")
            .args([
                "-e",
                &format!("tell application id \"{identifier}\" to quit"),
            ])
            .output()?;
        if !output.status.success() {
            bail!("macOS refused to close {identifier}; allow automation or close it manually")
        }
        Ok(Self { target })
    }
    pub fn running(&self) -> Result<bool> {
        Ok(!ids(self.target)?.is_empty())
    }
    pub fn has_visible_windows(&self) -> Result<bool> {
        self.running()
    }
    pub fn terminate_background(&self) -> Result<()> {
        bail!("The app is still running; close it normally before reconnecting")
    }
}
#[derive(Debug, Default)]
pub struct CodexDesktopCloseResult {
    pub window_processes: Vec<(u32, String)>,
    pub process_tree: Vec<(u32, String)>,
    pub window_count: usize,
}
pub fn request_codex_desktop_close() -> Result<CodexDesktopCloseResult> {
    let processes = ids(ClientTarget::CodexDesktop)?
        .into_iter()
        .map(|pid| (pid, "Codex".to_owned()))
        .collect::<Vec<_>>();
    if !processes.is_empty() {
        DesktopRestartSession::begin(ClientTarget::CodexDesktop)?;
    }
    Ok(CodexDesktopCloseResult {
        window_count: usize::from(!processes.is_empty()),
        window_processes: processes.clone(),
        process_tree: processes,
    })
}
pub fn request_new_codex_desktop_close(before: &[u32]) -> Result<CodexDesktopCloseResult> {
    if ids(ClientTarget::CodexDesktop)?
        .iter()
        .any(|pid| before.contains(pid))
    {
        bail!("An existing Codex Desktop session is still running")
    }
    request_codex_desktop_close()
}
pub fn terminate_codex_desktop_process_tree(pids: &[u32]) -> Result<Vec<u32>> {
    if !running_any_process_ids(pids)?.is_empty() {
        bail!("Codex Desktop did not exit normally; force termination was skipped")
    }
    Ok(Vec::new())
}
// Verified against Claude Code CLI's model-access and refusal chains. Desktop
// builds its own chat subprocess environment; these are not a Desktop policy.
pub const CLAUDE_GATEWAY_MODEL_POLICY: [(&str, &str); 3] = [
    ("CLAUDE_CODE_NO_MODEL_FALLBACK", "1"),
    ("CLAUDE_CODE_DISABLE_MODEL_ACCESS_FALLBACK", "1"),
    ("CLAUDE_CODE_DISABLE_REFUSAL_FALLBACK", "1"),
];

pub fn configure_codex_desktop_environment(
    command: &mut tokio::process::Command,
    home: &Path,
    gateway: Option<(&str, &str)>,
) {
    command.env("CODEX_HOME", home);
    // Desktop converts these app-server variables to command-line overrides,
    // which outrank config.toml. Do not inherit another launcher's route.
    for key in [
        "OPENAI_BASE_URL",
        "OPENAI_API_KEY",
        "CODEX_APP_SERVER_OPENAI_BASE_URL",
        "CODEX_APP_SERVER_CHATGPT_BASE_URL",
    ] {
        command.env_remove(key);
    }
    if let Some((url, token)) = gateway {
        let url = url.trim_end_matches('/');
        command
            .env("OPENAI_BASE_URL", url)
            .env("CODEX_APP_SERVER_OPENAI_BASE_URL", url)
            .env("OPENAI_API_KEY", token)
            .env("NO_PROXY", "127.0.0.1,localhost,::1")
            .env("no_proxy", "127.0.0.1,localhost,::1");
    }
}

/// LaunchServices starts the Desktop app with its own macOS resource ownership.
/// Keep the helper to isolate launch environment changes from concurrent clients
/// and return the actual app PID for readiness, injection and lifecycle checks.
pub async fn spawn_desktop_detached(
    executable: &Path,
    target_args: &[String],
    configure: impl FnOnce(&mut TokioCommand),
) -> Result<u32> {
    let helper_executable = std::env::current_exe()?;
    let mut helper =
        desktop_launch_helper_command(&helper_executable, executable, target_args, configure);
    let output = helper
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .context("Detached Desktop helper failed to start")?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Detached Desktop helper exited with {}: {}",
            output.status,
            detail.trim()
        );
    }
    let pid = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .context("Detached Desktop helper returned an invalid PID")?;
    if pid == 0 {
        bail!("Detached Desktop helper returned PID 0");
    }
    Ok(pid)
}

fn desktop_launch_helper_command(
    helper_executable: &Path,
    executable: &Path,
    target_args: &[String],
    configure: impl FnOnce(&mut TokioCommand),
) -> TokioCommand {
    let mut helper = TokioCommand::new(helper_executable);
    helper
        .arg("internal-desktop-launch")
        .arg("--executable")
        .arg(executable);
    for argument in target_args {
        helper.arg("--target-arg").arg(argument);
    }
    configure(&mut helper);
    // LaunchServices has its own inherited environment. Explicitly clear keys
    // removed by the caller so a launchd value cannot restore a foreign route.
    let removed = helper
        .as_std()
        .get_envs()
        .filter_map(|(key, value)| value.is_none().then(|| key.to_os_string()))
        .collect::<Vec<_>>();
    for key in removed {
        helper.arg("--unset-env").arg(key);
    }
    helper
}

pub fn run_detached_desktop(
    executable: &Path,
    target_args: &[String],
    unset_environment: &[String],
) -> Result<u32> {
    use objc2_app_kit::{NSRunningApplication, NSWorkspace, NSWorkspaceOpenConfiguration};
    use objc2_foundation::{NSArray, NSDictionary, NSError, NSString, NSURL};

    let executable = executable
        .canonicalize()
        .context("Desktop executable is missing")?;
    let bundle =
        outer_app_bundle(&executable).context("Desktop executable is not in an app bundle")?;
    if bundle_executable(&bundle)
        .and_then(|path| path.canonicalize().ok())
        .as_ref()
        != Some(&executable)
    {
        bail!("Desktop executable does not match the app bundle's main executable");
    }
    let bundle = bundle.to_str().context("Desktop app path is not UTF-8")?;
    let environment = desktop_launch_environment(std::env::vars_os(), unset_environment)?;

    objc2::rc::autoreleasepool(|_| {
        let configuration = NSWorkspaceOpenConfiguration::configuration();
        configuration.setCreatesNewApplicationInstance(true);
        configuration.setAllowsRunningApplicationSubstitution(false);
        configuration.setAddsToRecentItems(false);
        let arguments = target_args
            .iter()
            .map(|s| NSString::from_str(s))
            .collect::<Vec<_>>();
        configuration.setArguments(&NSArray::from_retained_slice(&arguments));
        let keys = environment
            .keys()
            .map(|s| NSString::from_str(s))
            .collect::<Vec<_>>();
        let values = environment
            .values()
            .map(|s| NSString::from_str(s))
            .collect::<Vec<_>>();
        configuration.setEnvironment(&NSDictionary::from_slices(
            &keys.iter().map(|s| &**s).collect::<Vec<_>>(),
            &values.iter().map(|s| &**s).collect::<Vec<_>>(),
        ));

        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let completion =
            block2::RcBlock::new(move |app: *mut NSRunningApplication, error: *mut NSError| {
                // AppKit keeps both callback arguments alive for the callback.
                let result = unsafe {
                    if let Some(error) = error.as_ref() {
                        Err(format!(
                            "Desktop application launch failed ({} / {})",
                            error.domain(),
                            error.code()
                        ))
                    } else if let Some(app) = app.as_ref() {
                        u32::try_from(app.processIdentifier())
                            .ok()
                            .filter(|pid| *pid > 0)
                            .ok_or_else(|| "Desktop application returned an invalid PID".to_owned())
                    } else {
                        Err("Desktop application launch returned no application".to_owned())
                    }
                };
                let _ = sender.send(result);
            });
        NSWorkspace::sharedWorkspace().openApplicationAtURL_configuration_completionHandler(
            &NSURL::fileURLWithPath_isDirectory(&NSString::from_str(bundle), true),
            &configuration,
            Some(&completion),
        );
        // AppKit invokes the completion on a concurrent queue, not the main loop.
        let pid = receiver
            .recv_timeout(Duration::from_secs(30))
            .context("Timed out waiting for LaunchServices to start the Desktop app")?
            .map_err(anyhow::Error::msg)?;
        if !process_matches_image(pid, &executable) {
            bail!("LaunchServices did not start the requested Desktop executable");
        }
        Ok(pid)
    })
}

fn desktop_launch_environment(
    inherited: impl IntoIterator<Item = (OsString, OsString)>,
    unset_environment: &[String],
) -> Result<std::collections::BTreeMap<String, String>> {
    let mut environment = std::collections::BTreeMap::new();
    for (key, value) in inherited {
        let key = key
            .into_string()
            .map_err(|_| anyhow::anyhow!("Desktop environment key is not UTF-8"))?;
        let value = value
            .into_string()
            .map_err(|_| anyhow::anyhow!("Desktop environment value is not UTF-8"))?;
        environment.insert(key, value);
    }
    for key in unset_environment {
        environment.insert(key.clone(), String::new());
    }
    Ok(environment)
}

#[cfg(test)]
#[path = "macos_desktop_tests.rs"]
mod desktop_launch_tests;

async fn activate_app(target: ClientTarget, arguments: &str) -> Result<u32> {
    let executable = desktop_executable(target)?;
    let target_args = arguments
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let codex_home = crate::config::ClientPaths::default_codex_home()?;
    let claude_environment = if target == ClientTarget::ClaudeDesktop {
        Some(crate::network_proxy::child_proxy_environment(
            "http://127.0.0.1",
        )?)
    } else {
        None
    };
    spawn_desktop_detached(&executable, &target_args, |command| {
        command
            .env_remove("OPENAI_BASE_URL")
            .env_remove("OPENAI_API_KEY");
        if target == ClientTarget::ClaudeDesktop {
            for (name, _) in CLAUDE_GATEWAY_MODEL_POLICY {
                command.env_remove(name);
            }
            for name in [
                "ANTHROPIC_BASE_URL",
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_AUTH_TOKEN",
                "CLAUDE_CODE_OAUTH_TOKEN",
                "CLAUDE_GATEWAY_ALLOW_LOOPBACK",
                "CLAUDE_USER_DATA_DIR",
                "CLAUDE_E2E_MANAGED_PLIST",
            ] {
                command.env_remove(name);
            }
            if let Some(values) = claude_environment.as_deref() {
                for (name, value) in values {
                    command.env(name, value);
                }
            }
        }
        if target == ClientTarget::CodexDesktop {
            // Account restore uses this home. Do not inherit an unrelated CLI or
            // gateway profile from the process that launched Little-Mungbean.
            configure_codex_desktop_environment(command, &codex_home, None);
        }
    })
    .await
}
pub async fn activate_store_codex(arguments: &str) -> Result<u32> {
    activate_app(ClientTarget::CodexDesktop, arguments).await
}
pub async fn activate_store_claude() -> Result<u32> {
    activate_app(ClientTarget::ClaudeDesktop, "").await
}
pub async fn activate_claude_gateway() -> Result<u32> {
    activate_store_claude().await
}
pub fn read_codex_bundled_catalog(home: &Path) -> Result<String> {
    let executable = find_executable("codex").context("Codex CLI is not installed")?;
    let output = codex_bundled_catalog_command(&executable, home)?
        .output()
        .with_context(|| {
            format!(
                "Unable to read Codex bundled model catalog: {}",
                executable.display()
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Codex 自带模型目录读取失败（退出码 {:?}）：{}",
            output.status.code(),
            stderr.trim().chars().take(500).collect::<String>()
        );
    }
    String::from_utf8(output.stdout).context("Codex 自带模型目录不是 UTF-8 JSON")
}

fn codex_bundled_catalog_command(executable: &Path, home: &Path) -> Result<Command> {
    let mut command = Command::new(executable);
    // GUI apps do not inherit the shell PATH; npm's Codex entry uses env node.
    command
        .args(["debug", "models", "--bundled"])
        .env("CODEX_HOME", home)
        .env("PATH", executable_search_path(executable)?);
    Ok(command)
}
pub fn set_user_environment(_values: &[(&str, &str)]) -> Result<()> {
    Ok(())
}
pub fn verify_authenticode(path: &Path) -> Result<()> {
    let output = Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(path)
        .output()?;
    if !output.status.success() {
        bail!("The macOS application signature could not be verified")
    }
    Ok(())
}
pub fn open_installer_package(path: &Path) -> Result<()> {
    if !Command::new("/usr/bin/open").arg(path).status()?.success() {
        bail!("Unable to open the installer")
    }
    Ok(())
}
pub fn open_directory(path: &Path) -> Result<()> {
    if !path.is_dir() {
        anyhow::bail!("Configuration directory does not exist: {}", path.display());
    }
    if !Command::new("/usr/bin/open").arg(path).status()?.success() {
        anyhow::bail!("Unable to open the configuration directory");
    }
    Ok(())
}
pub fn local_time(ms: i64) -> Option<libc::tm> {
    let seconds = ms.div_euclid(1000) as libc::time_t;
    let mut result = std::mem::MaybeUninit::<libc::tm>::uninit();
    if unsafe { libc::localtime_r(&seconds, result.as_mut_ptr()) }.is_null() {
        None
    } else {
        Some(unsafe { result.assume_init() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_desktop_takeover_clears_foreign_app_server_environment() {
        for gateway in [Some(("http://127.0.0.1:54321/", "local-token")), None] {
            let mut command = tokio::process::Command::new("unused-test-executable");
            for key in [
                "OPENAI_BASE_URL",
                "OPENAI_API_KEY",
                "CODEX_APP_SERVER_OPENAI_BASE_URL",
                "CODEX_APP_SERVER_CHATGPT_BASE_URL",
            ] {
                command.env(key, "foreign-route");
            }
            configure_codex_desktop_environment(
                &mut command,
                Path::new("/isolated/codex"),
                gateway,
            );
            let env = command
                .as_std()
                .get_envs()
                .collect::<std::collections::BTreeMap<_, _>>();
            assert_eq!(
                env[std::ffi::OsStr::new("CODEX_HOME")],
                Some(std::ffi::OsStr::new("/isolated/codex"))
            );
            assert_eq!(
                env[std::ffi::OsStr::new("CODEX_APP_SERVER_CHATGPT_BASE_URL")],
                None
            );
            for key in ["OPENAI_BASE_URL", "CODEX_APP_SERVER_OPENAI_BASE_URL"] {
                assert_eq!(
                    env[std::ffi::OsStr::new(key)],
                    gateway.map(|_| std::ffi::OsStr::new("http://127.0.0.1:54321"))
                );
            }
            assert_eq!(
                env[std::ffi::OsStr::new("OPENAI_API_KEY")],
                gateway.map(|_| std::ffi::OsStr::new("local-token"))
            );
        }
    }

    #[test]
    fn completed_zombie_process_does_not_block_terminal_reconnect() {
        let stamp = "Sat Oct 10 08:30:00 2026";
        for state in ["S+", "R", "Ss"] {
            assert_eq!(
                live_process_stamp(&format!(" {stamp} {state}\n")).unwrap(),
                stamp
            );
        }
        for state in ["Z", "Z+", "Zs"] {
            assert!(live_process_stamp(&format!("{stamp} {state}")).is_err());
        }
        for missing in ["", "\n", " Z", "exited"] {
            assert!(live_process_stamp(missing).is_err());
        }
    }

    #[test]
    fn terminal_launch_identity_requires_one_exact_window_and_tty() {
        let window = OwnedTerminalWindow::from_launch("421\n/dev/ttys003\n").unwrap();
        assert_eq!(window.id, 421);
        assert_eq!(window.tty, "/dev/ttys003");
        for invalid in [
            "0\n/dev/ttys003\n",
            "421\n",
            "421\n/dev/ttys\n",
            "421\n/dev/ttys003/other\n",
            "421\n/dev/ttys003; close windows\n",
            "421\nnot a tty\n",
            "421\n/dev/ttys003\n422\n/dev/ttys004\n",
        ] {
            assert!(OwnedTerminalWindow::from_launch(invalid).is_err());
        }
    }

    #[test]
    fn process_scan_excludes_exited_cli_and_preserves_command_paths() {
        let command = "/Users/test/Library/Application Support/tools/codex";
        for state in ["S+", "R", "Ss"] {
            assert_eq!(
                live_process_row(&format!(" 101  1  502  {state}  {command}")),
                Some((101, 1, 502, command))
            );
        }
        for state in ["Z", "Z+", "Zs"] {
            assert!(live_process_row(&format!("101 1 502 {state} {command}")).is_none());
        }
        for invalid in ["", "101", "101 1", "101 1 502", "bad 1 502 S codex"] {
            assert!(live_process_row(invalid).is_none());
        }
    }

    #[test]
    fn gui_worker_and_gateway_locks_are_independent_and_reusable() {
        let fixture = Fixture::new();
        let names = [
            r"Local\Little-Mungbean-gui-v1",
            r"Local\Little-Mungbean-worker-v3-codex-desktop",
            r"Local\Little-Mungbean-worker-v3-claude-code",
            r"Local\Little-Mungbean-gateway-codex-desktop",
            r"Local\Little-Mungbean-gateway-daemon",
        ];
        let guards: Vec<_> = names
            .iter()
            .map(|name| {
                try_acquire_named_mutex_at(&fixture.0, name)
                    .unwrap()
                    .unwrap()
            })
            .collect();
        assert!(fixture.0.join("gui.lock").is_file());
        for name in names {
            assert!(
                try_acquire_named_mutex_at(&fixture.0, name)
                    .unwrap()
                    .is_none()
            );
        }
        drop(guards);
        for name in names {
            assert!(
                try_acquire_named_mutex_at(&fixture.0, name)
                    .unwrap()
                    .is_some()
            );
        }
        // Hash the complete name, rather than replacing punctuation with underscores.
        assert_ne!(mutex_file_name("a/b"), mutex_file_name("a_b"));
    }

    #[test]
    fn named_mutex_excludes_other_processes_until_released() {
        let fixture = Fixture::new();
        let name = r"Local\Little-Mungbean-worker-v3-codex-desktop";
        let path = fixture.0.join(mutex_file_name(name));
        let script = "import fcntl,sys\nf=open(sys.argv[1], 'a+')\ntry:\n fcntl.flock(f, fcntl.LOCK_EX|fcntl.LOCK_NB)\nexcept BlockingIOError:\n sys.exit(23)\n";
        let probe = || {
            Command::new("/usr/bin/python3")
                .args(["-c", script])
                .arg(&path)
                .status()
                .unwrap()
                .code()
        };
        let guard = try_acquire_named_mutex_at(&fixture.0, name)
            .unwrap()
            .unwrap();
        assert_eq!(probe(), Some(23));
        drop(guard);
        assert_eq!(probe(), Some(0));
    }

    fn classified(image: &str, command: &str) -> Option<ClientTarget> {
        classify_process(Path::new(image), command, |bundle| {
            match bundle.file_name()?.to_str()? {
                "Codex.app" | "ChatGPT.app" | "Renamed Codex.app" => {
                    Some(ClientTarget::CodexDesktop)
                }
                "Claude.app" | "Renamed Claude.app" => Some(ClientTarget::ClaudeDesktop),
                _ => None,
            }
        })
    }

    #[test]
    fn desktop_helpers_and_embedded_clis_belong_to_the_outer_application() {
        for image in [
            "/Applications/Codex.app/Contents/MacOS/Codex",
            "/Applications/ChatGPT.app/Contents/MacOS/ChatGPT",
            "/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex",
            "/Users/test/Applications/Renamed Codex.app/Contents/Frameworks/Codex Helper.app/Contents/MacOS/Codex Helper",
        ] {
            assert_eq!(
                classified(image, image),
                Some(ClientTarget::CodexDesktop),
                "{image}"
            );
        }
        for image in [
            "/Applications/Claude.app/Contents/MacOS/Claude",
            "/Applications/Claude.app/Contents/Resources/claude",
            "/Users/test/Applications/Renamed Claude.app/Contents/Frameworks/Claude Helper (Renderer).app/Contents/MacOS/Claude Helper (Renderer)",
        ] {
            assert_eq!(
                classified(image, image),
                Some(ClientTarget::ClaudeDesktop),
                "{image}"
            );
        }
        assert_eq!(
            classified("/Applications/Other.app/Contents/Resources/codex", "codex"),
            None
        );
    }

    #[test]
    fn unrelated_apps_are_not_opened_during_process_detection() {
        let known = std::collections::HashMap::from([
            (
                PathBuf::from("/Applications/ChatGPT.app"),
                ClientTarget::CodexDesktop,
            ),
            (
                PathBuf::from("/Users/test/Desktop/Renamed Client.app"),
                ClientTarget::ClaudeDesktop,
            ),
        ]);
        for image in [
            "/Users/test/Documents/Other.app/Contents/MacOS/codex",
            "/Users/test/Desktop/Other.app/Contents/Resources/claude",
            "/Users/test/Pictures/Other.app/Contents/MacOS/Other",
            "/Applications/Other.app/Contents/MacOS/Other",
        ] {
            assert_eq!(
                classify_known_process(Path::new(image), image, &known, |_| {
                    panic!("Unrelated application metadata must not be read")
                }),
                None
            );
        }
        for (image, expected) in [
            (
                "/Applications/ChatGPT.app/Contents/Resources/CodexCLI.app/Contents/MacOS/codex",
                ClientTarget::CodexDesktop,
            ),
            (
                "/Users/test/Desktop/Renamed Client.app/Contents/Frameworks/Claude Helper.app/Contents/MacOS/Claude Helper",
                ClientTarget::ClaudeDesktop,
            ),
        ] {
            assert_eq!(
                classify_known_process(Path::new(image), image, &known, |bundle| {
                    known.get(bundle).copied()
                }),
                Some(expected)
            );
            assert_eq!(
                classify_known_process(Path::new(image), image, &known, |_| {
                    Some(ClientTarget::CodexCli)
                }),
                None,
                "A stale/misleading registration must not authorize process control"
            );
        }
        assert_eq!(
            classify_known_process(
                Path::new("/opt/homebrew/bin/codex"),
                "codex",
                &known,
                |_| { panic!("Standalone CLIs must not require desktop metadata") }
            ),
            Some(ClientTarget::CodexCli)
        );
    }

    #[test]
    fn standalone_cli_detection_uses_the_executable_and_node_entrypoint() {
        for (image, command, expected) in [
            (
                "/opt/homebrew/bin/codex",
                "codex",
                Some(ClientTarget::CodexCli),
            ),
            (
                "/Users/test/.local/bin/claude",
                "claude",
                Some(ClientTarget::ClaudeCode),
            ),
            (
                "/Users/test/.local/share/claude/versions/2.1.0",
                "claude",
                Some(ClientTarget::ClaudeCode),
            ),
            (
                "/opt/homebrew/bin/node",
                "/opt/homebrew/bin/node /usr/local/lib/node_modules/@openai/codex/bin/codex.js",
                Some(ClientTarget::CodexCli),
            ),
            (
                "/opt/homebrew/bin/node",
                "/usr/local/bin/node /usr/local/lib/node_modules/@anthropic-ai/claude-code/cli.js",
                Some(ClientTarget::ClaudeCode),
            ),
            (
                "/opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe",
                "claude",
                Some(ClientTarget::ClaudeCode),
            ),
            (
                "/Users/test/.npm-global/lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe",
                "claude",
                Some(ClientTarget::ClaudeCode),
            ),
            ("/tmp/claude.exe", "claude", None),
            (
                "/Applications/Other.app/Contents/Resources/node_modules/@anthropic-ai/claude-code/bin/claude.exe",
                "claude",
                None,
            ),
            ("/bin/sh", "/bin/sh /tmp/codex-notes.command", None),
            ("/bin/sleep", "codex 10", None),
            (
                "/opt/homebrew/bin/node",
                "node --require /tmp/preload.js /usr/local/lib/node_modules/@anthropic-ai/claude-code/cli.js",
                Some(ClientTarget::ClaudeCode),
            ),
            (
                "/opt/homebrew/bin/node",
                "node --eval=1 /usr/local/lib/node_modules/@openai/codex/bin/codex.js",
                None,
            ),
            (
                "/opt/homebrew/bin/node",
                "node /Applications/Other.app/Contents/Resources/node_modules/@openai/codex/bin/codex.js",
                None,
            ),
            (
                "/bin/sh",
                "/bin/sh -c echo /Applications/Codex.app/Contents/MacOS/Codex",
                None,
            ),
            (
                "/usr/bin/python3",
                "python3 /tmp/@openai/codex/bin/codex.js",
                None,
            ),
            (
                "/opt/homebrew/bin/node",
                "/opt/homebrew/bin/node /tmp/server.js /tmp/@anthropic-ai/claude-code/cli.js",
                None,
            ),
            (
                "/opt/homebrew/bin/node",
                "/opt/homebrew/bin/node -e /tmp/@openai/codex/bin/codex.js",
                None,
            ),
        ] {
            assert_eq!(classified(image, command), expected, "{image}: {command}");
        }
    }

    #[test]
    fn codex_app_server_daemon_is_not_an_interactive_cli_session() {
        for image in [
            "/Users/test/Library/Application Support/Little-Mungbean/profiles/codex-cli/packages/app-server-daemon/releases/0.162.1-aarch64-apple-darwin/bin/codex",
            "/Users/test/.codex/packages/app-server-daemon/releases/0.162.0-aarch64-apple-darwin/bin/codex",
        ] {
            assert_eq!(classified(image, image), None);
        }
        assert_eq!(
            classified(
                "/Users/test/.local/bin/codex",
                "/Users/test/.local/bin/codex",
            ),
            Some(ClientTarget::CodexCli)
        );
    }

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "little-mungbean-detection-{}",
                rand::random::<u64>()
            ));
            fs::create_dir_all(&root).unwrap();
            // proc_pidpath resolves /var to /private/var; use the same path
            // identity for fixture bundles and their native process images.
            Self(root.canonicalize().unwrap())
        }
        fn executable(&self, relative: &str) -> PathBuf {
            use std::os::unix::fs::PermissionsExt;
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        }
        fn bundle(&self, name: &str, identifier: &str) -> PathBuf {
            let root = self.0.join(name);
            self.executable(&format!("{name}/Contents/MacOS/Client"));
            fs::write(
                root.join("Contents/Info.plist"),
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>CFBundleIdentifier</key><string>{identifier}</string>
<key>CFBundleExecutable</key><string>Client</string></dict></plist>"#
                ),
            )
            .unwrap();
            root
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn bundled_catalog_resolves_node_with_gui_path() {
        const CHILD: &str = "LITTLE_MUNGBEAN_NODE_PATH_TEST";
        if std::env::var_os(CHILD).is_none() {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "platform::macos_impl::tests::bundled_catalog_resolves_node_with_gui_path",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("PATH", "/usr/bin:/bin")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        assert_eq!(std::env::var("PATH").unwrap(), "/usr/bin:/bin");
        let fixture = Fixture::new();
        let executable = fixture.executable("Node tools/bin/codex");
        let node = fixture.executable("Node tools/bin/node");
        fs::write(&executable, "#!/usr/bin/env node\n").unwrap();
        fs::write(
            &node,
            concat!(
                "#!/bin/sh\n",
                "if [ \"$2\" = --version ]; then printf 'codex-cli 0.1.0\\n'; exit 0; fi\n",
                "[ \"$2\" = debug ] && [ \"$3\" = models ] && [ \"$4\" = --bundled ] || exit 2\n",
                "[ -d \"$CODEX_HOME\" ] || exit 3\n",
                "printf '%s' '{\"models\":[{\"slug\":\"node-path-test\"}]}'\n",
            ),
        )
        .unwrap();
        let original = Command::new(&executable).output().unwrap();
        assert!(!original.status.success());
        assert!(String::from_utf8_lossy(&original.stderr).contains("node"));
        assert!(
            cli_version_works(&executable, "codex"),
            "GUI detection must supply Node's PATH too"
        );
        let output = codex_bundled_catalog_command(&executable, &fixture.0)
            .unwrap()
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let catalog: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(catalog["models"][0]["slug"], "node-path-test");
        assert_eq!(std::env::var("PATH").unwrap(), "/usr/bin:/bin");
    }

    #[test]
    fn bundle_identity_survives_renaming_and_rejects_misleading_names() {
        let fixture = Fixture::new();
        let codex = fixture.bundle("Renamed Client.app", "com.openai.codex");
        let claude = fixture.bundle("Another Client.app", "com.anthropic.claudefordesktop");
        let other = fixture.bundle("Codex.app", "example.unrelated");
        assert_eq!(bundle_target(&codex), Some(ClientTarget::CodexDesktop));
        assert_eq!(bundle_target(&claude), Some(ClientTarget::ClaudeDesktop));
        assert_eq!(bundle_target(&other), None);
        assert!(bundle_executable(&codex).is_some());
        fs::remove_file(codex.join("Contents/MacOS/Client")).unwrap();
        assert!(bundle_executable(&codex).is_none());
    }

    #[test]
    fn cli_installation_rejects_embedded_binaries_symlinks_and_nonexecutables() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let standalone = fixture.executable("bin/codex");
        let embedded = fixture.executable("Desktop.app/Contents/Resources/codex");
        let link = fixture.0.join("codex-link");
        symlink(&embedded, &link).unwrap();
        assert!(cli_executable_candidate(&standalone, "codex"));
        assert!(!cli_executable_candidate(&embedded, "codex"));
        assert!(!cli_executable_candidate(&link, "codex"));
        fs::remove_file(&link).unwrap();
        symlink(&standalone, &link).unwrap();
        assert!(cli_executable_candidate(&link, "codex"));
        fs::remove_file(&standalone).unwrap();
        assert!(!cli_executable_candidate(&link, "codex"));
        fs::write(fixture.0.join("not-executable"), b"data").unwrap();
        assert!(!cli_executable_candidate(
            &fixture.0.join("not-executable"),
            "claude"
        ));
    }

    #[test]
    fn cli_probe_rejects_stale_launchers_and_invalid_versions() {
        let fixture = Fixture::new();
        for (name, version) in [
            ("codex", "codex-cli 0.162.1"),
            ("claude", "2.1.296 (Claude Code)"),
        ] {
            let path = fixture.executable(&format!("bin/{name}"));
            fs::write(
                &path,
                format!(
                    "#!/bin/sh\n[ \"$1\" = --version ] || exit 9\nprintf '%s\\n' '{version}'\n"
                ),
            )
            .unwrap();
            assert!(cli_version_works(&path, name));
            // Replacing a cached launcher with an empty shim must invalidate it.
            fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
            assert!(!cli_version_works(&path, name));
            fs::write(
                &path,
                format!("#!/bin/sh\nprintf '%s\\n' '{version}'\nexit 127\n"),
            )
            .unwrap();
            assert!(!cli_version_works(&path, name));
            fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' '{version}'\n")).unwrap();
            assert!(cli_version_works(&path, name));
            fs::remove_file(&path).unwrap();
            assert!(!cli_version_works(&path, name));
        }
        for (name, output) in [
            ("codex", "unrelated 1.2.3"),
            ("claude", "codex-cli 1.2.3"),
            ("codex", "codex-cli unknown"),
        ] {
            assert!(!valid_cli_version(name, output));
        }
        assert!(valid_cli_version("codex", "codex-cli 1.2.3-beta.1"));
    }

    #[test]
    fn cli_probe_times_out_without_waiting_for_a_hung_launcher() {
        let fixture = Fixture::new();
        let path = fixture.executable("bin/claude");
        fs::write(&path, "#!/bin/sh\n/bin/sleep 20 &\nwait\n").unwrap();
        let started = Instant::now();
        assert!(!probe_cli_version(
            &path,
            "claude",
            Duration::from_millis(100)
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn cli_detection_reports_current_usable_installations() {
        for name in ["codex", "claude"] {
            let executable = find_executable(name);
            if let Some(path) = &executable {
                assert!(cli_executable_candidate(path, name));
                assert!(probe_cli_version(path, name, Duration::from_secs(3)));
            }
            println!(
                "{}",
                serde_json::json!({"client": name, "installed": executable.is_some(), "executable": executable})
            );
        }
    }

    #[test]
    fn real_process_status_tracks_launch_exit_and_ignores_orphan_helpers() {
        struct TestChild(std::process::Child);
        impl Drop for TestChild {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let fixture = Fixture::new();
        let codex_bundle = fixture.bundle("Renamed Codex.app", "com.openai.codex");
        let claude_bundle = fixture.bundle("Renamed Claude.app", "com.anthropic.claudefordesktop");
        // Supply the identities a real installed app gets from LaunchServices;
        // test bundles must not be registered in the user's system database.
        let known = std::collections::HashMap::from([
            (codex_bundle.clone(), ClientTarget::CodexDesktop),
            (claude_bundle.clone(), ClientTarget::ClaudeDesktop),
        ]);
        let running_fixture_processes = || {
            processes_for_bundles(&known)
                .unwrap()
                .into_iter()
                .filter_map(|p| p.target.map(|target| (target, p.pid)))
                .collect::<Vec<_>>()
        };
        let images = [
            (fixture.0.join("bin/codex"), ClientTarget::CodexCli),
            (fixture.0.join("bin/claude"), ClientTarget::ClaudeCode),
            (
                codex_bundle.join("Contents/MacOS/Client"),
                ClientTarget::CodexDesktop,
            ),
            (
                claude_bundle.join("Contents/MacOS/Client"),
                ClientTarget::ClaudeDesktop,
            ),
            (
                codex_bundle.join("Contents/Resources/CodexCLI.app/Contents/MacOS/codex"),
                ClientTarget::CodexDesktop,
            ),
            (
                claude_bundle
                    .join("Contents/Frameworks/Claude Helper.app/Contents/MacOS/Claude Helper"),
                ClientTarget::ClaudeDesktop,
            ),
        ];
        // Build a native fixture: scripts appear as /bin/sh in proc_pidpath,
        // while relocated Apple platform binaries can be killed by code signing.
        let source = fixture.0.join("process.c");
        let executable = fixture.0.join("test-process");
        fs::write(
            &source,
            "#include <unistd.h>\nint main(void) { sleep(30); return 0; }\n",
        )
        .unwrap();
        let compiled = Command::new("/usr/bin/clang")
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        // Finish writing bundles before launching them: macOS protects running apps.
        for (image, _) in &images {
            fs::create_dir_all(image.parent().unwrap()).unwrap();
            fs::copy(&executable, image).unwrap();
        }
        let daemon_image = fixture
            .0
            .join("profiles/codex-cli/packages/app-server-daemon/releases/test/bin/codex");
        fs::create_dir_all(daemon_image.parent().unwrap()).unwrap();
        fs::copy(&executable, &daemon_image).unwrap();
        let daemon = TestChild(Command::new(&daemon_image).spawn().unwrap());
        assert!(process_matches_image(daemon.0.id(), &daemon_image));
        let mut children = Vec::new();
        for (image, target) in &images {
            let child = TestChild(Command::new(image).arg("30").spawn().unwrap());
            assert!(process_matches_image(child.0.id(), image));
            children.push((child, *target));
        }
        let running = running_fixture_processes();
        assert!(running.iter().all(|(_, pid)| *pid != daemon.0.id()));
        for (child, target) in &children {
            assert!(
                running.contains(&(*target, child.0.id())),
                "missing {target:?}"
            );
        }
        for index in [2, 3] {
            children[index].0.0.kill().unwrap();
            children[index].0.0.wait().unwrap();
        }
        let running = running_fixture_processes();
        for (index, (child, target)) in children.iter_mut().enumerate() {
            let status = child.0.try_wait().unwrap();
            assert_eq!(
                running.contains(&(*target, child.0.id())),
                index < 2,
                "{target:?} pid={} exit={status:?}",
                child.0.id()
            );
        }
        // Keep the exited CLI unreaped, as Terminal can do after closing its
        // window. Status scanning must agree with the restart identity check.
        children[0].0.0.kill().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while identity(children[0].0.0.id()).is_ok() {
            assert!(Instant::now() < deadline, "fixture CLI did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            running_fixture_processes()
                .iter()
                .all(|(_, pid)| *pid != children[0].0.0.id())
        );
        for (child, _) in &mut children {
            let _ = child.0.kill();
            let _ = child.0.wait();
        }
        let running = running_fixture_processes();
        assert!(
            children
                .iter()
                .all(|(child, target)| !running.contains(&(*target, child.0.id())))
        );
    }

    #[test]
    fn installed_and_running_statuses_use_the_same_desktop_identity() {
        // These are read-only queries against LaunchServices and real process images.
        // This test also produces reviewable local status without exposing arguments.
        let running = running_client_processes().unwrap();
        for target in ClientTarget::ALL {
            let path = match target {
                ClientTarget::CodexCli => find_executable("codex"),
                ClientTarget::ClaudeCode => find_executable("claude"),
                _ => desktop_executable(target).ok(),
            };
            println!(
                "{}",
                serde_json::json!({
                    "client": target.id(), "installed": path.is_some(), "executable": path,
                    "pids": running.iter().filter_map(|(client, pid)| (*client == target).then_some(*pid)).collect::<Vec<_>>()
                })
            );
            if let Some(path) = path {
                assert!(executable_file(&path));
                if matches!(
                    target,
                    ClientTarget::CodexDesktop | ClientTarget::ClaudeDesktop
                ) {
                    assert_eq!(
                        bundle_target(&outer_app_bundle(&path).unwrap()),
                        Some(target)
                    );
                }
            }
        }
    }

    #[test]
    fn keychain_encryption_authenticates_nonce_ciphertext_and_format() {
        let secret = b"temporary-macos-test-secret";
        let encrypted = protect_or_unprotect(secret, true).unwrap();
        let second = protect_or_unprotect(secret, true).unwrap();
        assert_ne!(encrypted, second, "each record must have a fresh nonce");
        assert!(!encrypted.windows(secret.len()).any(|bytes| bytes == secret));
        assert_eq!(protect_or_unprotect(&encrypted, false).unwrap(), secret);

        for index in [0, SECRET_MAGIC.len(), encrypted.len() - 1] {
            let mut modified = encrypted.clone();
            modified[index] ^= 1;
            assert!(protect_or_unprotect(&modified, false).is_err());
        }
        for invalid in [
            b"Windows-DPAPI-data".as_slice(),
            &encrypted[..encrypted.len() - 1],
        ] {
            assert!(protect_or_unprotect(invalid, false).is_err());
        }
    }

    #[test]
    fn shell_arguments_do_not_execute_interpolation() {
        let value = "name ' with $() and `code`";
        let output = Command::new("/bin/sh")
            .args(["-c", &format!("printf %s {}", shell_quote(value))])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap(), value);
    }

    #[test]
    fn terminal_command_replaces_shell_and_quotes_script_path() {
        let fixture = Fixture::new();
        let script = fixture.0.join("CLI ' 中文 $(touch unintended).command");
        fs::write(&script, "#!/bin/sh\nprintf '%s\\n' \"$$\"\n").unwrap();
        let child = Command::new("/bin/sh")
            .args(["-c", &terminal_exec_command(&script)])
            .current_dir(&fixture.0)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            pid.to_string()
        );
        assert!(!fixture.0.join("unintended").exists());
    }

    #[test]
    fn app_server_status_only_refreshes_a_running_daemon() {
        assert!(app_server_is_running(br#"{"status":"running"}"#).unwrap());
        assert!(!app_server_is_running(br#"{"status":"stopped"}"#).unwrap());
        assert!(app_server_is_running(b"not json").is_err());
    }

    #[test]
    fn codex_daemon_refresh_skips_profiles_without_a_control_socket() {
        let root =
            std::env::temp_dir().join(format!("mungbean-daemon-{:016x}", rand::random::<u64>()));
        fs::create_dir_all(&root).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert!(
            !runtime
                .block_on(refresh_codex_cli_app_server(&root, &[]))
                .unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }
}
