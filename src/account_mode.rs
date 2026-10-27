//! Track only the configuration fields taken over by Little-Mungbean. Account
//! credentials are encrypted in the snapshot and never copied over a newer login.
use crate::{backup, config::ClientPaths, gui_worker::ClientTarget, i18n};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use toml_edit::{DocumentMut, Item};
use zeroize::Zeroizing;

const MARKER: &str = "little-mungbean-account-mode.dpapi";

#[derive(Default, Serialize, Deserialize)]
struct Snapshot {
    files: BTreeMap<String, SavedFile>,
}
#[derive(Serialize, Deserialize)]
struct SavedFile {
    before: Option<String>,
    written: String,
}

pub fn paths(target: ClientTarget) -> Result<ClientPaths> {
    if target == ClientTarget::CodexCli {
        ClientPaths::launcher_codex_cli_profile()
    } else {
        ClientPaths::discover(None, None)
    }
}
fn marker(paths: &ClientPaths, target: ClientTarget) -> PathBuf {
    match target {
        ClientTarget::ClaudeCode => paths.claude_home.join(MARKER),
        _ => paths.codex_home.join(MARKER),
    }
}
fn optional_text(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(None),
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context(i18n::tr(
            "无法读取客户端配置",
            "Unable to read client configuration",
        )),
    }
}
fn document(raw: Option<&str>) -> Result<DocumentMut> {
    raw.unwrap_or("").parse().context(i18n::tr(
        "客户端配置不是有效 TOML",
        "Client configuration is not valid TOML",
    ))
}
fn object(raw: Option<&str>) -> Result<Map<String, Value>> {
    let value: Value = serde_json::from_str(raw.unwrap_or("{}")).context(i18n::tr(
        "客户端配置不是有效 JSON",
        "Client configuration is not valid JSON",
    ))?;
    value.as_object().cloned().context(i18n::tr(
        "客户端配置必须是 JSON 对象",
        "Client configuration must be a JSON object",
    ))
}
fn loopback(value: Option<&str>) -> bool {
    value
        .and_then(|value| url::Url::parse(value).ok())
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1" | "[::1]"))
}
fn codex_gateway(doc: &DocumentMut) -> bool {
    loopback(doc.get("openai_base_url").and_then(Item::as_str))
        || doc
            .get("model_provider")
            .and_then(Item::as_str)
            .is_some_and(|id| {
                loopback(
                    doc.get("model_providers")
                        .and_then(|item| item.get(id))
                        .and_then(|item| item.get("base_url"))
                        .and_then(Item::as_str),
                )
            })
}
pub fn is_managed(target: ClientTarget) -> Result<bool> {
    if target == ClientTarget::ClaudeDesktop {
        return crate::claude_desktop_registry::is_little_mungbean_gateway();
    }
    is_managed_at(&paths(target)?, target)
}

fn is_managed_at(paths: &ClientPaths, target: ClientTarget) -> Result<bool> {
    if marker(paths, target).is_file() {
        return Ok(true);
    }
    if target == ClientTarget::ClaudeCode {
        let settings = object(optional_text(&paths.claude_settings)?.as_deref())?;
        Ok(settings
            .get("env")
            .and_then(|v| v.get("ANTHROPIC_BASE_URL"))
            .and_then(Value::as_str)
            .is_some_and(|url| loopback(Some(url)))
            && settings
                .get("env")
                .and_then(|v| v.get("CLAUDE_GATEWAY_ALLOW_LOOPBACK"))
                .and_then(Value::as_str)
                == Some("1"))
    } else {
        let doc = document(optional_text(&paths.codex_config)?.as_deref())?;
        let auth = object(optional_text(&paths.codex_auth)?.as_deref())?;
        let local_credential = auth
            .get("OPENAI_API_KEY")
            .and_then(Value::as_str)
            .is_some_and(|key| key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit()));
        Ok(codex_gateway(&doc)
            && (local_credential
                || paths.codex_catalog.is_file()
                || doc.get("model_provider").and_then(Item::as_str) == Some("agent_gateway")))
    }
}
fn load(path: &Path) -> Result<Snapshot> {
    if !path.is_file() {
        return Ok(Snapshot::default());
    }
    let encrypted = fs::read(path)?;
    let plain = Zeroizing::new(crate::gui_settings::protect_or_unprotect(
        &encrypted, false,
    )?);
    serde_json::from_slice(&plain).context(i18n::tr(
        "账号模式备份已损坏",
        "The account-mode backup is damaged",
    ))
}
fn save(path: &Path, snapshot: &Snapshot) -> Result<()> {
    let plain = Zeroizing::new(serde_json::to_vec(snapshot)?);
    backup::atomic_write(path, &crate::gui_settings::protect(&plain)?)
}
fn tracked(paths: &ClientPaths, target: ClientTarget, path: &Path) -> bool {
    if target == ClientTarget::ClaudeCode {
        path == paths.claude_settings || path == paths.claude_model_cache
    } else {
        path == paths.codex_config || path == paths.codex_auth || path == paths.codex_catalog
    }
}

fn official_cli_account() -> Result<Option<String>> {
    let official = ClientPaths::discover(None, None)?;
    let mut auth = object(optional_text(&official.codex_auth)?.as_deref())?;
    if !auth.get("tokens").is_some_and(|tokens| !tokens.is_null()) {
        let snapshot = load(&marker(&official, ClientTarget::CodexDesktop))?;
        if let Some(saved) = snapshot
            .files
            .get(official.codex_auth.to_string_lossy().as_ref())
        {
            auth = object(saved.before.as_deref())?;
        }
    }
    auth.remove("OPENAI_API_KEY");
    if auth.get("tokens").is_some_and(|tokens| !tokens.is_null()) {
        auth.insert("auth_mode".into(), Value::String("chatgpt".into()));
        Ok(Some(serde_json::to_string_pretty(&auth)?))
    } else {
        Ok(None)
    }
}

/// Persist the first pre-gateway version and the latest owned values before any
/// write. A failed/partial write remains recoverable and reconnect never replaces
/// the original account snapshot with another gateway configuration.
pub fn apply_gateway_files(
    paths: &ClientPaths,
    target: ClientTarget,
    files: Vec<(PathBuf, Vec<u8>)>,
) -> Result<()> {
    let marker = marker(paths, target);
    let mut snapshot = load(&marker)?;
    let legacy = snapshot.files.is_empty() && is_managed_at(paths, target)?;
    for (path, bytes) in &files {
        if !tracked(paths, target, path) {
            continue;
        }
        let id = path.to_string_lossy().into_owned();
        let mut before = optional_text(path)?;
        if legacy {
            before = clean_legacy(paths, target, path, before.as_deref())?;
        }
        if !cfg!(test)
            && target == ClientTarget::CodexCli
            && path == &paths.codex_auth
            && !object(before.as_deref())?
                .get("tokens")
                .is_some_and(|tokens| !tokens.is_null())
        {
            // Seed a first CLI gateway session with the official account login,
            // without sharing Desktop's gateway endpoint or modifying its files.
            if let Some(account) = official_cli_account()? {
                before = Some(account);
            }
        }
        let written = String::from_utf8(bytes.clone())?;
        if path == &paths.codex_auth
            && let Some(saved) = snapshot.files.get_mut(&id)
        {
            let recent = object(before.as_deref())?;
            if recent.get("tokens").is_some_and(|tokens| !tokens.is_null()) {
                let mut original = object(saved.before.as_deref())?;
                for key in ["tokens", "last_refresh"] {
                    if let Some(value) = recent.get(key) {
                        original.insert(key.into(), value.clone());
                    } else {
                        original.remove(key);
                    }
                }
                original.insert("auth_mode".into(), Value::String("chatgpt".into()));
                saved.before = Some(serde_json::to_string_pretty(&original)?);
            }
        }
        snapshot
            .files
            .entry(id)
            .and_modify(|saved| saved.written = written.clone())
            .or_insert(SavedFile { before, written });
    }
    save(&marker, &snapshot)?;
    for (path, bytes) in files {
        backup::atomic_write(&path, &bytes)?;
    }
    Ok(())
}

fn restore_json(
    current: &mut Map<String, Value>,
    before: &Map<String, Value>,
    written: &Map<String, Value>,
) {
    let keys: std::collections::BTreeSet<_> =
        before.keys().chain(written.keys()).cloned().collect();
    for key in keys {
        if before.get(&key) == written.get(&key) {
            continue;
        }
        if let (Some(Value::Object(owned)), Some(Value::Object(now))) =
            (written.get(&key), current.get_mut(&key))
        {
            if before.get(&key).is_none_or(Value::is_object) {
                restore_json(
                    now,
                    before
                        .get(&key)
                        .and_then(Value::as_object)
                        .unwrap_or(&Map::new()),
                    owned,
                );
                if now.is_empty() && !before.contains_key(&key) {
                    current.remove(&key);
                }
            }
        } else if current.get(&key) == written.get(&key) {
            if let Some(value) = before.get(&key) {
                current.insert(key, value.clone());
            } else {
                current.remove(&key);
            }
        }
    }
}
fn restore_toml(current: &mut DocumentMut, before: &DocumentMut, written: &DocumentMut) {
    // Only these fields are owned. Replacing a provider restores only that
    // provider, keeping unrelated providers and user settings intact.
    for key in [
        "model",
        "model_provider",
        "openai_base_url",
        "model_catalog_json",
        "cli_auth_credentials_store",
    ] {
        let old = before.get(key).map(ToString::to_string);
        let owned = written.get(key).map(ToString::to_string);
        if old != owned && current.get(key).map(ToString::to_string) == owned {
            if let Some(value) = before.get(key) {
                current[key] = value.clone();
            } else {
                current.remove(key);
            }
        }
    }
    if let Some(id) = written.get("model_provider").and_then(Item::as_str) {
        let old = before
            .get("model_providers")
            .and_then(|table| table.get(id));
        let owned = written
            .get("model_providers")
            .and_then(|table| table.get(id));
        let now = current
            .get("model_providers")
            .and_then(|table| table.get(id));
        if old.map(ToString::to_string) != owned.map(ToString::to_string)
            && now.map(ToString::to_string) == owned.map(ToString::to_string)
            && let Some(table) = current
                .get_mut("model_providers")
                .and_then(Item::as_table_mut)
        {
            if let Some(old) = old {
                table.insert(id, old.clone());
            } else {
                table.remove(id);
            }
        }
    }
    #[cfg(any(windows, target_os = "macos"))]
    {
        // Takeover also updates account aliases and the selected profile.
        // Restore only values we wrote, retaining later user edits.
        for (section, fields) in [
            (
                "model_providers",
                &[
                    "name",
                    "base_url",
                    "wire_api",
                    "requires_openai_auth",
                    "env_key",
                    "env_key_instructions",
                    "experimental_bearer_token",
                    "gateway_oauth",
                    "auth",
                    "aws",
                    "http_headers",
                    "env_http_headers",
                    "query_params",
                    "model_catalog_url",
                    "request_max_retries",
                    "stream_max_retries",
                    "stream_idle_timeout_ms",
                    "supports_websockets",
                    "supports_standalone_web_search",
                ][..],
            ),
            (
                "profiles",
                &[
                    "model",
                    "model_provider",
                    "openai_base_url",
                    "model_catalog_json",
                    "cli_auth_credentials_store",
                ][..],
            ),
        ] {
            let Some(tables) = written.get(section).and_then(Item::as_table) else {
                continue;
            };
            for (id, owned) in tables.iter() {
                let previous = before.get(section).and_then(|table| table.get(id));
                let Some(now) = current
                    .get_mut(section)
                    .and_then(|table| table.get_mut(id))
                    .and_then(Item::as_table_mut)
                else {
                    continue;
                };
                for key in fields {
                    let old = previous.and_then(|table| table.get(key));
                    let owned = owned.get(key);
                    if old.map(ToString::to_string) != owned.map(ToString::to_string)
                        && now.get(key).map(ToString::to_string) == owned.map(ToString::to_string)
                    {
                        if let Some(old) = old {
                            now.insert(key, old.clone());
                        } else {
                            now.remove(key);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn restore_null_codex_account_tokens(
    now: &mut Map<String, Value>,
    before: &Map<String, Value>,
    written: &Map<String, Value>,
) {
    // Codex may serialize an API-key login with tokens:null, whereas our
    // gateway file omits tokens. Treat that as the same owned gateway state,
    // but only with the exact credential we wrote; never replace a new login.
    if now.get("tokens") == Some(&Value::Null)
        && now
            .get("auth_mode")
            .is_none_or(|mode| mode.is_null() || mode.as_str() == Some("apikey"))
        && written.get("OPENAI_API_KEY").is_some_and(Value::is_string)
        && now.get("OPENAI_API_KEY") == written.get("OPENAI_API_KEY")
        && before.get("tokens").is_some_and(Value::is_object)
    {
        now.remove("tokens");
        now.insert("auth_mode".into(), Value::String("apikey".into()));
        // last_refresh can also be materialized as null by the client.
        if now.get("last_refresh") == Some(&Value::Null) && !written.contains_key("last_refresh") {
            now.remove("last_refresh");
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn codex_account_provider_alias() -> Item {
    // No base_url, env_key, bearer token or headers: Codex chooses its official
    // endpoint from the active account authentication, as for built-in OpenAI.
    let mut provider = toml_edit::Table::new();
    provider["name"] = toml_edit::value("OpenAI");
    provider["wire_api"] = toml_edit::value("responses");
    provider["requires_openai_auth"] = toml_edit::value(true);
    Item::Table(provider)
}

#[cfg(any(windows, target_os = "macos"))]
fn ensure_codex_account_alias(doc: &mut DocumentMut, id: &str) {
    if id == "openai" || id.is_empty() {
        return;
    }
    if doc.get("model_providers").is_none() {
        doc["model_providers"] = Item::Table(toml_edit::Table::new());
    }
    if let Some(providers) = doc.get_mut("model_providers").and_then(Item::as_table_mut) {
        providers.insert(id, codex_account_provider_alias());
    }
}

#[cfg(windows)]
pub(crate) fn ensure_windows_history_aliases(doc: &mut DocumentMut, sessions: &Path) {
    for id in crate::config::historical_provider_ids(sessions) {
        if doc
            .get("model_providers")
            .and_then(|table| table.get(&id))
            .is_none()
        {
            ensure_codex_account_alias(doc, &id);
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn retain_restored_codex_provider_references(
    doc: &mut DocumentMut,
    previous: &DocumentMut,
    written: &DocumentMut,
) {
    // The old session provider is immutable history. Preserve the identifiers
    // owned by this takeover even if the three-way merge removed their tables.
    for source in [previous, written] {
        let Some(id) = source.get("model_provider").and_then(Item::as_str) else {
            continue;
        };
        let provider = doc.get("model_providers").and_then(|table| table.get(id));
        if provider.is_none()
            || loopback(
                provider
                    .and_then(|p| p.get("base_url"))
                    .and_then(Item::as_str),
            )
        {
            ensure_codex_account_alias(doc, id);
        }
    }
    let aliases = written
        .get("model_providers")
        .and_then(Item::as_table)
        .into_iter()
        .flat_map(|table| table.iter())
        .filter(|(_, provider)| provider.get("name").and_then(Item::as_str) == Some("OpenAI"))
        .map(|(id, _)| id.to_owned())
        .collect::<Vec<_>>();
    for id in aliases {
        let current = doc.get("model_providers").and_then(|table| table.get(&id));
        #[cfg(windows)]
        if current.is_none() {
            ensure_codex_account_alias(doc, &id);
            continue;
        }
        if loopback(
            current
                .and_then(|provider| provider.get("base_url"))
                .and_then(Item::as_str),
        ) {
            ensure_codex_account_alias(doc, &id);
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn clear_restored_codex_loopback(doc: &mut DocumentMut) -> bool {
    let selected_profile = doc.get("profile").and_then(Item::as_str).map(str::to_owned);
    let profile_provider = selected_profile
        .as_deref()
        .and_then(|id| doc.get("profiles").and_then(|profiles| profiles.get(id)))
        .filter(|profile| {
            loopback(profile.get("openai_base_url").and_then(Item::as_str))
                || profile
                    .get("model_provider")
                    .and_then(Item::as_str)
                    .is_some_and(|id| {
                        loopback(
                            doc.get("model_providers")
                                .and_then(|providers| providers.get(id))
                                .and_then(|provider| provider.get("base_url"))
                                .and_then(Item::as_str),
                        )
                    })
        })
        .map(|profile| {
            profile
                .get("model_provider")
                .and_then(Item::as_str)
                .unwrap_or("openai")
                .to_owned()
        });
    if !codex_gateway(doc) && profile_provider.is_none() {
        return false;
    }
    // An explicit account-mode switch must not restore a dead local route
    // captured before takeover. Keep unrelated providers and user settings.
    let provider = doc
        .get("model_provider")
        .and_then(Item::as_str)
        .unwrap_or("openai")
        .to_owned();
    ensure_codex_account_alias(doc, &provider);
    if provider == "openai"
        && let Some(providers) = doc.get_mut("model_providers").and_then(Item::as_table_mut)
    {
        providers.remove("openai");
    }
    for key in ["openai_base_url", "model_catalog_json", "model"] {
        doc.remove(key);
    }
    doc["model_provider"] = toml_edit::value("openai");
    if let Some(id) = profile_provider {
        ensure_codex_account_alias(doc, &id);
        if let Some(profile) = selected_profile
            .as_deref()
            .and_then(|id| {
                doc.get_mut("profiles")
                    .and_then(|profiles| profiles.get_mut(id))
            })
            .and_then(Item::as_table_mut)
        {
            profile["model_provider"] = toml_edit::value("openai");
            for key in ["openai_base_url", "model_catalog_json", "model"] {
                profile.remove(key);
            }
        }
    }
    true
}
fn clean_legacy(
    paths: &ClientPaths,
    target: ClientTarget,
    path: &Path,
    raw: Option<&str>,
) -> Result<Option<String>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if path == paths.codex_config {
        let mut doc = document(Some(raw))?;
        #[cfg(any(windows, target_os = "macos"))]
        if clear_restored_codex_loopback(&mut doc) {
            doc.remove("cli_auth_credentials_store");
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        if codex_gateway(&doc) {
            let id = doc
                .get("model_provider")
                .and_then(Item::as_str)
                .unwrap_or("openai")
                .to_owned();
            if let Some(table) = doc.get_mut("model_providers").and_then(Item::as_table_mut) {
                table.remove(&id);
            }
            for key in [
                "openai_base_url",
                "model_catalog_json",
                "model",
                "cli_auth_credentials_store",
            ] {
                doc.remove(key);
            }
            doc["model_provider"] = toml_edit::value("openai");
        }
        return Ok(Some(doc.to_string()));
    }
    if path == paths.codex_auth {
        let mut auth = object(Some(raw))?;
        auth.remove("OPENAI_API_KEY");
        if auth.contains_key("tokens") {
            auth.insert("auth_mode".into(), Value::String("chatgpt".into()));
        } else {
            auth.remove("auth_mode");
        }
        return Ok(Some(serde_json::to_string_pretty(&auth)?));
    }
    if target == ClientTarget::ClaudeCode && path == paths.claude_settings {
        let mut settings = object(Some(raw))?;
        settings.remove("modelPicker");
        settings.remove("model");
        if let Some(env) = settings.get_mut("env").and_then(Value::as_object_mut) {
            env.retain(|key, _| {
                !(matches!(
                    key.as_str(),
                    "ANTHROPIC_BASE_URL"
                        | "ANTHROPIC_AUTH_TOKEN"
                        | "CLAUDE_GATEWAY_ALLOW_LOOPBACK"
                        | "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY"
                        | "CLAUDE_CODE_MODEL_LIST_REFRESH_INTERVAL_MS"
                ) || key.starts_with("ANTHROPIC_DEFAULT_"))
            });
        }
        return Ok(Some(serde_json::to_string_pretty(&settings)?));
    }
    // Old versions did not retain a pre-takeover catalog. Delete only the
    // dedicated gateway cache, never a client's general-purpose cache/history.
    Ok(None)
}

pub fn restore(target: ClientTarget) -> Result<()> {
    if target == ClientTarget::ClaudeDesktop {
        crate::claude_desktop_registry::restore_account_mode()?;
        return Ok(());
    }
    let paths = paths(target)?;
    restore_at(&paths, target)?;
    if !cfg!(test)
        && target == ClientTarget::CodexCli
        && !object(optional_text(&paths.codex_auth)?.as_deref())?
            .get("tokens")
            .is_some_and(|tokens| !tokens.is_null())
        && let Some(account) = official_cli_account()?
    {
        backup::atomic_write(&paths.codex_auth, account.as_bytes())?;
    }
    Ok(())
}

pub fn validate_restore(target: ClientTarget) -> Result<()> {
    if target == ClientTarget::ClaudeDesktop {
        return crate::claude_desktop_registry::validate_account_restore();
    }
    let paths = paths(target)?;
    validate_at(&paths, target, &load(&marker(&paths, target))?)
}
fn validate_at(paths: &ClientPaths, target: ClientTarget, snapshot: &Snapshot) -> Result<()> {
    for (id, saved) in &snapshot.files {
        let path = Path::new(id);
        if !tracked(paths, target, path) {
            bail!(
                "{}",
                i18n::tr(
                    "账号模式备份包含无效路径",
                    "The account-mode backup contains an invalid path"
                )
            );
        }
        if path == paths.codex_config {
            document(saved.before.as_deref())?;
            document(Some(&saved.written))?;
        } else {
            if let Some(before) = &saved.before {
                serde_json::from_str::<Value>(before)?;
            }
            serde_json::from_str::<Value>(&saved.written)?;
        }
    }
    if target == ClientTarget::ClaudeCode {
        object(optional_text(&paths.claude_settings)?.as_deref())?;
    } else {
        document(optional_text(&paths.codex_config)?.as_deref())?;
        object(optional_text(&paths.codex_auth)?.as_deref())?;
    }
    Ok(())
}
fn restore_at(paths: &ClientPaths, target: ClientTarget) -> Result<()> {
    restore_snapshot_at(paths, target, &load(&marker(paths, target))?)
}

fn restore_snapshot_at(
    paths: &ClientPaths,
    target: ClientTarget,
    snapshot: &Snapshot,
) -> Result<()> {
    let marker = marker(paths, target);
    validate_at(paths, target, snapshot)?;
    let allowed = if target == ClientTarget::ClaudeCode {
        vec![&paths.claude_settings, &paths.claude_model_cache]
    } else {
        vec![&paths.codex_config, &paths.codex_auth, &paths.codex_catalog]
    };
    // Validate every stored path before writing any file.
    if snapshot.files.keys().any(|id| {
        !allowed
            .iter()
            .any(|path| path.to_string_lossy() == id.as_str())
    }) {
        bail!(
            "{}",
            i18n::tr(
                "账号模式备份包含无效路径",
                "The account-mode backup contains an invalid path"
            )
        );
    }
    if snapshot.files.is_empty() && !is_managed_at(paths, target)? {
        return Ok(());
    }
    #[cfg(any(windows, target_os = "macos"))]
    let mut cleared_codex_loopback = false;
    for path in allowed {
        let raw = optional_text(path)?;
        let saved = snapshot.files.get(path.to_string_lossy().as_ref());
        let restored = if let Some(saved) = saved {
            if path == &paths.codex_config {
                let mut doc = document(raw.as_deref())?;
                let before = document(saved.before.as_deref())?;
                let written = document(Some(&saved.written))?;
                #[cfg(any(windows, target_os = "macos"))]
                let previous = doc.clone();
                restore_toml(&mut doc, &before, &written);
                #[cfg(any(windows, target_os = "macos"))]
                {
                    // Reserved built-in IDs are invalid even when captured
                    // from another router before our explicit takeover.
                    if let Some(providers) =
                        doc.get_mut("model_providers").and_then(Item::as_table_mut)
                    {
                        providers.remove("openai");
                    }
                    cleared_codex_loopback = clear_restored_codex_loopback(&mut doc);
                    retain_restored_codex_provider_references(&mut doc, &previous, &written);
                    #[cfg(windows)]
                    ensure_windows_history_aliases(&mut doc, &paths.codex_sessions);
                }
                Some(doc.to_string())
            } else if path == &paths.codex_auth || path == &paths.claude_settings {
                let mut now = object(raw.as_deref())?;
                let before = object(saved.before.as_deref())?;
                let written = object(Some(&saved.written))?;
                #[cfg(any(windows, target_os = "macos"))]
                if path == &paths.codex_auth {
                    restore_null_codex_account_tokens(&mut now, &before, &written);
                }
                // A login performed while the gateway was active takes precedence
                // over the old snapshot. Do not resurrect an older refresh token.
                let newer_login = path == &paths.codex_auth
                    && now.get("tokens").is_some_and(|tokens| !tokens.is_null());
                if newer_login {
                    if now.get("OPENAI_API_KEY") == written.get("OPENAI_API_KEY") {
                        now.remove("OPENAI_API_KEY");
                    }
                    now.insert("auth_mode".into(), Value::String("chatgpt".into()));
                } else {
                    restore_json(&mut now, &before, &written);
                }
                #[cfg(any(windows, target_os = "macos"))]
                if path == &paths.codex_auth && cleared_codex_loopback {
                    now.remove("OPENAI_API_KEY");
                    if now.get("tokens").is_some_and(|tokens| !tokens.is_null()) {
                        now.insert("auth_mode".into(), Value::String("chatgpt".into()));
                    } else {
                        now.remove("auth_mode");
                    }
                }
                Some(serde_json::to_string_pretty(&now)?)
            } else if raw.as_deref() == Some(saved.written.as_str()) {
                saved.before.clone()
            } else {
                raw.clone()
            }
        } else if snapshot.files.is_empty() {
            clean_legacy(paths, target, path, raw.as_deref())?
        } else {
            raw.clone()
        };
        match restored {
            Some(text) => {
                if raw.as_deref() != Some(text.as_str()) {
                    backup::atomic_write(path, text.as_bytes())?;
                }
            }
            None => {
                backup::remove_file_if_exists(path)?;
            }
        }
    }
    backup::remove_file_if_exists(&marker)?;
    Ok(())
}

#[cfg(all(test, any(windows, target_os = "macos")))]
mod account_regression_tests {
    use super::*;
    use serde_json::json;

    fn auth(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn claude_account_restore_removes_gateway_policy_and_preserves_user_choice() {
        let root =
            std::env::temp_dir().join(format!("as-claude-policy-{:032x}", rand::random::<u128>()));
        let paths =
            ClientPaths::discover(Some(root.join("codex")), Some(root.join("claude"))).unwrap();
        fs::create_dir_all(paths.claude_settings.parent().unwrap()).unwrap();
        let before = json!({
            "model":"claude-sonnet-4-6",
            "permissions":{"allow":["Read"]},
            "env":{"CLAUDE_CODE_NO_MODEL_FALLBACK":"0","USER_OPTION":"keep"}
        });
        let mut written = before.clone();
        for (name, value) in crate::platform::CLAUDE_GATEWAY_MODEL_POLICY {
            written["env"][name] = json!(value);
        }
        written["env"]["ANTHROPIC_BASE_URL"] = json!("http://127.0.0.1:5555");
        written["env"]["ANTHROPIC_AUTH_TOKEN"] = json!("isolated-token");
        written["env"]["CLAUDE_GATEWAY_ALLOW_LOOPBACK"] = json!("1");
        written["env"]["CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY"] = json!("1");
        written["env"]["ANTHROPIC_DEFAULT_SONNET_MODEL"] = json!("gateway-model");
        written["model"] = json!("gateway-model");
        written["modelPicker"] = json!({"options":[{"model":"gateway-model"}]});
        let mut now = written.clone();
        now["env"]["USER_OPTION"] = json!("edited");
        now["permissions"]["allow"] = json!(["Read", "Edit"]);
        let credentials = paths.claude_home.join(".credentials.json");
        let credential_bytes = br#"{"claudeAiOauth":{"accessToken":"isolated-account"}}"#;
        fs::write(&credentials, credential_bytes).unwrap();
        fs::write(&paths.claude_settings, serde_json::to_vec(&now).unwrap()).unwrap();
        let mut snapshot = Snapshot::default();
        snapshot.files.insert(
            paths.claude_settings.to_string_lossy().into_owned(),
            SavedFile {
                before: Some(before.to_string()),
                written: written.to_string(),
            },
        );
        restore_snapshot_at(&paths, ClientTarget::ClaudeCode, &snapshot).unwrap();
        let restored = object(optional_text(&paths.claude_settings).unwrap().as_deref()).unwrap();
        assert_eq!(restored["env"]["CLAUDE_CODE_NO_MODEL_FALLBACK"], "0");
        assert_eq!(restored["env"]["USER_OPTION"], "edited");
        assert_eq!(restored["model"], "claude-sonnet-4-6");
        assert!(!restored.contains_key("modelPicker"));
        assert_eq!(restored["permissions"]["allow"], json!(["Read", "Edit"]));
        assert_eq!(fs::read(&credentials).unwrap().as_slice(), credential_bytes);
        assert!(!is_managed_at(&paths, ClientTarget::ClaudeCode).unwrap());
        for key in [
            "CLAUDE_CODE_DISABLE_MODEL_ACCESS_FALLBACK",
            "CLAUDE_CODE_DISABLE_REFUSAL_FALLBACK",
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_GATEWAY_ALLOW_LOOPBACK",
            "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY",
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
        ] {
            assert!(restored["env"].get(key).is_none());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nullable_gateway_login_restores_saved_account_without_keychain_access() {
        let before = auth(
            json!({"auth_mode":"chatgpt", "tokens":{"refresh_token":"saved"},
            "last_refresh":"saved-time", "keep":"original"}),
        );
        let written =
            auth(json!({"auth_mode":"apikey", "OPENAI_API_KEY":"owned", "keep":"original"}));
        for mode in [Some(json!("apikey")), Some(Value::Null), None] {
            let mut now = auth(json!({"tokens":null, "last_refresh":null,
                "OPENAI_API_KEY":"owned", "keep":"edited"}));
            if let Some(mode) = mode {
                now.insert("auth_mode".into(), mode);
            }
            restore_null_codex_account_tokens(&mut now, &before, &written);
            restore_json(&mut now, &before, &written);
            assert_eq!(now["auth_mode"], "chatgpt");
            assert_eq!(now["tokens"]["refresh_token"], "saved");
            assert_eq!(now["last_refresh"], "saved-time");
            assert_eq!(now["keep"], "edited");
            assert!(!now.contains_key("OPENAI_API_KEY"));
        }
    }

    #[test]
    fn nullable_login_repair_requires_owned_key_and_preserves_new_login() {
        let before = auth(json!({"auth_mode":"chatgpt", "tokens":{"refresh_token":"old"}}));
        let written = auth(json!({"auth_mode":"apikey", "OPENAI_API_KEY":"owned"}));
        for value in [
            json!({"auth_mode":"apikey", "OPENAI_API_KEY":"another", "tokens":null}),
            json!({"auth_mode":"chatgpt", "tokens":{"refresh_token":"new"}}),
            json!({"auth_mode":"chatgpt", "tokens":null}),
        ] {
            let mut now = auth(value);
            let original = now.clone();
            restore_null_codex_account_tokens(&mut now, &before, &written);
            assert_eq!(now, original);
        }
    }

    fn assert_account_alias(doc: &DocumentMut, id: &str) {
        let provider = &doc["model_providers"][id];
        assert_eq!(provider["requires_openai_auth"].as_bool(), Some(true));
        assert_eq!(provider["wire_api"].as_str(), Some("responses"));
        for key in [
            "base_url",
            "env_key",
            "experimental_bearer_token",
            "http_headers",
            "query_params",
        ] {
            assert!(
                provider.get(key).is_none(),
                "{id}: retained gateway field {key}"
            );
        }
    }

    #[test]
    fn gateway_aliases_and_selected_profile_restore_without_losing_user_edits() {
        let before = document(Some(concat!(
            "model_provider='openai'\nprofile='work'\n",
            "[model_providers.custom]\nname='OpenAI'\nwire_api='responses'\nrequires_openai_auth=true\n",
            "[profiles.work]\nmodel_provider='foreign'\nmodel='foreign-model'\nopenai_base_url='http://127.0.0.1:1234'\nmodel_reasoning_effort='low'\n",
            "[model_providers.foreign]\nname='Other router'\nbase_url='http://127.0.0.1:1234'\n"
        ))).unwrap();
        let written = document(Some(concat!(
            "model_provider='openai'\nprofile='work'\nopenai_base_url='http://127.0.0.1:54321'\n",
            "[model_providers.custom]\nname='OpenAI'\nwire_api='responses'\nrequires_openai_auth=true\nbase_url='http://127.0.0.1:54321'\nsupports_websockets=false\n",
            "[profiles.work]\nmodel_provider='openai'\nmodel='gateway-model'\nopenai_base_url='http://127.0.0.1:54321'\nmodel_reasoning_effort='low'\ncli_auth_credentials_store='file'\n",
            "[model_providers.foreign]\nname='Other router'\nbase_url='http://127.0.0.1:1234'\n"
        ))).unwrap();
        let mut now = written.clone();
        now["profiles"]["work"]["model_reasoning_effort"] = toml_edit::value("high");
        now["model_providers"]["custom"]["request_max_retries"] = toml_edit::value(3);
        restore_toml(&mut now, &before, &written);
        assert!(clear_restored_codex_loopback(&mut now));
        retain_restored_codex_provider_references(&mut now, &written, &written);
        assert_account_alias(&now, "custom");
        assert_account_alias(&now, "foreign");
        assert_eq!(
            now["profiles"]["work"]["model_provider"].as_str(),
            Some("openai")
        );
        assert_eq!(
            now["profiles"]["work"]["model_reasoning_effort"].as_str(),
            Some("high")
        );
        assert_eq!(
            now["model_providers"]["custom"]["request_max_retries"].as_integer(),
            Some(3)
        );
        assert!(now["profiles"]["work"].get("openai_base_url").is_none());
        assert!(now["profiles"]["work"].get("model").is_none());
        assert!(now.get("openai_base_url").is_none());
    }

    #[test]
    fn claude_takeover_restore_keeps_account_credentials_and_new_preferences() {
        let before = auth(
            json!({"env":{"ANTHROPIC_API_KEY":"original","CLAUDE_CODE_OAUTH_TOKEN":"original-oauth","KEEP":"original"},"theme":"dark"}),
        );
        let written = auth(
            json!({"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:54321","ANTHROPIC_AUTH_TOKEN":"local","CLAUDE_GATEWAY_ALLOW_LOOPBACK":"1","KEEP":"original"},"theme":"dark"}),
        );
        let mut now = written.clone();
        now["env"]["KEEP"] = json!("edited");
        now["theme"] = json!("light");
        restore_json(&mut now, &before, &written);
        assert_eq!(now["env"]["ANTHROPIC_API_KEY"], "original");
        assert_eq!(now["env"]["CLAUDE_CODE_OAUTH_TOKEN"], "original-oauth");
        assert_eq!(now["env"]["KEEP"], "edited");
        assert_eq!(now["theme"], "light");
        assert!(now["env"].get("ANTHROPIC_BASE_URL").is_none());
        assert!(now["env"].get("ANTHROPIC_AUTH_TOKEN").is_none());
    }

    #[test]
    fn new_gateway_provider_remains_resolvable_after_official_account_restore() {
        let before = document(Some("model_provider='openai'\n")).unwrap();
        let written = document(Some("model_provider='custom'\n[model_providers.custom]\nbase_url='http://127.0.0.1:5555/v1'\nenv_key='OLD_ROUTER_KEY'\nexperimental_bearer_token='obsolete'\n")).unwrap();
        let mut now = written.clone();
        restore_toml(&mut now, &before, &written);
        retain_restored_codex_provider_references(&mut now, &written, &written);
        assert_eq!(now["model_provider"].as_str(), Some("openai"));
        assert_account_alias(&now, "custom");
        // Repeating gateway/account switches must keep the historical ID.
        for _ in 0..2 {
            let before = now.clone();
            now = written.clone();
            restore_toml(&mut now, &before, &written);
            retain_restored_codex_provider_references(&mut now, &written, &written);
            assert_account_alias(&now, "custom");
            assert!(!codex_gateway(&now));
        }
    }

    #[test]
    fn legacy_account_cleanup_preserves_history_provider_without_old_route() {
        let root = std::env::temp_dir().join(format!(
            "little-mungbean-legacy-{:032x}",
            rand::random::<u128>()
        ));
        let paths =
            ClientPaths::discover(Some(root.join("codex")), Some(root.join("claude"))).unwrap();
        let raw = "model_provider='custom'\ncli_auth_credentials_store='file'\n[model_providers.custom]\nbase_url='http://localhost:53601/v1'\nhttp_headers={Authorization='obsolete'}\n";
        let cleaned = clean_legacy(
            &paths,
            ClientTarget::CodexCli,
            &paths.codex_config,
            Some(raw),
        )
        .unwrap()
        .unwrap();
        let doc = document(Some(&cleaned)).unwrap();
        assert_eq!(doc["model_provider"].as_str(), Some("openai"));
        assert_account_alias(&doc, "custom");
        assert!(doc.get("cli_auth_credentials_store").is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn account_restore_does_not_resurrect_pre_takeover_local_route() {
        let before = document(Some(concat!(
            "model_provider='old_router'\nmodel='gateway-only'\n",
            "cli_auth_credentials_store='keyring'\n",
            "[model_providers.old_router]\nbase_url='http://127.0.0.1:63462/v1'\n",
            "[model_providers.unrelated]\nbase_url='https://example.com/v1'\n",
            "[user_settings]\nkeep=true\n"
        )))
        .unwrap();
        let written = document(Some(concat!(
            "model_provider='agent_gateway'\nmodel='gateway-only'\n",
            "cli_auth_credentials_store='file'\n",
            "[model_providers.agent_gateway]\nbase_url='http://127.0.0.1:5555/v1'\n"
        )))
        .unwrap();
        let mut now = before.clone();
        now["model_provider"] = written["model_provider"].clone();
        now["cli_auth_credentials_store"] = written["cli_auth_credentials_store"].clone();
        now["model_providers"]["agent_gateway"] =
            written["model_providers"]["agent_gateway"].clone();
        restore_toml(&mut now, &before, &written);
        assert!(codex_gateway(&now));
        assert!(clear_restored_codex_loopback(&mut now));
        retain_restored_codex_provider_references(&mut now, &written, &written);
        assert_account_alias(&now, "old_router");
        assert_account_alias(&now, "agent_gateway");
        assert!(!codex_gateway(&now));
        assert_eq!(now["model_provider"].as_str(), Some("openai"));
        assert_eq!(now["cli_auth_credentials_store"].as_str(), Some("keyring"));
        assert_eq!(
            now["model_providers"]["unrelated"]["base_url"].as_str(),
            Some("https://example.com/v1")
        );
        assert_eq!(now["user_settings"]["keep"].as_bool(), Some(true));
        assert!(now.get("model").is_none());
    }

    #[test]
    fn official_and_remote_routes_are_not_modified_by_local_route_cleanup() {
        for raw in [
            "model='official'",
            "model_provider='remote'\n[model_providers.remote]\nbase_url='https://example.com/v1'\n",
        ] {
            let mut doc = document(Some(raw)).unwrap();
            let original = doc.to_string();
            assert!(!clear_restored_codex_loopback(&mut doc));
            assert_eq!(doc.to_string(), original);
        }
        let mut root_route = document(Some(
            "openai_base_url='http://localhost:63462'\nmodel_catalog_json='gateway.json'\n",
        ))
        .unwrap();
        assert!(clear_restored_codex_loopback(&mut root_route));
        assert!(root_route.get("openai_base_url").is_none());
        assert!(root_route.get("model_catalog_json").is_none());
    }

    #[test]
    fn both_codex_profiles_restore_account_without_changing_the_other_client() {
        struct TempRoot(PathBuf);
        impl Drop for TempRoot {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let root = TempRoot(std::env::temp_dir().join(format!(
            "little-mungbean-account-restore-{:032x}",
            rand::random::<u128>()
        )));
        for target in [ClientTarget::CodexCli, ClientTarget::CodexDesktop] {
            let own = root.0.join(target.id());
            let paths = ClientPaths::discover(Some(own), Some(root.0.join("claude"))).unwrap();
            fs::create_dir_all(&paths.codex_home).unwrap();
            let other = root.0.join(format!("{}-other", target.id()));
            fs::create_dir_all(&other).unwrap();
            let other_config = b"model_provider='other'\n[model_providers.other]\nbase_url='http://127.0.0.1:6001/v1'\n";
            let other_auth = br#"{"auth_mode":"apikey","OPENAI_API_KEY":"other-client"}"#;
            fs::write(other.join("config.toml"), other_config).unwrap();
            fs::write(other.join("auth.json"), other_auth).unwrap();
            fs::create_dir_all(&paths.codex_sessions).unwrap();
            fs::write(paths.codex_sessions.join("history.jsonl"), b"keep-history").unwrap();

            let before_config = "model_provider='custom'\nmodel='old-gateway-model'\ncli_auth_credentials_store='file'\n[model_providers.custom]\nbase_url='http://127.0.0.1:53601/v1'\n[user]\nkeep=true\n";
            let gateway_config = "model_provider='custom'\nmodel='gateway-model'\ncli_auth_credentials_store='file'\n[model_providers.custom]\nbase_url='http://127.0.0.1:5555/v1'\n[user]\nkeep=true\n";
            let before_auth = r#"{"auth_mode":"chatgpt","OPENAI_API_KEY":"old-route","tokens":{"refresh_token":"saved-login"},"last_refresh":"saved-time"}"#;
            let gateway_auth = r#"{"auth_mode":"apikey","OPENAI_API_KEY":"owned-gateway"}"#;
            fs::write(
                &paths.codex_config,
                gateway_config.replace("keep=true", "keep=false"),
            )
            .unwrap();
            fs::write(&paths.codex_auth, br#"{"auth_mode":"apikey","OPENAI_API_KEY":"owned-gateway","tokens":null,"last_refresh":null}"#).unwrap();
            fs::write(marker(&paths, target), b"test-owned-marker").unwrap();
            let mut snapshot = Snapshot::default();
            for (path, before, written) in [
                (&paths.codex_config, before_config, gateway_config),
                (&paths.codex_auth, before_auth, gateway_auth),
            ] {
                snapshot.files.insert(
                    path.to_string_lossy().into_owned(),
                    SavedFile {
                        before: Some(before.to_owned()),
                        written: written.to_owned(),
                    },
                );
            }
            restore_snapshot_at(&paths, target, &snapshot).unwrap();
            let doc = document(optional_text(&paths.codex_config).unwrap().as_deref()).unwrap();
            assert!(!codex_gateway(&doc));
            assert_eq!(doc["model_provider"].as_str(), Some("openai"));
            assert_eq!(doc["user"]["keep"].as_bool(), Some(false));
            assert_account_alias(&doc, "custom");
            let login = object(optional_text(&paths.codex_auth).unwrap().as_deref()).unwrap();
            assert_eq!(login["auth_mode"], "chatgpt");
            assert_eq!(login["tokens"]["refresh_token"], "saved-login");
            assert_eq!(login["last_refresh"], "saved-time");
            assert!(!login.contains_key("OPENAI_API_KEY"));
            assert!(!marker(&paths, target).exists());
            // A login refreshed while the gateway was active must take priority
            // over the earlier snapshot in the actual file restore path too.
            fs::write(&paths.codex_config, gateway_config).unwrap();
            fs::write(&paths.codex_auth, br#"{"auth_mode":"chatgpt","OPENAI_API_KEY":"owned-gateway","tokens":{"refresh_token":"new-login"},"last_refresh":"new-time"}"#).unwrap();
            restore_snapshot_at(&paths, target, &snapshot).unwrap();
            let login = object(optional_text(&paths.codex_auth).unwrap().as_deref()).unwrap();
            assert_eq!(login["tokens"]["refresh_token"], "new-login");
            assert_eq!(login["last_refresh"], "new-time");
            assert!(!login.contains_key("OPENAI_API_KEY"));
            assert_eq!(
                fs::read(paths.codex_sessions.join("history.jsonl")).unwrap(),
                b"keep-history"
            );
            assert_eq!(fs::read(other.join("config.toml")).unwrap(), other_config);
            assert_eq!(fs::read(other.join("auth.json")).unwrap(), other_auth);
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    fn test_paths() -> ClientPaths {
        let root = std::env::temp_dir().join(format!(
            "little-mungbean-account-test-{:032x}",
            rand::random::<u128>()
        ));
        fs::create_dir_all(&root).unwrap();
        ClientPaths::discover(Some(root.join(".codex")), Some(root.join(".claude"))).unwrap()
    }

    #[test]
    fn windows_history_provider_alias_keeps_old_codex_thread_resumable() {
        let paths = test_paths();
        fs::create_dir_all(paths.codex_sessions.join("2026")).unwrap();
        fs::write(
            paths.codex_sessions.join("2026/thread.jsonl"),
            br#"{"type":"session_meta","payload":{"model_provider":"custom"}}"#,
        )
        .unwrap();
        let mut doc = document(Some("model_provider='openai'\n")).unwrap();
        ensure_windows_history_aliases(&mut doc, &paths.codex_sessions);
        assert_eq!(
            doc["model_providers"]["custom"]["name"].as_str(),
            Some("OpenAI")
        );
        assert!(doc["model_providers"]["custom"].get("base_url").is_none());
        let _ = fs::remove_dir_all(paths.codex_home.parent().unwrap());
    }
    #[test]
    fn snapshot_survives_reconnect_and_preserves_new_settings_and_login() {
        let paths = test_paths();
        backup::atomic_write(
            &paths.codex_config,
            b"model = 'original'\n[unrelated]\nkeep = true\n",
        )
        .unwrap();
        backup::atomic_write(
            &paths.codex_auth,
            br#"{"auth_mode":"chatgpt","tokens":{"refresh_token":"old-secret"}}"#,
        )
        .unwrap();
        for port in [1234, 5678] {
            apply_gateway_files(&paths, ClientTarget::CodexDesktop, vec![
                (paths.codex_config.clone(), format!("model = 'gateway'\nmodel_provider = 'openai'\nopenai_base_url = 'http://127.0.0.1:{port}'\n[unrelated]\nkeep = true\n").into_bytes()),
                (paths.codex_auth.clone(), br#"{"auth_mode":"apikey","OPENAI_API_KEY":"local-secret"}"#.to_vec()),
            ]).unwrap();
        }
        let encrypted = fs::read(marker(&paths, ClientTarget::CodexDesktop)).unwrap();
        assert!(!encrypted.windows(10).any(|bytes| bytes == b"old-secret"));
        fs::write(&paths.codex_auth, br#"{"auth_mode":"chatgpt","OPENAI_API_KEY":"local-secret","tokens":{"refresh_token":"new-secret"}}"#).unwrap();
        let config = fs::read_to_string(&paths.codex_config)
            .unwrap()
            .replace("keep = true", "keep = false");
        fs::write(&paths.codex_config, config).unwrap();
        restore_at(&paths, ClientTarget::CodexDesktop).unwrap();
        let doc = document(optional_text(&paths.codex_config).unwrap().as_deref()).unwrap();
        assert_eq!(doc["model"].as_str(), Some("original"));
        assert_eq!(doc["unrelated"]["keep"].as_bool(), Some(false));
        assert!(doc.get("openai_base_url").is_none());
        let auth = object(optional_text(&paths.codex_auth).unwrap().as_deref()).unwrap();
        assert_eq!(auth["tokens"]["refresh_token"], "new-secret");
        assert!(!auth.contains_key("OPENAI_API_KEY"));
        assert!(!is_managed_at(&paths, ClientTarget::CodexDesktop).unwrap());
        fs::remove_dir_all(paths.codex_home.parent().unwrap()).unwrap();
    }
    #[test]
    fn account_credentials_return_without_erasing_other_claude_environment() {
        let paths = test_paths();
        backup::atomic_write(
            &paths.claude_settings,
            br#"{"env":{"MY_SETTING":"original"},"permissions":{"allow":["Read"]}}"#,
        )
        .unwrap();
        apply_gateway_files(&paths, ClientTarget::ClaudeCode, vec![(paths.claude_settings.clone(), br#"{"env":{"MY_SETTING":"original","ANTHROPIC_BASE_URL":"http://127.0.0.1:1234","ANTHROPIC_AUTH_TOKEN":"local-secret","CLAUDE_GATEWAY_ALLOW_LOOPBACK":"1"},"modelPicker":{},"permissions":{"allow":["Read"]}}"#.to_vec())]).unwrap();
        let mut now = object(optional_text(&paths.claude_settings).unwrap().as_deref()).unwrap();
        now["env"]["MY_SETTING"] = Value::String("edited".into());
        fs::write(&paths.claude_settings, serde_json::to_vec(&now).unwrap()).unwrap();
        restore_at(&paths, ClientTarget::ClaudeCode).unwrap();
        let restored = object(optional_text(&paths.claude_settings).unwrap().as_deref()).unwrap();
        assert_eq!(restored["env"]["MY_SETTING"], "edited");
        assert!(restored["env"].get("ANTHROPIC_AUTH_TOKEN").is_none());
        assert!(restored.get("modelPicker").is_none());
        assert_eq!(restored["permissions"]["allow"][0], "Read");
        fs::remove_dir_all(paths.codex_home.parent().unwrap()).unwrap();
    }
    #[test]
    fn legacy_codex_cleanup_keeps_existing_account_tokens() {
        let paths = test_paths();
        backup::atomic_write(
            &paths.codex_config,
            b"model_provider='openai'\nopenai_base_url='http://127.0.0.1:1234'\nmodel='gateway'\n",
        )
        .unwrap();
        backup::atomic_write(&paths.codex_auth, br#"{"OPENAI_API_KEY":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","tokens":{"refresh_token":"account-secret"}}"#).unwrap();
        restore_at(&paths, ClientTarget::CodexDesktop).unwrap();
        let restored = object(optional_text(&paths.codex_auth).unwrap().as_deref()).unwrap();
        assert_eq!(restored["tokens"]["refresh_token"], "account-secret");
        assert!(!restored.contains_key("OPENAI_API_KEY"));
        assert!(!is_managed_at(&paths, ClientTarget::CodexDesktop).unwrap());
        fs::remove_dir_all(paths.codex_home.parent().unwrap()).unwrap();
    }
    #[test]
    fn refreshed_login_is_saved_before_another_gateway_connection() {
        let paths = test_paths();
        backup::atomic_write(
            &paths.codex_auth,
            br#"{"auth_mode":"chatgpt","tokens":{"refresh_token":"first"}}"#,
        )
        .unwrap();
        let gateway = br#"{"auth_mode":"apikey","OPENAI_API_KEY":"local-secret"}"#;
        apply_gateway_files(
            &paths,
            ClientTarget::CodexDesktop,
            vec![(paths.codex_auth.clone(), gateway.to_vec())],
        )
        .unwrap();
        fs::write(
            &paths.codex_auth,
            br#"{"auth_mode":"chatgpt","tokens":{"refresh_token":"refreshed"}}"#,
        )
        .unwrap();
        apply_gateway_files(
            &paths,
            ClientTarget::CodexDesktop,
            vec![(paths.codex_auth.clone(), gateway.to_vec())],
        )
        .unwrap();
        restore_at(&paths, ClientTarget::CodexDesktop).unwrap();
        let restored = object(optional_text(&paths.codex_auth).unwrap().as_deref()).unwrap();
        assert_eq!(restored["auth_mode"], "chatgpt");
        assert_eq!(restored["tokens"]["refresh_token"], "refreshed");
        assert!(!restored.contains_key("OPENAI_API_KEY"));
        fs::remove_dir_all(paths.codex_home.parent().unwrap()).unwrap();
    }
    #[test]
    fn restoration_keeps_new_environment_when_no_environment_existed_before() {
        let paths = test_paths();
        apply_gateway_files(&paths, ClientTarget::ClaudeCode, vec![(paths.claude_settings.clone(), br#"{"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:1234","ANTHROPIC_AUTH_TOKEN":"local","CLAUDE_GATEWAY_ALLOW_LOOPBACK":"1"}}"#.to_vec())]).unwrap();
        let mut now = object(optional_text(&paths.claude_settings).unwrap().as_deref()).unwrap();
        now["env"]["USER_ADDED"] = Value::String("keep".into());
        fs::write(&paths.claude_settings, serde_json::to_vec(&now).unwrap()).unwrap();
        restore_at(&paths, ClientTarget::ClaudeCode).unwrap();
        let restored = object(optional_text(&paths.claude_settings).unwrap().as_deref()).unwrap();
        assert_eq!(restored["env"]["USER_ADDED"], "keep");
        assert!(restored["env"].get("ANTHROPIC_AUTH_TOKEN").is_none());
        fs::remove_dir_all(paths.codex_home.parent().unwrap()).unwrap();
    }
    #[test]
    fn unrelated_loopback_provider_is_not_taken_over_or_deleted() {
        let paths = test_paths();
        let config = "model_provider='local'\n[model_providers.local]\nbase_url='http://127.0.0.1:1234/v1'\n";
        backup::atomic_write(&paths.codex_config, config.as_bytes()).unwrap();
        backup::atomic_write(&paths.codex_auth, br#"{"OPENAI_API_KEY":"user-key"}"#).unwrap();
        assert!(!is_managed_at(&paths, ClientTarget::CodexDesktop).unwrap());
        restore_at(&paths, ClientTarget::CodexDesktop).unwrap();
        assert_eq!(fs::read_to_string(&paths.codex_config).unwrap(), config);
        fs::remove_dir_all(paths.codex_home.parent().unwrap()).unwrap();
    }
    #[test]
    fn invalid_backup_path_is_rejected_before_any_configuration_write() {
        let paths = test_paths();
        backup::atomic_write(&paths.codex_config, b"model='keep'").unwrap();
        let mut snapshot = Snapshot::default();
        snapshot.files.insert(
            paths
                .codex_home
                .join("unrelated.json")
                .to_string_lossy()
                .into_owned(),
            SavedFile {
                before: None,
                written: "{}".into(),
            },
        );
        save(&marker(&paths, ClientTarget::CodexDesktop), &snapshot).unwrap();
        assert!(restore_at(&paths, ClientTarget::CodexDesktop).is_err());
        assert_eq!(
            fs::read_to_string(&paths.codex_config).unwrap(),
            "model='keep'"
        );
        assert!(marker(&paths, ClientTarget::CodexDesktop).is_file());
        fs::remove_dir_all(paths.codex_home.parent().unwrap()).unwrap();
    }
}
