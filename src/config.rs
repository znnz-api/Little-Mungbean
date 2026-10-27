use crate::claude_desktop_proxy::{
    DesktopModelMenu, ModelSlots, SlotOverrides, select_claude_code_model_slots, select_model_slots,
};
use crate::gateway::GatewayIdentity;
use crate::{backup, catalog, platform};
use anyhow::{Context, Result, bail};
use directories::{BaseDirs, UserDirs};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table, value};
use tracing::info;

const CODEX_CLI_GATEWAY_PROVIDER_ID: &str = "agent_gateway";

#[cfg(windows)]
pub(crate) fn historical_provider_ids(sessions: &Path) -> BTreeSet<String> {
    use std::io::Read;
    let mut ids = BTreeSet::new();
    let mut pending = vec![sessions.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            let path = entry.path();
            if kind.is_dir() {
                pending.push(path);
                continue;
            }
            if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(file) = fs::File::open(path) else {
                continue;
            };
            // Read only bounded session metadata, never the conversation body.
            let mut first = String::new();
            if BufReader::new(file.take(64 * 1024))
                .read_line(&mut first)
                .is_err()
            {
                continue;
            }
            let Ok(metadata) = serde_json::from_str::<Value>(&first) else {
                continue;
            };
            let id = metadata
                .get("model_provider")
                .or_else(|| metadata.pointer("/payload/model_provider"))
                .and_then(Value::as_str)
                .map(str::trim);
            if let Some(id) =
                id.filter(|id| !id.is_empty() && !matches!(*id, "openai" | "ollama" | "lmstudio"))
            {
                ids.insert(id.to_owned());
            }
        }
    }
    ids
}

fn set_codex_gateway_provider(
    doc: &mut DocumentMut,
    id: &str,
    gateway: &GatewayIdentity,
) -> Result<()> {
    if doc.get("model_providers").is_some() && !doc["model_providers"].is_table() {
        bail!("model_providers 不是 TOML 表，拒绝覆盖");
    }
    if doc.get("model_providers").is_none() {
        doc["model_providers"] = Item::Table(Table::new());
    }
    if doc["model_providers"].get(id).is_none() {
        doc["model_providers"][id] = Item::Table(Table::new());
    }
    let provider = doc["model_providers"][id]
        .as_table_mut()
        .with_context(|| format!("model_providers.{id} 不是 TOML 表，拒绝覆盖"))?;
    for key in [
        "env_key",
        "env_key_instructions",
        "experimental_bearer_token",
        "gateway_oauth",
        "http_headers",
        "query_params",
    ] {
        provider.remove(key);
    }
    #[cfg(any(windows, target_os = "macos"))]
    for key in ["auth", "aws", "env_http_headers", "model_catalog_url"] {
        provider.remove(key);
    }
    provider["name"] = value(&gateway.display_name);
    provider["base_url"] = value(&gateway.base_url);
    provider["wire_api"] = value("responses");
    provider["requires_openai_auth"] = value(true);
    provider["request_max_retries"] = value(1);
    provider["stream_max_retries"] = value(1);
    provider["stream_idle_timeout_ms"] = value(120000);
    provider["supports_websockets"] = value(false);
    provider["supports_standalone_web_search"] = value(false);
    Ok(())
}

#[cfg(any(windows, target_os = "macos"))]
fn codex_provider_matches(provider: &Item, url: &str) -> bool {
    provider.get("base_url").and_then(Item::as_str) == Some(url.trim_end_matches('/'))
        && provider.get("requires_openai_auth").and_then(Item::as_bool) == Some(true)
        && [
            "env_key",
            "env_key_instructions",
            "experimental_bearer_token",
            "auth",
            "aws",
            "gateway_oauth",
            "http_headers",
            "env_http_headers",
            "query_params",
            "model_catalog_url",
        ]
        .iter()
        .all(|key| provider.get(key).is_none())
}

pub struct ClientPaths {
    pub user_home: PathBuf,
    pub codex_home: PathBuf,
    pub codex_config: PathBuf,
    pub codex_auth: PathBuf,
    pub codex_global_state: PathBuf,
    pub codex_catalog: PathBuf,
    pub codex_sessions: PathBuf,
    pub claude_home: PathBuf,
    pub claude_settings: PathBuf,
    pub claude_state: PathBuf,
    pub claude_model_cache: PathBuf,
}

impl ClientPaths {
    pub fn discover(codex_home: Option<PathBuf>, claude_home: Option<PathBuf>) -> Result<Self> {
        let user_home = UserDirs::new()
            .context("无法确定 Windows 用户目录")?
            .home_dir()
            .to_path_buf();
        let codex_home = absolute(codex_home.unwrap_or_else(|| user_home.join(".codex")))?;
        let custom_claude_home = claude_home.is_some();
        let claude_home = absolute(claude_home.unwrap_or_else(|| user_home.join(".claude")))?;
        let claude_root = if custom_claude_home {
            claude_home
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| user_home.clone())
        } else {
            user_home.clone()
        };
        Ok(Self {
            user_home: user_home.clone(),
            codex_config: codex_home.join("config.toml"),
            codex_auth: codex_home.join("auth.json"),
            codex_global_state: codex_home.join(".codex-global-state.json"),
            codex_catalog: codex_home.join("model-catalogs").join("gateway.json"),
            codex_sessions: codex_home.join("sessions"),
            codex_home,
            claude_settings: claude_home.join("settings.json"),
            claude_model_cache: claude_home.join("cache").join("gateway-models.json"),
            claude_home,
            claude_state: claude_root.join(".claude.json"),
        })
    }

    pub fn default_codex_home() -> Result<PathBuf> {
        Ok(UserDirs::new()
            .context("无法确定 Windows 用户目录")?
            .home_dir()
            .join(".codex"))
    }
}

/// Remove the obsolete locale override written by older launcher builds.
/// Codex Desktop now chooses its language from the operating-system language and
/// its own downloaded language resources, so the launcher must not force a locale.
pub fn remove_codex_desktop_locale_override(path: &Path) -> Result<bool> {
    let original = read_optional(path)?;
    if original.trim().is_empty() {
        return Ok(false);
    }

    let mut doc = parse_toml_or_empty(&original, path)?;
    let Some(desktop) = doc.get_mut("desktop").and_then(Item::as_table_mut) else {
        return Ok(false);
    };
    if desktop.remove("localeOverride").is_none() {
        return Ok(false);
    }

    let merged = format!("{}\n", doc.to_string().trim_end());
    if merged != original {
        backup::atomic_write(path, merged.as_bytes())?;
    }
    Ok(true)
}
pub fn read_codex_api_key(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("Unable to read Codex auth.json: {}", path.display()))?;
    let value: Value = serde_json::from_str(&raw).with_context(|| {
        format!(
            "Codex auth.json is not a valid JSON object: {}",
            path.display()
        )
    })?;
    Ok(value
        .get("OPENAI_API_KEY")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned))
}

pub fn key_fingerprint(key: &str) -> String {
    let digest = Sha256::digest(key.as_bytes());
    format!(
        "已设置，长度={}，SHA256={}…",
        key.chars().count(),
        hex_prefix(&digest, 8)
    )
}

fn merge_codex_config(
    mut doc: DocumentMut,
    provider_id: &str,
    default_model: &str,
    catalog_path: &Path,
    gateway_url: &str,
    pin_model_catalog: bool,
) -> Result<String> {
    let gateway = GatewayIdentity::parse(gateway_url)?;
    #[cfg(windows)]
    {
        if let Some(providers) = doc.get_mut("model_providers").and_then(Item::as_table_mut) {
            providers.remove("openai");
        }
        if provider_id != CODEX_CLI_GATEWAY_PROVIDER_ID {
            doc["openai_base_url"] = value(gateway_url.trim_end_matches('/'));
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(providers) = doc.get_mut("model_providers").and_then(Item::as_table_mut) {
            providers.remove("openai");
        }
        doc["openai_base_url"] = value(gateway_url.trim_end_matches('/'));
    }
    doc["model_provider"] = value(provider_id);
    doc["model"] = value(default_model);
    // Gateway mode must use the local API credential, never a cached OAuth
    // login from the OS keyring. The account-mode snapshot restores this field.
    doc["cli_auth_credentials_store"] = value("file");
    if pin_model_catalog {
        doc["model_catalog_json"] = value(catalog_path.to_string_lossy().replace('\\', "/"));
    } else {
        doc.remove("model_catalog_json");
    }

    if provider_id.eq_ignore_ascii_case("openai") {
        // The built-in provider cannot be overridden by a same-name table.
        // Explicit takeover removes foreign overrides regardless of their name.
        doc["openai_base_url"] = value(gateway_url.trim_end_matches('/'));
        #[cfg(not(any(windows, target_os = "macos")))]
        if let Some(providers) = doc.get_mut("model_providers").and_then(Item::as_table_mut) {
            let remove_stale = providers
                .get("openai")
                .and_then(Item::as_table)
                .is_some_and(|provider| {
                    provider.get("name").and_then(Item::as_str) == Some("znnz.net")
                });
            if remove_stale {
                providers.remove("openai");
            }
        }
    } else {
        if provider_id == CODEX_CLI_GATEWAY_PROVIDER_ID {
            // 启动器为 Codex CLI 使用独立 provider，并明确禁用 WebSocket。
            // 同时移除根级 openai_base_url，避免它继续覆盖独立 provider。
            doc.remove("openai_base_url");
        }
        set_codex_gateway_provider(&mut doc, provider_id, &gateway)?;
    }

    #[cfg(any(windows, target_os = "macos"))]
    {
        // Account-mode compatibility aliases are still referenced by old
        // threads. Route them too, without changing any thread history.
        let aliases = doc
            .get("model_providers")
            .and_then(Item::as_table)
            .into_iter()
            .flat_map(|providers| providers.iter())
            .filter(|(id, provider)| {
                *id != provider_id
                    && provider.get("base_url").is_none_or(|url| {
                        url.as_str()
                            .and_then(|url| url::Url::parse(url).ok())
                            .is_some_and(|url| {
                                matches!(
                                    url.host_str(),
                                    Some("127.0.0.1" | "localhost" | "::1" | "[::1]")
                                )
                            })
                    })
                    && provider.get("name").and_then(Item::as_str) == Some("OpenAI")
                    && provider.get("requires_openai_auth").and_then(Item::as_bool) == Some(true)
            })
            .map(|(id, _)| id.to_owned())
            .collect::<Vec<_>>();
        for id in aliases {
            set_codex_gateway_provider(&mut doc, &id, &gateway)?;
            doc["model_providers"][&id]["name"] = value("OpenAI");
        }
        if let Some(profile) = doc.get("profile").and_then(Item::as_str).map(str::to_owned)
            && let Some(profile) = doc
                .get_mut("profiles")
                .and_then(Item::as_table_mut)
                .and_then(|profiles| profiles.get_mut(&profile))
                .and_then(Item::as_table_mut)
        {
            profile["model_provider"] = value(provider_id);
            profile["model"] = value(default_model);
            profile["cli_auth_credentials_store"] = value("file");
            profile["openai_base_url"] = value(gateway_url.trim_end_matches('/'));
            if pin_model_catalog {
                profile["model_catalog_json"] =
                    value(catalog_path.to_string_lossy().replace('\\', "/"));
            } else {
                profile.remove("model_catalog_json");
            }
        }
    }

    Ok(format!("{}\n", doc.to_string().trim_end()))
}
fn merge_codex_auth(mut object: Map<String, Value>, api_key: &str) -> Value {
    object.remove("tokens");
    object.remove("last_refresh");
    object.insert("auth_mode".to_owned(), Value::String("apikey".to_owned()));
    object.insert(
        "OPENAI_API_KEY".to_owned(),
        Value::String(api_key.to_owned()),
    );
    Value::Object(object)
}

fn merge_codex_global_state(mut object: Map<String, Value>) -> Result<Value> {
    let atom = object
        .entry("electron-persisted-atom-state".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let atom = atom
        .as_object_mut()
        .context("electron-persisted-atom-state 不是 JSON 对象，拒绝覆盖 Desktop 状态")?;
    atom.insert(
        "composer-model-picker-menu-view-v1".to_owned(),
        Value::String("advanced".to_owned()),
    );
    object.remove("composer-model-picker-menu-view-v1");
    Ok(Value::Object(object))
}

fn merge_claude_settings(
    mut object: Map<String, Value>,
    gateway_url: &str,
    use_gateway_model_list: bool,
    slots: &ModelSlots,
    gateway_catalog: &Value,
) -> Result<Value> {
    let gateway = GatewayIdentity::parse(gateway_url)?;
    // Do not force a concrete model on startup. Removing a stale value also
    // makes Claude Code use its own `Default (recommended)` entry.
    object.remove("model");
    let source_label =
        catalog_source_label(gateway_catalog).unwrap_or_else(|| gateway.source_label());
    if use_gateway_model_list {
        let menu = DesktopModelMenu::claude_code_catalog(gateway_catalog, &source_label)?;
        object.insert("modelPicker".to_owned(), menu.claude_code_model_picker());
    } else {
        object.remove("modelPicker");
    }
    let env = object
        .entry("env".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let env = env
        .as_object_mut()
        .context("Claude settings.json 的 env 不是 JSON 对象")?;
    let common_entries = [
        (
            "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY",
            if use_gateway_model_list { "1" } else { "0" },
        ),
        ("CLAUDE_CODE_MODEL_LIST_REFRESH_INTERVAL_MS", "300000"),
    ];
    for (key, value) in common_entries {
        env.insert(key.to_owned(), Value::String(value.to_owned()));
    }
    if use_gateway_model_list {
        env.remove("ANTHROPIC_BASE_URL");
    } else {
        env.insert(
            "ANTHROPIC_BASE_URL".to_owned(),
            Value::String(gateway.base_url.clone()),
        );
    }

    let slot_model_keys = [
        "ANTHROPIC_DEFAULT_FABLE_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    ];
    let slot_presentation_keys = [
        "ANTHROPIC_DEFAULT_FABLE_MODEL_NAME",
        "ANTHROPIC_DEFAULT_FABLE_MODEL_DESCRIPTION",
        "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
        "ANTHROPIC_DEFAULT_OPUS_MODEL_DESCRIPTION",
        "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
        "ANTHROPIC_DEFAULT_SONNET_MODEL_DESCRIPTION",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL_DESCRIPTION",
    ];
    let slot_capability_keys = [
        "ANTHROPIC_DEFAULT_FABLE_MODEL_SUPPORTED_CAPABILITIES",
        "ANTHROPIC_DEFAULT_OPUS_MODEL_SUPPORTED_CAPABILITIES",
        "ANTHROPIC_DEFAULT_SONNET_MODEL_SUPPORTED_CAPABILITIES",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL_SUPPORTED_CAPABILITIES",
    ];
    for key in slot_model_keys
        .into_iter()
        .chain(slot_presentation_keys)
        .chain(slot_capability_keys)
    {
        env.remove(key);
    }
    if !use_gateway_model_list {
        let visible_models = catalog::visible_slugs(gateway_catalog);
        let slot_values = [
            (
                "ANTHROPIC_DEFAULT_FABLE_MODEL",
                slots.fable.as_str(),
                "Fable",
            ),
            ("ANTHROPIC_DEFAULT_OPUS_MODEL", slots.opus.as_str(), "Opus"),
            (
                "ANTHROPIC_DEFAULT_SONNET_MODEL",
                slots.sonnet.as_str(),
                "Sonnet",
            ),
            (
                "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                slots.haiku.as_str(),
                "Haiku",
            ),
        ];
        for (model_key, model, friendly_name) in slot_values {
            // Only customize a native family slot when that exact model is
            // available to this key. Missing families retain Claude Code's
            // built-in model, label, and description instead of looking like
            // gateway-provided models or being mapped to another family.
            if !visible_models.iter().any(|visible| visible == model) {
                continue;
            }
            env.insert(model_key.to_owned(), Value::String(model.to_owned()));
            env.insert(
                format!("{model_key}_NAME"),
                Value::String(friendly_name.to_owned()),
            );
            env.insert(
                format!("{model_key}_DESCRIPTION"),
                Value::String(source_label.clone()),
            );
        }
    }
    Ok(Value::Object(object))
}

fn catalog_source_label(catalog: &Value) -> Option<String> {
    catalog
        .get("models")
        .and_then(Value::as_array)
        .and_then(|models| {
            models.iter().find_map(|model| {
                model
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|description| description.starts_with("From "))
                    .map(ToOwned::to_owned)
            })
        })
}

fn merge_claude_state(mut object: Map<String, Value>) -> Value {
    object.insert("hasCompletedOnboarding".to_owned(), Value::Bool(true));
    Value::Object(object)
}

fn choose_provider_id(doc: &DocumentMut, sessions_directory: &Path) -> String {
    let existing = doc
        .get("model_provider")
        .and_then(Item::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(provider) = existing
        && !matches!(provider.to_ascii_lowercase().as_str(), "znnz" | "znnz.net")
    {
        return provider.to_owned();
    }

    if let Some(provider) = most_used_historical_provider(sessions_directory) {
        return provider;
    }

    existing.unwrap_or("openai").to_owned()
}

fn most_used_historical_provider(sessions_directory: &Path) -> Option<String> {
    let mut counts = BTreeMap::<String, usize>::new();
    let mut pending = vec![sessions_directory.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(file) = fs::File::open(path) else {
                continue;
            };
            let mut first_line = String::new();
            if BufReader::new(file).read_line(&mut first_line).is_err() {
                continue;
            }
            let Some(provider) = serde_json::from_str::<Value>(&first_line)
                .ok()
                .and_then(|value| value.get("model_provider")?.as_str().map(str::to_owned))
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            if matches!(provider.to_ascii_lowercase().as_str(), "znnz" | "znnz.net") {
                continue;
            }
            *counts.entry(provider).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by(|(left_name, left_count), (right_name, right_count)| {
            left_count
                .cmp(right_count)
                .then_with(|| right_name.cmp(left_name))
        })
        .map(|(provider, _)| provider)
}

fn parse_toml_or_empty(raw: &str, path: &Path) -> Result<DocumentMut> {
    if raw.trim().is_empty() {
        return Ok(DocumentMut::new());
    }
    raw.parse::<DocumentMut>()
        .with_context(|| format!("配置不是有效 TOML，拒绝覆盖: {}", path.display()))
}

fn read_json_object(path: &Path, description: &str) -> Result<Map<String, Value>> {
    if !path.exists() {
        return Ok(Map::new());
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("无法读取 {description}: {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(Map::new());
    }
    serde_json::from_str::<Value>(&raw)
        .with_context(|| format!("{description} 不是有效 JSON，拒绝覆盖: {}", path.display()))?
        .as_object()
        .cloned()
        .with_context(|| format!("{description} 必须是 JSON 对象: {}", path.display()))
}

fn read_optional(path: &Path) -> Result<String> {
    if !path.exists() {
        return Ok(String::new());
    }
    fs::read_to_string(path).with_context(|| format!("无法读取 {}", path.display()))
}

fn pretty_json(value: &Value) -> Result<String> {
    Ok(format!("{}\n", serde_json::to_string_pretty(value)?))
}

fn load_codex_bundled_catalog(codex_home: &Path) -> Result<Value> {
    #[cfg(test)]
    {
        let _ = codex_home;
        parse_codex_bundled_catalog(
            r#"{"models":[{"slug":"gpt-test","display_name":"GPT Test","description":"Bundled test model.","visibility":"list","supported_in_api":true}]}"#,
        )
    }
    #[cfg(not(test))]
    {
        let raw = platform::read_codex_bundled_catalog(codex_home)?;
        parse_codex_bundled_catalog(&raw)
    }
}

fn parse_codex_bundled_catalog(raw: &str) -> Result<Value> {
    let parsed: Value = serde_json::from_str(raw).context("Codex 自带模型目录没有返回有效 JSON")?;
    catalog::validate_catalog(&parsed).context("Codex 自带模型目录格式无效")?;
    Ok(json!({ "models": parsed["models"].clone() }))
}

fn annotate_codex_bundled_catalog_sources(
    mut bundled_catalog: Value,
    gateway_catalog: &Value,
    gateway_url: &str,
) -> Result<Value> {
    let gateway_models = catalog::visible_slugs(gateway_catalog)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let source_label = catalog_source_label(gateway_catalog).unwrap_or_else(|| {
        GatewayIdentity::parse(gateway_url)
            .map(|gateway| gateway.source_label())
            .unwrap_or_default()
    });
    let models = bundled_catalog
        .get_mut("models")
        .and_then(Value::as_array_mut)
        .context("Codex 自带模型目录缺少 models 数组")?;

    for model in models {
        let visible = model.get("visibility").and_then(Value::as_str) != Some("hide");
        let supported = model.get("supported_in_api").and_then(Value::as_bool) != Some(false);
        if !visible || !supported {
            continue;
        }
        let Some(slug) = model.get("slug").and_then(Value::as_str) else {
            continue;
        };
        if !gateway_models.contains(slug) {
            continue;
        }
        let Some(description) = model
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|description| !description.is_empty())
        else {
            continue;
        };
        model["description"] = Value::String(format!("{description} ({source_label})"));
    }

    catalog::validate_catalog(&bundled_catalog).context("Codex 默认模型目录来源标记后格式无效")?;
    Ok(bundled_catalog)
}

fn validate_gateway_url(value: &str) -> Result<()> {
    let parsed = url::Url::parse(value.trim()).context("网关地址无效")?;
    if parsed.scheme() != "https"
        && !matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "::1"))
    {
        bail!("网关必须使用 HTTPS；只有回环地址允许 HTTP");
    }
    if parsed.host_str().is_none() {
        bail!("网关地址缺少主机名");
    }
    Ok(())
}

fn detect_codex_version() -> Option<String> {
    #[cfg(target_os = "macos")]
    let executables = crate::platform::find_executable("codex")
        .into_iter()
        .collect::<Vec<_>>();
    #[cfg(not(target_os = "macos"))]
    let executables = ["codex.exe", "codex"].map(PathBuf::from);
    for executable in executables {
        let Ok(output) = std::process::Command::new(executable)
            .arg("--version")
            .output()
        else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        if let Some(token) = text
            .split_whitespace()
            .find(|part| part.chars().next().is_some_and(|c| c.is_ascii_digit()))
        {
            return Some(token.trim().to_owned());
        }
    }
    None
}

fn absolute(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn hex_prefix(bytes: &[u8], count: usize) -> String {
    bytes
        .iter()
        .take(count)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub gateway_url: String,
    pub default_model: Option<String>,
    pub dry_run: bool,
    pub set_user_environment: bool,
}

impl ClientPaths {
    pub fn launcher_codex_cli_profile() -> Result<Self> {
        let base = BaseDirs::new()
            .context("Unable to locate the current user's local application data directory")?
            .data_local_dir()
            .join("Little-Mungbean")
            .join("profiles")
            .join("codex-cli");
        Self::discover(Some(base), None)
    }
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    backup::atomic_write(path, pretty_json(value)?.as_bytes())
}

pub(crate) async fn fetch_gateway_catalog(
    options: &InstallOptions,
    api_key: &str,
) -> Result<Value> {
    validate_gateway_url(&options.gateway_url)?;
    let client_version = detect_codex_version().unwrap_or_else(|| "0.145.0".to_owned());
    catalog::fetch_catalog(&options.gateway_url, api_key, &client_version).await
}

fn codex_catalog_for_mode(
    paths: &ClientPaths,
    gateway_catalog: &Value,
    gateway_url: &str,
    gateway_mode: bool,
) -> Result<Value> {
    if gateway_mode {
        Ok(gateway_catalog.clone())
    } else {
        let bundled_catalog = load_codex_bundled_catalog(&paths.codex_home)?;
        annotate_codex_bundled_catalog_sources(bundled_catalog, gateway_catalog, gateway_url)
    }
}

fn merged_codex_files(
    paths: &ClientPaths,
    options: &InstallOptions,
    api_key: &str,
    catalog_value: &Value,
    provider_id: &str,
    pin_catalog: bool,
    write_catalog: bool,
) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    let default_model =
        catalog::select_default_model(catalog_value, options.default_model.as_deref())?;
    let config = parse_toml_or_empty(&read_optional(&paths.codex_config)?, &paths.codex_config)?;
    #[cfg(windows)]
    let config = {
        let mut config = config;
        crate::account_mode::ensure_windows_history_aliases(&mut config, &paths.codex_sessions);
        config
    };
    let merged_config = merge_codex_config(
        config,
        provider_id,
        &default_model,
        &paths.codex_catalog,
        &options.gateway_url,
        pin_catalog,
    )?;
    let auth = merge_codex_auth(
        read_json_object(&paths.codex_auth, "Codex auth.json")?,
        api_key,
    );
    let global_state = merge_codex_global_state(read_json_object(
        &paths.codex_global_state,
        "Codex global state",
    )?)?;
    let mut output = vec![
        (paths.codex_config.clone(), merged_config.into_bytes()),
        (paths.codex_auth.clone(), pretty_json(&auth)?.into_bytes()),
        (
            paths.codex_global_state.clone(),
            pretty_json(&global_state)?.into_bytes(),
        ),
    ];
    if write_catalog {
        output.push((
            paths.codex_catalog.clone(),
            pretty_json(catalog_value)?.into_bytes(),
        ));
    }
    Ok(output)
}

fn apply_files(files: Vec<(PathBuf, Vec<u8>)>) -> Result<()> {
    for (path, bytes) in files {
        backup::atomic_write(&path, &bytes)?;
    }
    Ok(())
}

#[cfg(test)]
pub async fn install_codex_with_model_list(
    paths: &ClientPaths,
    options: &InstallOptions,
    api_key: &str,
    use_gateway_model_list: bool,
) -> Result<()> {
    install_codex_with_hidden_models(paths, options, api_key, use_gateway_model_list, &[]).await
}

#[cfg(test)]
pub async fn install_codex_with_hidden_models(
    paths: &ClientPaths,
    options: &InstallOptions,
    api_key: &str,
    use_gateway_model_list: bool,
    hidden_models: &[String],
) -> Result<()> {
    let gateway = fetch_gateway_catalog(options, api_key).await?;
    install_codex_with_catalog(
        paths,
        options,
        api_key,
        use_gateway_model_list,
        hidden_models,
        &gateway,
    )
}

pub fn install_codex_with_catalog(
    paths: &ClientPaths,
    options: &InstallOptions,
    api_key: &str,
    use_gateway_model_list: bool,
    hidden_models: &[String],
    gateway: &Value,
) -> Result<()> {
    catalog::validate_catalog(gateway).context(crate::i18n::tr(
        "网关模型目录尚未通过校验",
        "The gateway model catalog has not passed validation",
    ))?;
    if options.dry_run {
        return Ok(());
    }
    let config = parse_toml_or_empty(&read_optional(&paths.codex_config)?, &paths.codex_config)?;
    let provider = choose_provider_id(&config, &paths.codex_sessions);
    let mut files = merged_codex_files(
        paths,
        options,
        api_key,
        gateway,
        &provider,
        false,
        use_gateway_model_list,
    )?;
    if use_gateway_model_list {
        replace_model_menu_file(&mut files, &paths.codex_catalog, gateway, hidden_models)?;
    }
    crate::account_mode::apply_gateway_files(
        paths,
        crate::gui_worker::ClientTarget::CodexDesktop,
        files,
    )
}

/// Verify the configuration that the next Desktop process will actually read.
/// Other gateway tools may edit the shared Codex profile while the app is open;
/// explicit connection takes over the route, then verifies the written values.
pub fn codex_gateway_configuration_matches(
    paths: &ClientPaths,
    gateway_url: &str,
    api_key: &str,
) -> Result<bool> {
    let config = parse_toml_or_empty(&read_optional(&paths.codex_config)?, &paths.codex_config)?;
    let provider_id = config
        .get("model_provider")
        .and_then(Item::as_str)
        .unwrap_or_default();
    let configured_url = if provider_id.eq_ignore_ascii_case("openai") {
        config.get("openai_base_url").and_then(Item::as_str)
    } else {
        config
            .get("model_providers")
            .and_then(|providers| providers.get(provider_id))
            .and_then(|provider| provider.get("base_url"))
            .and_then(Item::as_str)
    };
    let auth = read_json_object(&paths.codex_auth, "Codex auth.json")?;
    let matches = configured_url
        .is_some_and(|value| value.trim_end_matches('/') == gateway_url.trim_end_matches('/'))
        && auth.get("OPENAI_API_KEY").and_then(Value::as_str) == Some(api_key);
    #[cfg(any(windows, target_os = "macos"))]
    let matches = matches
        && config
            .get("cli_auth_credentials_store")
            .and_then(Item::as_str)
            == Some("file")
        && auth.get("auth_mode").and_then(Value::as_str) == Some("apikey")
        && auth.get("tokens").is_none_or(Value::is_null)
        && if provider_id == "openai" {
            config
                .get("model_providers")
                .and_then(|providers| providers.get("openai"))
                .is_none()
        } else {
            config
                .get("model_providers")
                .and_then(|providers| providers.get(provider_id))
                .is_some_and(|provider| codex_provider_matches(provider, gateway_url))
        };
    #[cfg(any(windows, target_os = "macos"))]
    let matches = matches
        && config
            .get("profile")
            .and_then(Item::as_str)
            .is_none_or(|id| {
                config
                    .get("profiles")
                    .and_then(|profiles| profiles.get(id))
                    .is_some_and(|profile| {
                        profile.get("model_provider").and_then(Item::as_str) == Some(provider_id)
                            && profile
                                .get("cli_auth_credentials_store")
                                .and_then(Item::as_str)
                                == Some("file")
                            && profile.get("openai_base_url").and_then(Item::as_str)
                                == Some(gateway_url.trim_end_matches('/'))
                    })
            });
    Ok(matches)
}

#[cfg(any(windows, target_os = "macos"))]
pub fn claude_code_gateway_configuration_matches(
    paths: &ClientPaths,
    gateway_url: &str,
    api_key: &str,
) -> Result<bool> {
    let settings = read_json_object(&paths.claude_settings, "Claude settings.json")?;
    let env = settings.get("env").and_then(Value::as_object);
    let configured_url = env
        .and_then(|env| env.get("ANTHROPIC_BASE_URL"))
        .and_then(Value::as_str);
    let configured_token = env
        .and_then(|env| env.get("ANTHROPIC_AUTH_TOKEN"))
        .and_then(Value::as_str);
    let no_proxy = env
        .and_then(|env| env.get("NO_PROXY"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let is_loopback = configured_url
        .and_then(|value| url::Url::parse(value).ok())
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1" | "[::1]"));
    let bypasses_loopback = ["127.0.0.1", "localhost", "::1"].iter().all(|host| {
        no_proxy
            .split(',')
            .any(|entry| entry.trim().eq_ignore_ascii_case(host))
    });
    Ok(is_loopback
        && configured_url
            .is_some_and(|value| value.trim_end_matches('/') == gateway_url.trim_end_matches('/'))
        && configured_token == Some(api_key)
        && env
            .and_then(|env| env.get("CLAUDE_GATEWAY_ALLOW_LOOPBACK"))
            .and_then(Value::as_str)
            == Some("1")
        && ["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"]
            .iter()
            .all(|key| env.and_then(|env| env.get(*key)).is_none())
        && bypasses_loopback)
}

pub fn claude_code_connection_details(paths: &ClientPaths) -> Result<Option<(String, String)>> {
    let settings = read_json_object(&paths.claude_settings, "Claude settings.json")?;
    let Some(env) = settings.get("env").and_then(Value::as_object) else {
        return Ok(None);
    };
    let Some(url) = env
        .get("ANTHROPIC_BASE_URL")
        .and_then(Value::as_str)
        .filter(|url| {
            url::Url::parse(url).ok().is_some_and(|parsed| {
                matches!(
                    parsed.host_str(),
                    Some("127.0.0.1" | "localhost" | "::1" | "[::1]")
                )
            })
        })
    else {
        return Ok(None);
    };
    let Some(token) = env
        .get("ANTHROPIC_AUTH_TOKEN")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
    else {
        return Ok(None);
    };
    Ok(Some((url.to_owned(), token.to_owned())))
}

#[cfg(test)]
pub async fn install_codex_cli_with_model_list(
    paths: &ClientPaths,
    options: &InstallOptions,
    api_key: &str,
    use_gateway_model_list: bool,
) -> Result<()> {
    install_codex_cli_with_hidden_models(paths, options, api_key, use_gateway_model_list, &[]).await
}

#[cfg(test)]
pub async fn install_codex_cli_with_hidden_models(
    paths: &ClientPaths,
    options: &InstallOptions,
    api_key: &str,
    use_gateway_model_list: bool,
    hidden_models: &[String],
) -> Result<()> {
    let gateway = fetch_gateway_catalog(options, api_key).await?;
    install_codex_cli_with_catalog(
        paths,
        options,
        api_key,
        use_gateway_model_list,
        hidden_models,
        &gateway,
    )
}

pub fn install_codex_cli_with_catalog(
    paths: &ClientPaths,
    options: &InstallOptions,
    api_key: &str,
    use_gateway_model_list: bool,
    hidden_models: &[String],
    gateway: &Value,
) -> Result<()> {
    catalog::validate_catalog(gateway).context(crate::i18n::tr(
        "网关模型目录尚未通过校验",
        "The gateway model catalog has not passed validation",
    ))?;
    if options.dry_run {
        return Ok(());
    }
    let selected_catalog =
        codex_catalog_for_mode(paths, gateway, &options.gateway_url, use_gateway_model_list)?;
    // Pin the annotated bundled catalog so the CLI displays matching gateway sources.
    let pin_catalog = use_gateway_model_list || cfg!(any(windows, target_os = "macos"));
    let mut files = merged_codex_files(
        paths,
        options,
        api_key,
        &selected_catalog,
        CODEX_CLI_GATEWAY_PROVIDER_ID,
        pin_catalog,
        pin_catalog,
    )?;
    if use_gateway_model_list {
        replace_model_menu_file(
            &mut files,
            &paths.codex_catalog,
            &selected_catalog,
            hidden_models,
        )?;
    }
    crate::account_mode::apply_gateway_files(
        paths,
        crate::gui_worker::ClientTarget::CodexCli,
        files,
    )
}

fn replace_model_menu_file(
    files: &mut [(PathBuf, Vec<u8>)],
    path: &std::path::Path,
    catalog_value: &Value,
    hidden_models: &[String],
) -> Result<()> {
    let menu = catalog::model_menu_catalog(catalog_value, hidden_models);
    if let Some((_, bytes)) = files.iter_mut().find(|(candidate, _)| candidate == path) {
        *bytes = pretty_json(&menu)?.into_bytes();
    }
    Ok(())
}

#[cfg(test)]
pub async fn install_claude_code_with_model_list(
    paths: &ClientPaths,
    options: &InstallOptions,
    api_key: &str,
    use_gateway_model_list: bool,
) -> Result<Value> {
    install_claude_code_with_hidden_models(paths, options, api_key, use_gateway_model_list, &[])
        .await
}

#[cfg(test)]
pub async fn install_claude_code_with_hidden_models(
    paths: &ClientPaths,
    options: &InstallOptions,
    api_key: &str,
    use_gateway_model_list: bool,
    hidden_models: &[String],
) -> Result<Value> {
    let gateway = fetch_gateway_catalog(options, api_key).await?;
    install_claude_code_with_catalog(
        paths,
        options,
        use_gateway_model_list,
        hidden_models,
        &gateway,
    )
}

pub fn install_claude_code_with_catalog(
    paths: &ClientPaths,
    options: &InstallOptions,
    use_gateway_model_list: bool,
    hidden_models: &[String],
    gateway: &Value,
) -> Result<Value> {
    catalog::validate_catalog(gateway).context(crate::i18n::tr(
        "网关模型目录尚未通过校验",
        "The gateway model catalog has not passed validation",
    ))?;
    if options.dry_run {
        return Ok(gateway.clone());
    }
    let slots = if use_gateway_model_list {
        select_model_slots(gateway, &SlotOverrides::default())?
    } else {
        select_claude_code_model_slots(gateway)?
    };
    let mut settings = merge_claude_settings(
        read_json_object(&paths.claude_settings, "Claude settings.json")?,
        &options.gateway_url,
        use_gateway_model_list,
        &slots,
        gateway,
    )?;
    if use_gateway_model_list {
        let menu = DesktopModelMenu::claude_code_catalog(
            gateway,
            catalog_source_label(gateway).unwrap_or_else(|| {
                GatewayIdentity::parse(&options.gateway_url)
                    .map(|gateway| gateway.source_label())
                    .unwrap_or_default()
            }),
        )?
        .with_hidden_models(hidden_models);
        settings["modelPicker"] = menu.claude_code_model_picker();
        if !hidden_models.is_empty() {
            // Discovery must not repopulate the curated picker from the full proxy catalog.
            settings["env"]["CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY"] = json!("0");
        }
    }
    let state = merge_claude_state(read_json_object(&paths.claude_state, "Claude state")?);
    let cache = if use_gateway_model_list {
        catalog::model_menu_catalog(gateway, hidden_models)
    } else {
        json!({"models": []})
    };
    crate::account_mode::apply_gateway_files(
        paths,
        crate::gui_worker::ClientTarget::ClaudeCode,
        vec![
            (
                paths.claude_settings.clone(),
                pretty_json(&settings)?.into_bytes(),
            ),
            (
                paths.claude_state.clone(),
                pretty_json(&state)?.into_bytes(),
            ),
            (
                paths.claude_model_cache.clone(),
                pretty_json(&cache)?.into_bytes(),
            ),
        ],
    )?;
    Ok(gateway.clone())
}

/// Persist only the loopback credential. Reopening Claude Code from another
/// terminal must not depend on the environment of the original launcher shell.
pub fn save_claude_runtime_endpoint(paths: &ClientPaths, url: &str, token: &str) -> Result<()> {
    let settings = merge_claude_runtime_endpoint(
        read_json_object(&paths.claude_settings, "Claude settings.json")?,
        url,
        token,
    )?;
    crate::account_mode::apply_gateway_files(
        paths,
        crate::gui_worker::ClientTarget::ClaudeCode,
        vec![(
            paths.claude_settings.clone(),
            pretty_json(&Value::Object(settings))?.into_bytes(),
        )],
    )
}

fn merge_claude_runtime_endpoint(
    mut settings: Map<String, Value>,
    url: &str,
    token: &str,
) -> Result<Map<String, Value>> {
    let env = settings
        .entry("env")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("Claude settings.json 的 env 不是 JSON 对象")?;
    #[cfg(any(windows, target_os = "macos"))]
    for key in ["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"] {
        env.remove(key);
    }
    for (name, value) in [
        ("ANTHROPIC_BASE_URL", url),
        ("ANTHROPIC_AUTH_TOKEN", token),
        ("CLAUDE_GATEWAY_ALLOW_LOOPBACK", "1"),
    ] {
        env.insert(name.to_owned(), Value::String(value.to_owned()));
    }
    #[cfg(target_os = "macos")]
    for (name, value) in crate::platform::CLAUDE_GATEWAY_MODEL_POLICY {
        env.insert(name.to_owned(), Value::String(value.to_owned()));
    }
    let no_proxy = env
        .get("NO_PROXY")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| std::env::var("NO_PROXY").ok())
        .unwrap_or_default();
    let mut bypass = no_proxy
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for local in ["127.0.0.1", "localhost", "::1"] {
        if !bypass.iter().any(|value| value.eq_ignore_ascii_case(local)) {
            bypass.push(local.to_owned());
        }
    }
    env.insert("NO_PROXY".into(), Value::String(bypass.join(",")));
    Ok(settings)
}

pub async fn install(paths: &ClientPaths, options: &InstallOptions, api_key: &str) -> Result<()> {
    validate_gateway_url(&options.gateway_url)?;
    let gateway = fetch_gateway_catalog(options, api_key).await?;
    if options.dry_run {
        info!("dry-run: validated gateway catalog; no files were modified");
        return Ok(());
    }
    let config = parse_toml_or_empty(&read_optional(&paths.codex_config)?, &paths.codex_config)?;
    let provider = choose_provider_id(&config, &paths.codex_sessions);
    let mut files = merged_codex_files(paths, options, api_key, &gateway, &provider, false, true)?;
    let slots = select_model_slots(&gateway, &SlotOverrides::default())?;
    let settings = merge_claude_settings(
        read_json_object(&paths.claude_settings, "Claude settings.json")?,
        &options.gateway_url,
        true,
        &slots,
        &gateway,
    )?;
    let state = merge_claude_state(read_json_object(&paths.claude_state, "Claude state")?);
    files.extend([
        (
            paths.claude_settings.clone(),
            pretty_json(&settings)?.into_bytes(),
        ),
        (
            paths.claude_state.clone(),
            pretty_json(&state)?.into_bytes(),
        ),
        (
            paths.claude_model_cache.clone(),
            pretty_json(&gateway)?.into_bytes(),
        ),
    ]);
    apply_files(files)?;
    if options.set_user_environment {
        platform::set_user_environment(&[
            ("OPENAI_BASE_URL", options.gateway_url.trim_end_matches('/')),
            (
                "ANTHROPIC_BASE_URL",
                options.gateway_url.trim_end_matches('/'),
            ),
        ])?;
    }
    Ok(())
}

pub async fn refresh_catalog(
    paths: &ClientPaths,
    gateway_url: &str,
    api_key: &str,
    dry_run: bool,
) -> Result<()> {
    let options = InstallOptions {
        gateway_url: gateway_url.to_owned(),
        default_model: None,
        dry_run,
        set_user_environment: false,
    };
    let value = fetch_gateway_catalog(&options, api_key).await?;
    if dry_run {
        return Ok(());
    }
    write_json(&paths.codex_catalog, &value)
}
#[allow(dead_code)]
fn _example() -> Value {
    json!({})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::net::TcpListener;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "little-mungbean-client-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn remove_test_root(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }

    fn test_paths(root: &Path) -> ClientPaths {
        let codex_home = root.join("codex");
        let claude_home = root.join("claude");
        ClientPaths {
            user_home: root.to_path_buf(),
            codex_config: codex_home.join("config.toml"),
            codex_auth: codex_home.join("auth.json"),
            codex_global_state: codex_home.join(".codex-global-state.json"),
            codex_catalog: codex_home.join("model-catalogs/gateway.json"),
            codex_sessions: codex_home.join("sessions"),
            codex_home,
            claude_settings: claude_home.join("settings.json"),
            claude_state: root.join(".claude.json"),
            claude_model_cache: claude_home.join("cache/gateway-models.json"),
            claude_home,
        }
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn codex_takeover_overwrites_foreign_openai_and_keeps_history_aliases_routed() {
        let root = test_root("codex-takeover");
        let paths = test_paths(&root);
        let mut doc: DocumentMut = concat!(
            "model_provider='openai'\nprofile='work'\n",
            "[model_providers.openai]\nname='Other router'\nbase_url='https://api.openai.com/v1'\nenv_key='FOREIGN_KEY'\nexperimental_bearer_token='foreign'\nhttp_headers={Authorization='foreign'}\nquery_params={key='foreign'}\n",
            "[model_providers.custom]\nname='OpenAI'\nwire_api='responses'\nrequires_openai_auth=true\n",
            "[model_providers.unrelated]\nname='Unrelated'\nbase_url='https://unrelated.invalid/v1'\n",
            "[profiles.work]\nmodel_provider='unrelated'\nmodel_catalog_json='foreign.json'\nmodel_reasoning_effort='high'\n",
            "[projects.demo]\ntrust_level='trusted'\n"
        ).parse().unwrap();
        for (url, pin) in [
            ("http://127.0.0.1:54321/", false),
            ("http://127.0.0.1:54322", true),
        ] {
            doc = merge_codex_config(doc, "openai", "gpt-test", &paths.codex_catalog, url, pin)
                .unwrap()
                .parse()
                .unwrap();
            assert!(doc["model_providers"].get("openai").is_none());
            assert_eq!(
                doc["openai_base_url"].as_str(),
                Some(url.trim_end_matches('/'))
            );
            for id in ["custom"] {
                assert_eq!(
                    doc["model_providers"][id]["base_url"].as_str(),
                    Some(url.trim_end_matches('/'))
                );
                for key in [
                    "env_key",
                    "experimental_bearer_token",
                    "http_headers",
                    "query_params",
                ] {
                    assert!(doc["model_providers"][id].get(key).is_none());
                }
            }
            assert_eq!(
                doc["profiles"]["work"]["model_provider"].as_str(),
                Some("openai")
            );
            assert_eq!(
                doc["profiles"]["work"]["model_reasoning_effort"].as_str(),
                Some("high")
            );
            assert_eq!(
                doc["profiles"]["work"].get("model_catalog_json").is_some(),
                pin
            );
            assert_eq!(
                doc["model_providers"]["unrelated"]["base_url"].as_str(),
                Some("https://unrelated.invalid/v1")
            );
            backup::atomic_write(&paths.codex_config, doc.to_string().as_bytes()).unwrap();
            backup::atomic_write(
                &paths.codex_auth,
                br#"{"auth_mode":"apikey","OPENAI_API_KEY":"local"}"#,
            )
            .unwrap();
            assert!(codex_gateway_configuration_matches(&paths, url, "local").unwrap());
            doc["model_providers"]["openai"] = Item::Table(Table::new());
            doc["model_providers"]["openai"]["base_url"] = value("https://api.openai.com/v1");
            backup::atomic_write(&paths.codex_config, doc.to_string().as_bytes()).unwrap();
            assert!(!codex_gateway_configuration_matches(&paths, url, "local").unwrap());
        }
        remove_test_root(&root);
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn claude_explicit_takeover_overwrites_foreign_credentials_and_preserves_preferences() {
        let mut settings = json!({
            "env": {"ANTHROPIC_BASE_URL":"http://127.0.0.1:1234", "ANTHROPIC_AUTH_TOKEN":"foreign", "ANTHROPIC_API_KEY":"foreign-api", "CLAUDE_CODE_OAUTH_TOKEN":"foreign-oauth", "NO_PROXY":"company.invalid", "KEEP":"yes"},
            "theme":"light", "permissions":{"allow":["Read"]}, "modelPicker":{"options":["selected"]}
        }).as_object().unwrap().clone();
        for (url, token) in [
            ("http://127.0.0.1:54321", "first"),
            ("http://127.0.0.1:54322", "second"),
        ] {
            settings = merge_claude_runtime_endpoint(settings, url, token).unwrap();
            let env = &settings["env"];
            assert_eq!(env["ANTHROPIC_BASE_URL"], url);
            assert_eq!(env["ANTHROPIC_AUTH_TOKEN"], token);
            assert!(env.get("ANTHROPIC_API_KEY").is_none());
            assert!(env.get("CLAUDE_CODE_OAUTH_TOKEN").is_none());
            assert_eq!(env["KEEP"], "yes");
            let bypass = env["NO_PROXY"]
                .as_str()
                .unwrap()
                .split(',')
                .collect::<Vec<_>>();
            assert_eq!(bypass, ["company.invalid", "127.0.0.1", "localhost", "::1"]);
            assert_eq!(settings["theme"], "light");
            assert_eq!(settings["permissions"]["allow"][0], "Read");
            assert_eq!(settings["modelPicker"]["options"][0], "selected");
        }
    }

    #[test]
    fn claude_reopen_settings_keep_picker_and_save_only_local_credentials() {
        let root = test_root("claude-reopen");
        let paths = test_paths(&root);
        backup::atomic_write(
            &paths.claude_settings,
            br#"{"modelPicker":{"options":["a"]},"env":{"KEEP":"yes"},"unrelated":true}"#,
        )
        .unwrap();
        save_claude_runtime_endpoint(&paths, "http://127.0.0.1:12345", "local-only-token").unwrap();
        let settings: Value =
            serde_json::from_slice(&fs::read(&paths.claude_settings).unwrap()).unwrap();
        assert_eq!(settings.pointer("/modelPicker/options/0").unwrap(), "a");
        assert_eq!(settings.pointer("/env/KEEP").unwrap(), "yes");
        assert_eq!(
            settings.pointer("/env/ANTHROPIC_BASE_URL").unwrap(),
            "http://127.0.0.1:12345"
        );
        assert_eq!(
            settings.pointer("/env/ANTHROPIC_AUTH_TOKEN").unwrap(),
            "local-only-token"
        );
        assert_eq!(settings["unrelated"], true);
        remove_test_root(&root);
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn claude_gateway_configuration_match_checks_route_token_and_proxy_bypass() {
        let root = test_root("claude-route-check");
        let paths = test_paths(&root);
        backup::atomic_write(
            &paths.claude_settings,
            br#"{"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:12345","ANTHROPIC_AUTH_TOKEN":"local-token","CLAUDE_GATEWAY_ALLOW_LOOPBACK":"1","NO_PROXY":"127.0.0.1,localhost,::1"}}"#,
        )
        .unwrap();
        assert!(
            claude_code_gateway_configuration_matches(
                &paths,
                "http://127.0.0.1:12345/",
                "local-token"
            )
            .unwrap()
        );

        backup::atomic_write(
            &paths.claude_settings,
            br#"{"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:54321","ANTHROPIC_AUTH_TOKEN":"other-token","CLAUDE_GATEWAY_ALLOW_LOOPBACK":"1","NO_PROXY":"127.0.0.1,localhost,::1"}}"#,
        )
        .unwrap();
        assert!(
            !claude_code_gateway_configuration_matches(
                &paths,
                "http://127.0.0.1:12345",
                "local-token"
            )
            .unwrap()
        );

        backup::atomic_write(
            &paths.claude_settings,
            br#"{"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:12345","ANTHROPIC_AUTH_TOKEN":"local-token","CLAUDE_GATEWAY_ALLOW_LOOPBACK":"1"}}"#,
        )
        .unwrap();
        assert!(
            !claude_code_gateway_configuration_matches(
                &paths,
                "http://127.0.0.1:12345",
                "local-token"
            )
            .unwrap()
        );
        remove_test_root(&root);
    }

    #[test]
    fn gateway_auth_explicitly_selects_api_key_and_does_not_mix_oauth_tokens() {
        let account = serde_json::from_value(
            json!({"auth_mode":"chatgpt", "tokens":{"refresh_token":"private"},
            "last_refresh":"old", "unrelated":true}),
        )
        .unwrap();
        let auth = merge_codex_auth(account, "local-gateway-token");
        assert_eq!(auth["auth_mode"], "apikey");
        assert_eq!(auth["OPENAI_API_KEY"], "local-gateway-token");
        assert!(auth.get("tokens").is_none());
        assert!(auth.get("last_refresh").is_none());
        assert_eq!(auth["unrelated"], true);
    }

    fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn visit(root: &Path, current: &Path, output: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in fs::read_dir(current).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(root, &path, output);
                } else {
                    output.insert(
                        path.strip_prefix(root).unwrap().to_path_buf(),
                        fs::read(path).unwrap(),
                    );
                }
            }
        }
        let mut output = BTreeMap::new();
        visit(root, root, &mut output);
        output
    }

    #[test]
    fn codex_default_catalog_only_marks_exact_models_available_from_gateway() {
        let bundled = json!({
            "models": [
                {
                    "slug": "gpt-5.6-sol",
                    "description": "Latest frontier agentic coding model.",
                    "visibility": "list",
                    "supported_in_api": true
                },
                {
                    "slug": "gpt-5.6-terra",
                    "description": "Balanced agentic coding model for everyday work.",
                    "visibility": "list",
                    "supported_in_api": true
                },
                {
                    "slug": "gpt-5.6-luna",
                    "description": "Fast and affordable agentic coding model.",
                    "visibility": "list",
                    "supported_in_api": true
                },
                {
                    "slug": "gpt-5.5",
                    "description": "Frontier model for complex coding, research, and real-world work.",
                    "visibility": "list",
                    "supported_in_api": true
                },
                {
                    "slug": "gpt-5.2",
                    "description": "Optimized for professional work and long-running agents.",
                    "visibility": "list",
                    "supported_in_api": true
                },
                {
                    "slug": "hidden-bundled",
                    "description": "Keep hidden metadata.",
                    "visibility": "hide",
                    "supported_in_api": true
                },
                {
                    "slug": "unsupported-bundled",
                    "description": "Keep unsupported metadata.",
                    "visibility": "list",
                    "supported_in_api": false
                }
            ]
        });
        let gateway = json!({
            "models": [
                {"slug": "gpt-5.6-sol", "visibility": "list", "supported_in_api": true},
                {"slug": "gpt-5.6-terra-preview", "visibility": "list", "supported_in_api": true},
                {"slug": "gpt-5.6-luna", "visibility": "hide", "supported_in_api": true},
                {"slug": "gpt-5.5", "visibility": "list", "supported_in_api": false},
                {"slug": "hidden-bundled", "visibility": "list", "supported_in_api": true},
                {"slug": "unsupported-bundled", "visibility": "list", "supported_in_api": true}
            ]
        });

        let annotated = annotate_codex_bundled_catalog_sources(
            bundled,
            &gateway,
            "https://gateway.example.com/v1/?token=secret#fragment",
        )
        .unwrap();
        let descriptions = annotated["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|model| {
                (
                    model["slug"].as_str().unwrap(),
                    model["description"].as_str().unwrap(),
                )
            })
            .collect::<BTreeMap<_, _>>();

        assert_eq!(
            descriptions["gpt-5.6-sol"],
            "Latest frontier agentic coding model. (From https://gateway.example.com/v1)"
        );
        assert_eq!(
            descriptions["gpt-5.6-terra"],
            "Balanced agentic coding model for everyday work."
        );
        assert_eq!(
            descriptions["gpt-5.6-luna"],
            "Fast and affordable agentic coding model."
        );
        assert_eq!(
            descriptions["gpt-5.5"],
            "Frontier model for complex coding, research, and real-world work."
        );
        assert_eq!(
            descriptions["gpt-5.2"],
            "Optimized for professional work and long-running agents."
        );
        assert_eq!(descriptions["hidden-bundled"], "Keep hidden metadata.");
        assert_eq!(
            descriptions["unsupported-bundled"],
            "Keep unsupported metadata."
        );
        assert!(
            !serde_json::to_string(&annotated)
                .unwrap()
                .contains("secret")
        );
    }

    #[test]
    fn codex_merge_preserves_unrelated_settings() {
        let raw = r#"
model_provider = "custom"
model = "old-model"
model_reasoning_effort = "high"

[mcp_servers.demo]
command = "demo.exe"

[projects.demo]
trust_level = "trusted"

[model_providers.custom]
name = "old"
base_url = "https://old.invalid"
wire_api = "responses"
env_key = "CC_SWITCH_KEY"
experimental_bearer_token = "stale-token"
http_headers = { Authorization = "Bearer stale-token" }
query_params = { key = "stale-token" }
"#;
        let doc = raw.parse::<DocumentMut>().unwrap();
        let merged = merge_codex_config(
            doc,
            "custom",
            "gpt-test",
            Path::new(r"C:\Users\Test\.codex\catalog.json"),
            "https://api.znnz.net/",
            true,
        )
        .unwrap();
        let parsed = merged.parse::<DocumentMut>().unwrap();
        assert_eq!(parsed["model_provider"].as_str(), Some("custom"));
        assert_eq!(parsed["model"].as_str(), Some("gpt-test"));
        assert_eq!(parsed["model_reasoning_effort"].as_str(), Some("high"));
        assert_eq!(
            parsed["mcp_servers"]["demo"]["command"].as_str(),
            Some("demo.exe")
        );
        assert_eq!(
            parsed["projects"]["demo"]["trust_level"].as_str(),
            Some("trusted")
        );
        assert_eq!(
            parsed["model_providers"]["custom"]["base_url"].as_str(),
            Some("https://api.znnz.net")
        );
        let provider = &parsed["model_providers"]["custom"];
        for key in [
            "env_key",
            "experimental_bearer_token",
            "http_headers",
            "query_params",
        ] {
            assert!(
                provider.get(key).is_none(),
                "stale field was retained: {key}"
            );
        }
    }

    #[test]
    fn custom_gateway_uses_its_host_as_provider_name() {
        let merged = merge_codex_config(
            DocumentMut::new(),
            CODEX_CLI_GATEWAY_PROVIDER_ID,
            "model-a",
            Path::new("catalog.json"),
            "https://gateway.example.com/v1",
            true,
        )
        .unwrap();
        let parsed = merged.parse::<DocumentMut>().unwrap();
        let provider = &parsed["model_providers"][CODEX_CLI_GATEWAY_PROVIDER_ID];
        assert_eq!(provider["name"].as_str(), Some("gateway.example.com"));
        assert_eq!(
            provider["base_url"].as_str(),
            Some("https://gateway.example.com/v1")
        );
    }

    #[test]
    fn codex_built_in_mode_pins_catalog_and_keeps_gateway_and_unrelated_settings() {
        let raw = r#"
model_provider = "custom"
model = "old-model"
model_catalog_json = "old-catalog.json"

[unrelated]
keep = true

[model_providers.custom]
name = "old"
base_url = "https://old.invalid"
wire_api = "responses"
"#;
        let merged = merge_codex_config(
            raw.parse().unwrap(),
            "custom",
            "gpt-test",
            Path::new("catalog.json"),
            "https://api.znnz.net/",
            true,
        )
        .unwrap();
        let parsed = merged.parse::<DocumentMut>().unwrap();
        assert_eq!(parsed["model_catalog_json"].as_str(), Some("catalog.json"));
        assert_eq!(
            parsed["model_providers"]["custom"]["base_url"].as_str(),
            Some("https://api.znnz.net")
        );
        assert_eq!(parsed["unrelated"]["keep"].as_bool(), Some(true));
    }

    #[test]
    fn claude_code_model_discovery_follows_selected_mode() {
        let slots = ModelSlots {
            haiku: "claude-haiku-test".to_owned(),
            sonnet: "claude-sonnet-test".to_owned(),
            opus: "claude-opus-test".to_owned(),
            fable: "claude-fable-test".to_owned(),
        };
        let gateway_catalog = json!({
            "models": [{
                "slug": "gpt-5.4-mini",
                "visibility": "list",
                "supported_in_api": true
            }]
        });
        for (use_gateway_model_list, expected) in [(false, "0"), (true, "1")] {
            let merged = merge_claude_settings(
                serde_json::from_value::<Map<String, Value>>(json!({
                    "theme": "dark",
                    "model": "opus",
                    "env": {
                        "KEEP_ME": "yes",
                        "ANTHROPIC_DEFAULT_OPUS_MODEL": "stale-opus",
                        "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME": "stale-opus",
                        "ANTHROPIC_DEFAULT_OPUS_MODEL_DESCRIPTION": "stale"
                    }
                }))
                .unwrap(),
                "https://api.znnz.net/",
                use_gateway_model_list,
                &slots,
                &gateway_catalog,
            )
            .unwrap();
            assert_eq!(
                merged.pointer("/env/CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY"),
                Some(&json!(expected))
            );
            assert_eq!(merged.pointer("/env/KEEP_ME"), Some(&json!("yes")));
            assert_eq!(merged.get("theme"), Some(&json!("dark")));
            assert_eq!(merged.get("model"), None);
            if use_gateway_model_list {
                assert_eq!(merged.pointer("/env/ANTHROPIC_BASE_URL"), None);
                assert_eq!(merged.pointer("/env/ANTHROPIC_DEFAULT_OPUS_MODEL"), None);
                assert_eq!(
                    merged.pointer("/env/ANTHROPIC_DEFAULT_OPUS_MODEL_NAME"),
                    None
                );
                assert_eq!(
                    merged.pointer("/modelPicker/replaceBuiltInOptions"),
                    Some(&json!(true))
                );
                let options = merged
                    .pointer("/modelPicker/options")
                    .and_then(Value::as_array)
                    .unwrap();
                assert_eq!(options.len(), 1);
                assert_eq!(options[0]["label"], "gpt-5.4-mini");
                assert_eq!(options[0]["description"], "From https://api.znnz.net");
                assert!(
                    options[0]["model"]
                        .as_str()
                        .unwrap()
                        .starts_with("claude-gateway-route-")
                );
            } else {
                assert_eq!(merged.get("modelPicker"), None);
                assert_eq!(
                    merged.pointer("/env/ANTHROPIC_BASE_URL"),
                    Some(&json!("https://api.znnz.net"))
                );
                assert_eq!(merged.pointer("/env/ANTHROPIC_DEFAULT_OPUS_MODEL"), None);
                assert_eq!(merged.pointer("/env/ANTHROPIC_DEFAULT_SONNET_MODEL"), None);
                assert_eq!(merged.pointer("/env/ANTHROPIC_DEFAULT_HAIKU_MODEL"), None);
                assert_eq!(merged.pointer("/env/ANTHROPIC_DEFAULT_FABLE_MODEL"), None);
                assert_eq!(
                    merged.pointer("/env/ANTHROPIC_DEFAULT_OPUS_MODEL_DESCRIPTION"),
                    None
                );
            }
        }
    }

    #[test]
    fn claude_code_default_slots_only_brand_models_visible_to_the_key() {
        let gateway_catalog = json!({
            "models": [{
                "slug": "claude-opus-5",
                "visibility": "list",
                "supported_in_api": true
            }]
        });
        let slots = select_claude_code_model_slots(&gateway_catalog).unwrap();
        let merged = merge_claude_settings(
            Map::new(),
            "https://api.znnz.net",
            false,
            &slots,
            &gateway_catalog,
        )
        .unwrap();

        assert_eq!(
            merged.pointer("/env/ANTHROPIC_DEFAULT_OPUS_MODEL"),
            Some(&json!("claude-opus-5"))
        );
        assert_eq!(
            merged.pointer("/env/ANTHROPIC_DEFAULT_OPUS_MODEL_NAME"),
            Some(&json!("Opus"))
        );
        assert_eq!(
            merged.pointer("/env/ANTHROPIC_DEFAULT_OPUS_MODEL_DESCRIPTION"),
            Some(&json!("From https://api.znnz.net"))
        );
        for family in ["FABLE", "SONNET", "HAIKU"] {
            assert_eq!(
                merged.pointer(&format!("/env/ANTHROPIC_DEFAULT_{family}_MODEL")),
                None
            );
            assert_eq!(
                merged.pointer(&format!(
                    "/env/ANTHROPIC_DEFAULT_{family}_MODEL_DESCRIPTION"
                )),
                None
            );
        }
    }

    #[test]
    fn built_in_openai_provider_uses_root_base_url() {
        let raw = r#"
model_provider = "openai"
[model_providers.openai]
name = "znnz.net"
base_url = "https://api.znnz.net"
wire_api = "responses"
"#;
        let merged = merge_codex_config(
            raw.parse().unwrap(),
            "openai",
            "gpt-test",
            Path::new("catalog.json"),
            "https://api.znnz.net",
            true,
        )
        .unwrap();
        let parsed = merged.parse::<DocumentMut>().unwrap();
        assert_eq!(
            parsed["openai_base_url"].as_str(),
            Some("https://api.znnz.net")
        );
        assert!(
            parsed
                .get("model_providers")
                .and_then(Item::as_table)
                .and_then(|providers| providers.get("openai"))
                .is_none()
        );
    }

    #[test]
    fn codex_cli_gateway_provider_disables_websockets_and_removes_openai_override() {
        let raw = r#"
model_provider = "openai"
model = "old-model"
openai_base_url = "https://old.invalid"
"#;
        let merged = merge_codex_config(
            raw.parse().unwrap(),
            CODEX_CLI_GATEWAY_PROVIDER_ID,
            "gpt-test",
            Path::new("catalog.json"),
            "https://api.znnz.net/",
            true,
        )
        .unwrap();
        let parsed = merged.parse::<DocumentMut>().unwrap();
        assert_eq!(
            parsed["model_provider"].as_str(),
            Some(CODEX_CLI_GATEWAY_PROVIDER_ID)
        );
        assert!(parsed.get("openai_base_url").is_none());
        let provider = &parsed["model_providers"][CODEX_CLI_GATEWAY_PROVIDER_ID];
        assert_eq!(provider["base_url"].as_str(), Some("https://api.znnz.net"));
        assert_eq!(provider["wire_api"].as_str(), Some("responses"));
        assert_eq!(provider["requires_openai_auth"].as_bool(), Some(true));
        assert_eq!(provider["supports_websockets"].as_bool(), Some(false));
    }

    #[test]
    fn provider_selection_preserves_current_or_historical_provider() {
        let root = test_root("provider-history");
        let sessions = root.join("sessions/nested");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(
            sessions.join("one.jsonl"),
            "{\"model_provider\":\"custom\"}\n{\"type\":\"message\"}\n",
        )
        .unwrap();
        fs::write(
            sessions.join("two.jsonl"),
            "{\"model_provider\":\"custom\"}\n",
        )
        .unwrap();
        fs::write(
            sessions.join("three.jsonl"),
            "{\"model_provider\":\"openai\"}\n",
        )
        .unwrap();

        let legacy = "model_provider = \"znnz\"\n"
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(
            choose_provider_id(&legacy, &root.join("sessions")),
            "custom"
        );
        let current = "model_provider = \"keep-me\"\n"
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(
            choose_provider_id(&current, &root.join("sessions")),
            "keep-me"
        );
        let empty = DocumentMut::new();
        assert_eq!(choose_provider_id(&empty, &root.join("missing")), "openai");
        remove_test_root(&root);
    }

    #[test]
    fn global_state_merge_changes_only_model_menu_keys() {
        let mut object = Map::new();
        object.insert("projects".into(), json!({"keep": [1, 2, 3]}));
        object.insert("tasks".into(), json!([{"id": "old-task"}]));
        object.insert("composer-model-picker-menu-view-v1".into(), json!("legacy"));
        object.insert(
            "electron-persisted-atom-state".into(),
            json!({"another-setting": true}),
        );
        let merged = merge_codex_global_state(object).unwrap();
        assert_eq!(merged["projects"], json!({"keep": [1, 2, 3]}));
        assert_eq!(merged["tasks"], json!([{"id": "old-task"}]));
        assert_eq!(
            merged.pointer("/electron-persisted-atom-state/another-setting"),
            Some(&json!(true))
        );
        assert_eq!(
            merged.pointer("/electron-persisted-atom-state/composer-model-picker-menu-view-v1"),
            Some(&json!("advanced"))
        );
        assert!(merged.get("composer-model-picker-menu-view-v1").is_none());
    }

    #[test]
    fn invalid_structures_are_rejected_instead_of_overwritten() {
        let invalid_toml = "[broken";
        assert!(parse_toml_or_empty(invalid_toml, Path::new("config.toml")).is_err());

        let root = test_root("invalid-json");
        let json_path = root.join("state.json");
        fs::write(&json_path, "not-json").unwrap();
        assert!(read_json_object(&json_path, "test state").is_err());

        let wrong_provider_type = "model_providers = \"do-not-replace\"\n"
            .parse::<DocumentMut>()
            .unwrap();
        assert!(
            merge_codex_config(
                wrong_provider_type,
                "custom",
                "gpt-test",
                Path::new("catalog.json"),
                "https://api.znnz.net",
                true,
            )
            .is_err()
        );
        remove_test_root(&root);
    }

    #[test]
    fn key_fingerprint_never_contains_the_key() {
        let key = "sk-super-secret-value-that-must-not-leak";
        let fingerprint = key_fingerprint(key);
        assert!(!fingerprint.contains(key));
        assert!(!fingerprint.contains("super-secret"));
        assert!(fingerprint.contains("SHA256="));
    }

    #[test]
    fn launcher_codex_cli_profile_is_isolated_from_desktop_home() {
        let paths = ClientPaths::launcher_codex_cli_profile().unwrap();
        assert!(
            paths
                .codex_home
                .ends_with(Path::new("Little-Mungbean/profiles/codex-cli"))
        );
        assert_ne!(paths.codex_home, ClientPaths::default_codex_home().unwrap());
        assert_eq!(paths.codex_config, paths.codex_home.join("config.toml"));
    }

    #[test]
    fn custom_claude_home_keeps_state_and_backups_in_the_isolated_root() {
        let root = test_root("claude-root");
        let claude_home = root.join("profile/.claude");
        let paths = ClientPaths::discover(None, Some(claude_home.clone())).unwrap();
        assert_eq!(paths.claude_home, claude_home);
        assert_eq!(paths.claude_state, root.join("profile/.claude.json"));
        assert_eq!(paths.claude_state, root.join("profile/.claude.json"));
        remove_test_root(&root);
    }

    #[tokio::test]
    async fn dry_run_leaves_every_file_unchanged() {
        let root = test_root("dry-run");
        let paths = test_paths(&root);
        fs::create_dir_all(paths.codex_sessions.join("2026")).unwrap();
        fs::create_dir_all(&paths.claude_home).unwrap();
        fs::write(
            &paths.codex_config,
            "model_provider = \"custom\"\nmodel = \"gpt-test\"\n[unrelated]\nkeep = true\n",
        )
        .unwrap();
        fs::write(&paths.codex_auth, "{\"OPENAI_API_KEY\":\"old\"}\n").unwrap();
        fs::write(
            &paths.codex_global_state,
            "{\"projects\":{\"keep\":true}}\n",
        )
        .unwrap();
        fs::write(&paths.claude_settings, "{\"theme\":\"dark\"}\n").unwrap();
        fs::write(&paths.claude_state, "{\"history\":[1,2]}\n").unwrap();
        fs::write(
            paths.codex_sessions.join("2026/task.jsonl"),
            "{\"model_provider\":\"custom\"}\n",
        )
        .unwrap();
        let before = snapshot_tree(&root);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
            }
            let body =
                r#"{"models":[{"slug":"gpt-test","visibility":"list","supported_in_api":true}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });

        let options = InstallOptions {
            gateway_url: format!("http://{address}"),
            default_model: Some("gpt-test".into()),
            dry_run: true,
            set_user_environment: false,
        };
        install(&paths, &options, "sk-dry-run-secret")
            .await
            .unwrap();
        server.join().unwrap();
        assert_eq!(snapshot_tree(&root), before);
        remove_test_root(&root);
    }

    fn start_catalog_server() -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
            }
            let body =
                r#"{"models":[{"slug":"gpt-test","visibility":"list","supported_in_api":true}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        (format!("http://{address}"), server)
    }

    fn scoped_options(gateway_url: String) -> InstallOptions {
        InstallOptions {
            gateway_url,
            default_model: Some("gpt-test".into()),
            dry_run: false,
            set_user_environment: false,
        }
    }

    #[tokio::test]
    async fn prefetched_catalog_is_reused_for_menu_changes_without_another_request() {
        for target in ["codex-cli", "codex-desktop", "claude-code"] {
            let root = test_root(&format!("hidden-menu-{target}"));
            let paths = test_paths(&root);
            let (url, server) = start_catalog_server();
            let options = scoped_options(url);
            let gateway = fetch_gateway_catalog(&options, "test-key").await.unwrap();
            // The upstream exits after its single response. Both subsequent
            // configuration writes must use the preflight snapshot offline.
            server.join().unwrap();
            for hidden in [vec!["gpt-test".to_owned()], Vec::new()] {
                match target {
                    "codex-cli" => install_codex_cli_with_catalog(
                        &paths, &options, "test-key", true, &hidden, &gateway,
                    )
                    .unwrap(),
                    "codex-desktop" => install_codex_with_catalog(
                        &paths, &options, "test-key", true, &hidden, &gateway,
                    )
                    .unwrap(),
                    _ => {
                        let upstream = install_claude_code_with_catalog(
                            &paths, &options, true, &hidden, &gateway,
                        )
                        .unwrap();
                        assert_eq!(catalog::visible_slugs(&upstream), vec!["gpt-test"]);
                        let settings: Value =
                            serde_json::from_slice(&fs::read(&paths.claude_settings).unwrap())
                                .unwrap();
                        assert_eq!(
                            settings["modelPicker"]["options"].as_array().unwrap().len(),
                            usize::from(hidden.is_empty())
                        );
                        assert_eq!(
                            settings["env"]["CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY"],
                            if hidden.is_empty() { "1" } else { "0" }
                        );
                    }
                }
                let path = if target == "claude-code" {
                    &paths.claude_model_cache
                } else {
                    &paths.codex_catalog
                };
                let menu = catalog::read_model_menu_catalog(path).unwrap();
                assert_eq!(
                    catalog::visible_slugs(&menu).len(),
                    usize::from(hidden.is_empty())
                );
                assert_eq!(menu["models"].as_array().unwrap().len(), 1);
                assert_eq!(menu["models"][0]["supported_in_api"], true);
            }
            remove_test_root(&root);
        }
    }

    #[tokio::test]
    async fn codex_only_install_does_not_touch_claude_code_files() {
        let root = test_root("codex-only");
        let paths = test_paths(&root);
        fs::create_dir_all(&paths.claude_home).unwrap();
        fs::create_dir_all(paths.claude_model_cache.parent().unwrap()).unwrap();
        fs::write(&paths.claude_settings, "{\"theme\":\"dark\"}\n").unwrap();
        fs::write(&paths.claude_state, "{\"history\":[1,2]}\n").unwrap();
        fs::write(&paths.claude_model_cache, "claude-cache").unwrap();
        let before_settings = fs::read(&paths.claude_settings).unwrap();
        let before_state = fs::read(&paths.claude_state).unwrap();
        let before_cache = fs::read(&paths.claude_model_cache).unwrap();
        let (gateway_url, server) = start_catalog_server();

        install_codex_with_model_list(&paths, &scoped_options(gateway_url), "sk-codex-only", true)
            .await
            .unwrap();
        server.join().unwrap();

        assert!(paths.codex_config.is_file());
        assert_eq!(fs::read(&paths.claude_settings).unwrap(), before_settings);
        assert_eq!(fs::read(&paths.claude_state).unwrap(), before_state);
        assert_eq!(fs::read(&paths.claude_model_cache).unwrap(), before_cache);
        remove_test_root(&root);
    }

    #[tokio::test]
    async fn codex_cli_install_forces_dedicated_non_websocket_provider() {
        let root = test_root("codex-cli-provider");
        let paths = test_paths(&root);
        fs::create_dir_all(&paths.codex_home).unwrap();
        fs::write(
            &paths.codex_config,
            "model_provider = \"openai\"\nmodel = \"gpt-test\"\nopenai_base_url = \"https://old.invalid\"\n",
        )
        .unwrap();
        let (gateway_url, server) = start_catalog_server();

        install_codex_cli_with_model_list(
            &paths,
            &scoped_options(gateway_url),
            "sk-codex-cli",
            true,
        )
        .await
        .unwrap();
        server.join().unwrap();

        let config = fs::read_to_string(&paths.codex_config).unwrap();
        let parsed = config.parse::<DocumentMut>().unwrap();
        assert_eq!(
            parsed["model_provider"].as_str(),
            Some(CODEX_CLI_GATEWAY_PROVIDER_ID)
        );
        assert!(parsed.get("openai_base_url").is_none());
        assert_eq!(
            parsed["model_providers"][CODEX_CLI_GATEWAY_PROVIDER_ID]["supports_websockets"]
                .as_bool(),
            Some(false)
        );
        remove_test_root(&root);
    }

    #[tokio::test]
    async fn codex_cli_built_in_install_uses_codex_default_model_list() {
        let root = test_root("codex-cli-built-in");
        let paths = test_paths(&root);
        fs::create_dir_all(paths.codex_catalog.parent().unwrap()).unwrap();
        fs::write(
            &paths.codex_config,
            format!(
                "model_provider = \"openai\"\nmodel = \"gpt-test\"\nmodel_catalog_json = {:?}\n",
                paths.codex_catalog.to_string_lossy().replace('\\', "/")
            ),
        )
        .unwrap();
        fs::write(
            &paths.codex_catalog,
            r#"{"models":[{"slug":"old-gateway"}]}"#,
        )
        .unwrap();
        let (gateway_url, server) = start_catalog_server();

        install_codex_cli_with_model_list(
            &paths,
            &scoped_options(gateway_url.clone()),
            "sk-codex-cli-built-in",
            false,
        )
        .await
        .unwrap();
        server.join().unwrap();

        let config = fs::read_to_string(&paths.codex_config).unwrap();
        let parsed = config.parse::<DocumentMut>().unwrap();
        if cfg!(any(windows, target_os = "macos")) {
            assert_eq!(
                parsed["model_catalog_json"].as_str(),
                Some(
                    paths
                        .codex_catalog
                        .to_string_lossy()
                        .replace('\\', "/")
                        .as_str()
                )
            );
            let menu = catalog::read_model_menu_catalog(&paths.codex_catalog).unwrap();
            assert_eq!(catalog::visible_slugs(&menu), vec!["gpt-test"]);
            assert_eq!(
                menu["models"][0]["description"].as_str(),
                Some(format!("Bundled test model. (From {gateway_url})").as_str())
            );
        } else {
            assert!(parsed.get("model_catalog_json").is_none());
            assert_eq!(
                fs::read(&paths.codex_catalog).unwrap(),
                br#"{"models":[{"slug":"old-gateway"}]}"#
            );
        }
        assert_eq!(
            parsed["model_provider"].as_str(),
            Some(CODEX_CLI_GATEWAY_PROVIDER_ID)
        );
        assert_eq!(parsed["model"].as_str(), Some("gpt-test"));
        remove_test_root(&root);
    }

    #[tokio::test]
    #[cfg(any(windows, target_os = "macos"))]
    async fn codex_cli_default_model_without_gateway_match_keeps_its_description() {
        let root = test_root("codex-cli-source-without-match");
        let paths = test_paths(&root);
        let options = scoped_options("https://gateway.example.com/v1".to_owned());
        let gateway = json!({
            "models": [
                {"slug":"other-model", "visibility":"list", "supported_in_api":true}
            ]
        });
        install_codex_cli_with_catalog(&paths, &options, "test-key", false, &[], &gateway).unwrap();
        let menu = catalog::read_model_menu_catalog(&paths.codex_catalog).unwrap();
        assert_eq!(catalog::visible_slugs(&menu), vec!["gpt-test"]);
        assert_eq!(menu["models"][0]["description"], "Bundled test model.");
        remove_test_root(&root);
    }

    #[test]
    #[cfg(any(windows, target_os = "macos"))]
    fn codex_cli_default_model_sources_refresh_from_upstream_without_duplicate_labels() {
        let root = test_root("codex-cli-source-refresh");
        let paths = test_paths(&root);
        let options = scoped_options("http://127.0.0.1:54321".to_owned());
        for source in [
            "From https://gateway.example.com/v1",
            "From https://gateway.example.com/v1",
            "From https://other.example.com/api",
        ] {
            let gateway = json!({
                "models": [{
                    "slug": "gpt-test",
                    "description": source,
                    "visibility": "list",
                    "supported_in_api": true
                }]
            });
            install_codex_cli_with_catalog(&paths, &options, "test-key", false, &[], &gateway)
                .unwrap();
            let config = fs::read_to_string(&paths.codex_config)
                .unwrap()
                .parse::<DocumentMut>()
                .unwrap();
            let catalog_path = config["model_catalog_json"].as_str().unwrap();
            let menu = catalog::read_model_menu_catalog(Path::new(catalog_path)).unwrap();
            assert_eq!(catalog::visible_slugs(&menu), vec!["gpt-test"]);
            assert_eq!(menu["models"][0]["display_name"], "GPT Test");
            assert_eq!(
                menu["models"][0]["description"],
                format!("Bundled test model. ({source})")
            );
            assert_eq!(
                config["model_providers"][CODEX_CLI_GATEWAY_PROVIDER_ID]["base_url"].as_str(),
                Some(options.gateway_url.as_str())
            );
        }
        remove_test_root(&root);
    }

    #[tokio::test]
    async fn codex_desktop_built_in_install_does_not_require_cli_catalog() {
        let root = test_root("codex-desktop-built-in");
        let paths = test_paths(&root);
        fs::create_dir_all(paths.codex_catalog.parent().unwrap()).unwrap();
        fs::write(
            &paths.codex_config,
            format!(
                "model_provider = \"openai\"\nmodel = \"gpt-test\"\nmodel_catalog_json = {:?}\n[unrelated]\nkeep = true\n",
                paths.codex_catalog.to_string_lossy().replace('\\', "/")
            ),
        )
        .unwrap();
        let old_catalog = br#"{"models":[{"slug":"old-gateway"}]}"#;
        fs::write(&paths.codex_catalog, old_catalog).unwrap();
        let (gateway_url, server) = start_catalog_server();

        install_codex_with_model_list(
            &paths,
            &scoped_options(gateway_url.clone()),
            "sk-codex-desktop-built-in",
            false,
        )
        .await
        .unwrap();
        server.join().unwrap();

        let config = fs::read_to_string(&paths.codex_config).unwrap();
        let parsed = config.parse::<DocumentMut>().unwrap();
        assert!(parsed.get("model_catalog_json").is_none());
        assert_eq!(parsed["model"].as_str(), Some("gpt-test"));
        assert_eq!(
            parsed["openai_base_url"].as_str(),
            Some(gateway_url.as_str())
        );
        assert_eq!(parsed["unrelated"]["keep"].as_bool(), Some(true));
        assert_eq!(fs::read(&paths.codex_catalog).unwrap(), old_catalog);
        let auth: Value = serde_json::from_slice(&fs::read(&paths.codex_auth).unwrap()).unwrap();
        assert_eq!(
            auth.get("OPENAI_API_KEY").and_then(Value::as_str),
            Some("sk-codex-desktop-built-in")
        );
        remove_test_root(&root);
    }

    #[tokio::test]
    async fn claude_code_only_install_does_not_touch_codex_files() {
        let root = test_root("claude-only");
        let paths = test_paths(&root);
        fs::create_dir_all(&paths.codex_home).unwrap();
        fs::create_dir_all(paths.codex_catalog.parent().unwrap()).unwrap();
        fs::write(&paths.codex_config, "model = \"keep-me\"\n").unwrap();
        fs::write(&paths.codex_auth, "{\"OPENAI_API_KEY\":\"keep-me\"}\n").unwrap();
        fs::write(&paths.codex_global_state, "{\"tasks\":[1]}\n").unwrap();
        fs::write(&paths.codex_catalog, "{\"models\":[]}\n").unwrap();
        let codex_model_cache = paths.codex_home.join("models_cache.json");
        fs::write(&codex_model_cache, "codex-cache").unwrap();
        let before = [
            (&paths.codex_config, fs::read(&paths.codex_config).unwrap()),
            (&paths.codex_auth, fs::read(&paths.codex_auth).unwrap()),
            (
                &paths.codex_global_state,
                fs::read(&paths.codex_global_state).unwrap(),
            ),
            (
                &paths.codex_catalog,
                fs::read(&paths.codex_catalog).unwrap(),
            ),
            (&codex_model_cache, fs::read(&codex_model_cache).unwrap()),
        ];
        let (gateway_url, server) = start_catalog_server();

        install_claude_code_with_model_list(
            &paths,
            &scoped_options(gateway_url),
            "sk-claude-only",
            true,
        )
        .await
        .unwrap();
        server.join().unwrap();

        assert!(paths.claude_settings.is_file());
        for (path, expected) in before {
            assert_eq!(
                fs::read(path).unwrap(),
                expected,
                "changed {}",
                path.display()
            );
        }
        remove_test_root(&root);
    }
}
