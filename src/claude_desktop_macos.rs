//! macOS uses Claude's local configuration library, never Windows registry or
//! /Library/Managed Preferences. Account data in Application Support/Claude is
//! deliberately outside this adapter's write set.
use crate::{backup, gui_settings, i18n};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    fs, io,
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

const OWNED_ID: &str = "b7b73721-4147-4b55-92d6-78b68b173e52";
const MAGIC: &[u8] = b"ASCLAUDEMAC1\0";
pub const MANAGED_VALUES: [&str; 6] = [
    "inferenceGatewayBaseUrl",
    "inferenceGatewayApiKey",
    "inferenceProvider",
    "inferenceCredentialKind",
    "inferenceModels",
    "modelDiscoveryEnabled",
];

#[derive(Debug, Clone)]
pub struct RegistryStatus {
    pub policy_key_exists: bool,
    pub managed_values_present: Vec<String>,
    pub pending_backup: bool,
    pub backup_path: PathBuf,
}
#[derive(Serialize, Deserialize)]
struct Snapshot {
    version: u32,
    applied_id: Option<Value>,
    hybrid_pointer: Option<Value>,
    deployment_mode: Option<Value>,
}
struct Paths {
    meta: PathBuf,
    config: PathBuf,
    owned: PathBuf,
}
impl Paths {
    fn at(root: &Path) -> Self {
        Self {
            meta: root.join("configLibrary/_meta.json"),
            config: root.join("claude_desktop_config.json"),
            owned: root.join("configLibrary").join(format!("{OWNED_ID}.json")),
        }
    }
}
fn paths() -> Result<Paths> {
    let home = std::env::var_os("HOME").context("Cannot determine the macOS user home")?;
    let root = PathBuf::from(home).join("Library/Application Support/Claude-3p");
    let paths = Paths::at(&root);
    // A custom symlink could otherwise redirect this adapter into account data.
    for path in [
        &root,
        &root.join("configLibrary"),
        &paths.meta,
        &paths.config,
        &paths.owned,
    ] {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("Claude Desktop configuration uses a symlink; takeover was skipped")
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).context("Cannot inspect Claude Desktop configuration location");
            }
        }
    }
    Ok(paths)
}

pub fn configuration_directory() -> Result<PathBuf> {
    Ok(paths()?
        .owned
        .parent()
        .context("Invalid Claude Desktop configuration path")?
        .to_path_buf())
}

pub fn connection_details() -> Result<Option<(String, String)>> {
    connection_details_at(&paths()?)
}
fn connection_details_at(paths: &Paths) -> Result<Option<(String, String)>> {
    let meta = read_object(&paths.meta)?;
    if meta.get("appliedId").and_then(Value::as_str) != Some(OWNED_ID) {
        return Ok(None);
    }
    let object = read_object(&paths.owned)?;
    if object.get("inferenceProvider").and_then(Value::as_str) != Some("gateway") {
        return Ok(None);
    }
    let url = object
        .get("inferenceGatewayBaseUrl")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let token = object
        .get("inferenceGatewayApiKey")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if token.is_empty()
        || !url::Url::parse(url).ok().is_some_and(|parsed| {
            matches!(
                parsed.host_str(),
                Some("127.0.0.1" | "localhost" | "::1" | "[::1]")
            )
        })
    {
        return Ok(None);
    }
    Ok(Some((url.to_owned(), token.to_owned())))
}
pub fn backup_path() -> Result<PathBuf> {
    Ok(crate::platform_paths::data_root()?.join("claude-desktop/macos-config-backup.enc"))
}
fn read_object(path: &Path) -> Result<Map<String, Value>> {
    match fs::read(path) {
        Ok(raw) => serde_json::from_slice::<Value>(&raw)
            .context("Invalid Claude Desktop configuration JSON")?
            .as_object()
            .cloned()
            .context("Claude Desktop configuration must be a JSON object"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Map::new()),
        Err(error) => Err(error).context("Cannot read Claude Desktop configuration"),
    }
}
fn write_object(path: &Path, value: &Map<String, Value>) -> Result<()> {
    backup::atomic_write(path, &serde_json::to_vec_pretty(value)?)
}
fn owned_gateway(object: &Map<String, Value>) -> bool {
    object.get("inferenceProvider").and_then(Value::as_str) == Some("gateway")
        && object
            .get("inferenceGatewayBaseUrl")
            .and_then(Value::as_str)
            .and_then(|url| url::Url::parse(url).ok())
            .is_some_and(|url| url.scheme() == "http" && url.host_str() == Some("127.0.0.1"))
}
fn snapshot_at(paths: &Paths) -> Result<Snapshot> {
    // Never overwrite a file just because it uses our reserved identifier.
    if paths.owned.exists() {
        bail!(
            "The Little-Mungbean Claude configuration slot is already occupied; restore its pending backup first"
        );
    }
    let meta = read_object(&paths.meta)?;
    let config = read_object(&paths.config)?;
    Ok(Snapshot {
        version: 1,
        applied_id: meta.get("appliedId").cloned(),
        hybrid_pointer: meta.get("hybridPointer").cloned(),
        deployment_mode: config.get("deploymentMode").cloned(),
    })
}
fn load_snapshot() -> Result<Option<Snapshot>> {
    let encrypted = match fs::read(backup_path()?) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Cannot read Claude Desktop encrypted backup"),
    };
    if !encrypted.starts_with(MAGIC) {
        bail!("Invalid Claude Desktop macOS backup; current configuration was retained");
    }
    let plain = Zeroizing::new(gui_settings::protect_or_unprotect(
        &encrypted[MAGIC.len()..],
        false,
    )?);
    let snapshot: Snapshot = serde_json::from_slice(&plain)?;
    if snapshot.version != 1 {
        bail!("Unsupported Claude Desktop macOS backup version");
    }
    Ok(Some(snapshot))
}
/// Refuse managed/custom data locations before closing the user's application.
/// Feature detection is tied to the installed bundle, not its visible name.
pub fn preflight() -> Result<()> {
    if std::env::var_os("CLAUDE_USER_DATA_DIR").is_some()
        || std::env::var_os("CLAUDE_E2E_MANAGED_PLIST").is_some()
    {
        bail!(
            "{}",
            i18n::tr(
                "检测到 Claude 自定义数据目录或策略覆盖；为避免改错配置，本轮未接管",
                "A custom Claude data directory or policy override is present; takeover was skipped"
            )
        );
    }
    let mut policies = vec![PathBuf::from(
        "/Library/Managed Preferences/com.anthropic.claudefordesktop.plist",
    )];
    let user = std::process::Command::new("/usr/bin/id")
        .arg("-un")
        .output()
        .context("Cannot check Claude managed preferences")?;
    if !user.status.success() {
        bail!("Cannot check Claude managed preferences");
    }
    let user = String::from_utf8(user.stdout)?;
    let user = user.trim();
    if user.is_empty() || user.contains('/') {
        bail!("Cannot identify the macOS account");
    }
    policies.push(
        PathBuf::from("/Library/Managed Preferences")
            .join(user)
            .join("com.anthropic.claudefordesktop.plist"),
    );
    for policy in policies {
        match fs::symlink_metadata(&policy) {
            Ok(_) => bail!(
                "{}",
                i18n::tr(
                    "Claude Desktop 存在系统受管策略；请先由管理员确认，本程序不会覆盖策略",
                    "Claude Desktop has managed system preferences; this app will not override them"
                )
            ),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("Cannot check Claude managed preferences"),
        }
    }
    let executable =
        crate::platform::desktop_executable(crate::gui_worker::ClientTarget::ClaudeDesktop)?;
    let contents = executable
        .parent()
        .and_then(Path::parent)
        .context("Invalid Claude Desktop bundle")?;
    let resources = fs::read(contents.join("Resources/app.asar"))
        .context("Cannot inspect Claude Desktop configuration support")?;
    for signature in [
        b"configLibrary".as_slice(),
        b"inferenceGatewayBaseUrl".as_slice(),
        b"hybridPointer".as_slice(),
        b"deploymentMode".as_slice(),
    ] {
        if !resources
            .windows(signature.len())
            .any(|bytes| bytes == signature)
        {
            bail!(
                "{}",
                i18n::tr(
                    "此 Claude Desktop 版本未检测到兼容的本地网关配置入口",
                    "This Claude Desktop version has no recognized local gateway configuration entry"
                )
            );
        }
    }
    // Validate before closing Desktop, including recovery of any pending file.
    validate_account_restore()?;
    let paths = paths()?;
    read_object(&paths.meta)?;
    read_object(&paths.config)?;
    if paths.owned.exists()
        && !backup_path()?.exists()
        && !owned_gateway(&read_object(&paths.owned)?)
    {
        bail!(
            "The Little-Mungbean Claude configuration slot is occupied by an unrelated configuration"
        );
    }
    Ok(())
}
pub fn status() -> Result<RegistryStatus> {
    let paths = paths()?;
    let object = read_object(&paths.owned)?;
    let backup_path = backup_path()?;
    Ok(RegistryStatus {
        policy_key_exists: paths.owned.is_file(),
        managed_values_present: MANAGED_VALUES
            .iter()
            .filter(|key| object.contains_key(**key))
            .map(|key| (*key).into())
            .collect(),
        pending_backup: backup_path.is_file(),
        backup_path,
    })
}
pub fn is_little_mungbean_gateway() -> Result<bool> {
    if backup_path()?.is_file() {
        return Ok(true);
    }
    let paths = paths()?;
    Ok(read_object(&paths.meta)?
        .get("appliedId")
        .and_then(Value::as_str)
        == Some(OWNED_ID))
}
pub fn validate_account_restore() -> Result<()> {
    load_snapshot().map(|_| ())
}
pub fn capture_and_store() -> Result<()> {
    if backup_path()?.exists() {
        bail!("A pending Claude Desktop macOS backup must be restored first");
    }
    let snapshot = snapshot_at(&paths()?)?;
    let plain = Zeroizing::new(serde_json::to_vec(&snapshot)?);
    let encrypted = gui_settings::protect(&plain)?;
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&encrypted);
    backup::atomic_write(&backup_path()?, &bytes)
}
fn gateway_object(
    base_url: &str,
    local_token: &str,
    models: &str,
    discovery: Option<bool>,
) -> Result<Map<String, Value>> {
    let url = url::Url::parse(base_url)?;
    if url.scheme() != "http" || url.host_str() != Some("127.0.0.1") || local_token.is_empty() {
        bail!("Claude Desktop temporary gateway must be a token-protected loopback listener");
    }
    let models: Value = serde_json::from_str(models)?;
    if !models.as_array().is_some_and(|models| !models.is_empty()) {
        bail!("Claude Desktop requires a nonempty model list");
    }
    let mut object = json!({"inferenceProvider":"gateway", "inferenceCredentialKind":"static", "inferenceGatewayBaseUrl":base_url, "inferenceGatewayApiKey":local_token, "inferenceModels":models}).as_object().cloned().unwrap();
    if let Some(enabled) = discovery {
        object.insert("modelDiscoveryEnabled".into(), enabled.into());
    }
    Ok(object)
}
pub fn write_temporary_gateway(
    base_url: &str,
    local_token: &str,
    models: &str,
    discovery: Option<bool>,
) -> Result<()> {
    load_snapshot()?.context("Claude Desktop configuration must be backed up before takeover")?;
    let object = gateway_object(base_url, local_token, models, discovery)?;
    let paths = paths()?;
    write_object(&paths.owned, &object)?;
    let mut meta = read_object(&paths.meta)?;
    meta.insert("appliedId".into(), OWNED_ID.into());
    meta.remove("hybridPointer"); // A hybrid pointer takes precedence over appliedId.
    write_object(&paths.meta, &meta)?;
    ensure_third_party_mode()
}
pub fn validate_temporary_gateway(
    base_url: &str,
    local_token: &str,
    models: &str,
    discovery: Option<bool>,
) -> Result<()> {
    let paths = paths()?;
    let expected = gateway_object(base_url, local_token, models, discovery)?;
    if read_object(&paths.owned)? != expected
        || read_object(&paths.meta)?
            .get("appliedId")
            .and_then(Value::as_str)
            != Some(OWNED_ID)
        || read_object(&paths.meta)?.contains_key("hybridPointer")
        || read_object(&paths.config)?
            .get("deploymentMode")
            .and_then(Value::as_str)
            != Some("3p")
    {
        bail!(
            "Claude Desktop macOS gateway configuration was changed or did not persist; takeover was stopped"
        );
    }
    Ok(())
}
pub fn ensure_third_party_mode() -> Result<()> {
    let paths = paths()?;
    if read_object(&paths.meta)?
        .get("appliedId")
        .and_then(Value::as_str)
        != Some(OWNED_ID)
    {
        bail!(
            "Claude Desktop selected configuration changed; reconnect to apply the requested gateway"
        );
    }
    let mut config = read_object(&paths.config)?;
    if config.get("deploymentMode").and_then(Value::as_str) != Some("3p") {
        config.insert("deploymentMode".into(), "3p".into());
        write_object(&paths.config, &config)?;
    }
    Ok(())
}
fn restore_value(object: &mut Map<String, Value>, key: &str, value: &Option<Value>) {
    if let Some(value) = value {
        object.insert(key.into(), value.clone());
    } else {
        object.remove(key);
    }
}
fn restore_at(paths: &Paths, snapshot: &Snapshot) -> Result<()> {
    let mut meta = read_object(&paths.meta)?;
    let owned_file_exists = paths.owned.exists();
    if owned_file_exists && !owned_gateway(&read_object(&paths.owned)?) {
        bail!(
            "The owned Claude configuration file was replaced; it was retained with its recovery backup"
        );
    }
    // Another configuration selected after takeover takes precedence. Preserve
    // unrelated metadata/preferences written while the client was running.
    let owned_selection = meta.get("appliedId").and_then(Value::as_str) == Some(OWNED_ID)
        && !meta.contains_key("hybridPointer");
    // A failed write may already have restored metadata but not deploymentMode.
    let restored_selection = meta.get("appliedId") == snapshot.applied_id.as_ref()
        && meta.get("hybridPointer") == snapshot.hybrid_pointer.as_ref();
    if owned_selection {
        restore_value(&mut meta, "appliedId", &snapshot.applied_id);
        restore_value(&mut meta, "hybridPointer", &snapshot.hybrid_pointer);
        write_object(&paths.meta, &meta)?;
    }
    if owned_selection || (owned_file_exists && restored_selection) {
        let mut config = read_object(&paths.config)?;
        if config.get("deploymentMode").and_then(Value::as_str) == Some("3p") {
            restore_value(&mut config, "deploymentMode", &snapshot.deployment_mode);
            write_object(&paths.config, &config)?;
        }
    }
    if owned_file_exists {
        fs::remove_file(&paths.owned)?;
    }
    Ok(())
}
pub fn restore_pending() -> Result<bool> {
    let Some(snapshot) = load_snapshot()? else {
        return Ok(false);
    };
    restore_at(&paths()?, &snapshot)?;
    fs::remove_file(backup_path()?)?;
    Ok(true)
}
pub fn clear_stale_gateway_policy() -> Result<()> {
    if backup_path()?.exists() {
        bail!("Restore the pending Claude Desktop backup before clearing a stale gateway");
    }
    let paths = paths()?;
    if !paths.owned.exists()
        && read_object(&paths.meta)?
            .get("appliedId")
            .and_then(Value::as_str)
            != Some(OWNED_ID)
    {
        return Ok(());
    }
    if paths.owned.exists() && !owned_gateway(&read_object(&paths.owned)?) {
        bail!("The reserved Claude configuration slot contains an unrelated configuration");
    }
    // With no snapshot, detach only our reserved configuration, not other
    // programs' loopback routes, and leave their selected profiles untouched.
    restore_at(
        &paths,
        &Snapshot {
            version: 1,
            applied_id: None,
            hybrid_pointer: None,
            deployment_mode: Some("1p".into()),
        },
    )
}
pub fn restore_account_mode() -> Result<()> {
    if !restore_pending()? {
        clear_stale_gateway_policy()?;
    }
    // Account mode is an explicit user choice, even if the pre-takeover profile
    // belonged to another third-party gateway. Keep that profile on disk but
    // select first-party mode so Claude reopens its existing account data.
    let paths = paths()?;
    let mut config = read_object(&paths.config)?;
    config.insert("deploymentMode".into(), "1p".into());
    write_object(&paths.config, &config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_details_only_returns_the_active_owned_loopback_gateway() {
        let fixture = Fixture::new();
        let paths = Paths::at(&fixture.0);
        assert!(connection_details_at(&paths).unwrap().is_none());
        let mut object = gateway_object(
            "http://127.0.0.1:12345",
            "test-token",
            "[{\"name\":\"test-model\"}]",
            None,
        )
        .unwrap();
        write_object(&paths.owned, &object).unwrap();
        assert!(connection_details_at(&paths).unwrap().is_none());
        write_object(
            &paths.meta,
            json!({"appliedId": OWNED_ID}).as_object().unwrap(),
        )
        .unwrap();
        assert_eq!(
            connection_details_at(&paths).unwrap(),
            Some(("http://127.0.0.1:12345".into(), "test-token".into()))
        );
        object.insert(
            "inferenceGatewayBaseUrl".into(),
            "https://remote.example".into(),
        );
        write_object(&paths.owned, &object).unwrap();
        assert!(connection_details_at(&paths).unwrap().is_none());
    }
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "little-mungbean-claude-config-{:016x}",
                rand::random::<u64>()
            )))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn gateway_restore_preserves_account_data_and_new_preferences() {
        let fixture = Fixture::new();
        let account = fixture.0.join("Claude");
        let paths = Paths::at(&fixture.0.join("Claude-3p"));
        fs::create_dir_all(&account).unwrap();
        fs::write(account.join("auth.json"), "account sentinel").unwrap();
        write_object(&paths.meta, json!({"appliedId":"prior", "hybridPointer":"https://policy.example", "entries":["prior"]}).as_object().unwrap()).unwrap();
        write_object(
            &paths.config,
            json!({"deploymentMode":"1p", "preferences":{"theme":"dark"}})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        let snapshot = snapshot_at(&paths).unwrap();
        write_object(
            &paths.owned,
            &gateway_object(
                "http://127.0.0.1:12345",
                "test-token",
                "[{\"name\":\"claude-sonnet-4-5\"}]",
                Some(false),
            )
            .unwrap(),
        )
        .unwrap();
        write_object(
            &paths.meta,
            json!({"appliedId":OWNED_ID, "entries":["prior","new"]})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        write_object(
            &paths.config,
            json!({"deploymentMode":"3p", "preferences":{"theme":"light"}})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        restore_at(&paths, &snapshot).unwrap();
        let meta = read_object(&paths.meta).unwrap();
        assert_eq!(meta["appliedId"], "prior");
        assert_eq!(meta["entries"], json!(["prior", "new"]));
        assert_eq!(meta["hybridPointer"], "https://policy.example");
        let config = read_object(&paths.config).unwrap();
        assert_eq!(config["deploymentMode"], "1p");
        assert_eq!(config["preferences"]["theme"], "light");
        assert_eq!(
            fs::read_to_string(account.join("auth.json")).unwrap(),
            "account sentinel"
        );
        assert!(!paths.owned.exists());
        restore_at(&paths, &snapshot).unwrap(); // Retry after partial cleanup is safe.
    }
    #[test]
    fn unrelated_config_selection_is_preserved() {
        let fixture = Fixture::new();
        let paths = Paths::at(&fixture.0);
        let snapshot = snapshot_at(&paths).unwrap();
        write_object(
            &paths.meta,
            json!({"appliedId":"another", "hybridPointer":"https://other.example"})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        write_object(
            &paths.config,
            json!({"deploymentMode":"3p"}).as_object().unwrap(),
        )
        .unwrap();
        restore_at(&paths, &snapshot).unwrap();
        assert_eq!(read_object(&paths.meta).unwrap()["appliedId"], "another");
        assert_eq!(read_object(&paths.config).unwrap()["deploymentMode"], "3p");
        assert!(gateway_object("https://example.com", "test", "[]", None).is_err());
    }

    #[test]
    fn partial_restore_can_retry_and_reserved_file_conflicts_fail_safely() {
        let fixture = Fixture::new();
        let paths = Paths::at(&fixture.0);
        let snapshot = snapshot_at(&paths).unwrap();
        write_object(
            &paths.owned,
            &gateway_object(
                "http://127.0.0.1:12345",
                "test",
                "[\"claude-sonnet-4-5\"]",
                None,
            )
            .unwrap(),
        )
        .unwrap();
        write_object(
            &paths.config,
            json!({"deploymentMode":"3p", "keep":true})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        // Metadata has already returned to the snapshot, deploymentMode has not.
        restore_at(&paths, &snapshot).unwrap();
        let config = read_object(&paths.config).unwrap();
        assert!(!config.contains_key("deploymentMode"));
        assert_eq!(config["keep"], true);
        write_object(
            &paths.owned,
            json!({"inferenceProvider":"anthropic"})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        write_object(
            &paths.meta,
            json!({"appliedId":OWNED_ID}).as_object().unwrap(),
        )
        .unwrap();
        assert!(restore_at(&paths, &snapshot).is_err());
        assert!(snapshot_at(&paths).is_err());
        assert_eq!(read_object(&paths.meta).unwrap()["appliedId"], OWNED_ID);
        assert!(paths.owned.exists());
    }
}
