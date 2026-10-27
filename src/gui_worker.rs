use crate::catalog;
use crate::claude_desktop_proxy::{
    ClaudeDesktopModelMode, ClaudeDesktopProxy, DesktopModelMenu, SlotOverrides, select_model_slots,
};
use crate::config::{ClientPaths, InstallOptions};
use crate::gateway::GatewayIdentity;
use crate::i18n;
use crate::network_proxy;
use crate::{claude_desktop, config, desktop, local_gateway, platform};
use anyhow::{Context, Result};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use zeroize::Zeroizing;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ModelListMode {
    BuiltIn,
    #[default]
    Gateway,
}

impl ModelListMode {
    pub fn id(self) -> &'static str {
        match self {
            Self::BuiltIn => "built-in",
            Self::Gateway => "gateway",
        }
    }

    pub fn from_id(value: &str) -> Self {
        match value {
            "built-in" | "default" | "four-slots" => Self::BuiltIn,
            "gateway" | "full-catalog" => Self::Gateway,
            _ => Self::Gateway,
        }
    }

    fn uses_gateway_catalog(self) -> bool {
        self == Self::Gateway
    }

    fn claude_desktop_proxy_mode(self) -> ClaudeDesktopModelMode {
        match self {
            Self::BuiltIn => ClaudeDesktopModelMode::FourSlots,
            Self::Gateway => ClaudeDesktopModelMode::FullCatalog,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ClientTarget {
    CodexCli,
    CodexDesktop,
    ClaudeCode,
    ClaudeDesktop,
}

impl ClientTarget {
    pub const ALL: [Self; 4] = [
        Self::CodexCli,
        Self::CodexDesktop,
        Self::ClaudeCode,
        Self::ClaudeDesktop,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::CodexCli => "codex-cli",
            Self::CodexDesktop => "codex-desktop",
            Self::ClaudeCode => "claude-code",
            Self::ClaudeDesktop => "claude-desktop",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::CodexCli => "Codex CLI",
            Self::CodexDesktop => "Codex Desktop",
            Self::ClaudeCode => "Claude Code",
            Self::ClaudeDesktop => "Claude Desktop",
        }
    }

    pub fn from_id(value: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|target| target.id() == value)
            .unwrap_or(Self::CodexDesktop)
    }
}

pub async fn test_gateway(gateway_url: &str, api_key: String, fetch_models: bool) -> Result<()> {
    let api_key = Zeroizing::new(api_key);
    let gateway_url = gateway_url.trim().trim_end_matches('/');
    if fetch_models {
        println!(
            "{}",
            i18n::tr(
                "正在拉取网关模型目录……",
                "Fetching the gateway model catalog…"
            )
        );
    } else {
        println!(
            "{}",
            i18n::tr("正在测试接口连接……", "Testing the gateway connection…")
        );
    }
    let value = catalog::fetch_catalog(gateway_url, &api_key, "0.145.0").await?;
    if fetch_models {
        let visible = catalog::visible_slugs(&value);
        if let Some(log_path) = std::env::var_os("LITTLE_MUNGBEAN_RUNTIME_LOG_PATH") {
            let result_path = std::path::PathBuf::from(log_path).with_extension("models.json");
            crate::backup::atomic_write(&result_path, &serde_json::to_vec(&visible)?)?;
        }
        println!(
            "{} • {} {} • {}",
            i18n::tr("成功拉取", "Fetched"),
            visible.len(),
            i18n::tr("个可见模型", "visible models"),
            gateway_url
        );
    } else {
        println!("{} • {}", i18n::tr("成功连接", "Connected"), gateway_url);
    }
    Ok(())
}

pub async fn run(
    target: ClientTarget,
    gateway_url: &str,
    api_key: String,
    model_list_mode: ModelListMode,
    conversion_enabled: bool,
) -> Result<()> {
    let gateway_catalog =
        fetch_catalog_for_target(target, gateway_url, &api_key, model_list_mode).await?;
    run_with_catalog(
        target,
        gateway_url,
        api_key,
        model_list_mode,
        conversion_enabled,
        gateway_catalog,
    )
    .await
}

pub(crate) async fn fetch_catalog_for_target(
    target: ClientTarget,
    gateway_url: &str,
    api_key: &str,
    mode: ModelListMode,
) -> Result<serde_json::Value> {
    println!(
        "{}",
        i18n::tr(
            "正在拉取并校验网关模型目录……",
            "Fetching and validating the gateway model catalog…"
        )
    );
    let fetch_result = if target == ClientTarget::ClaudeDesktop {
        catalog::fetch_catalog(
            gateway_url,
            api_key,
            crate::claude_desktop::DISCOVERY_CLIENT_VERSION,
        )
        .await
    } else {
        config::fetch_gateway_catalog(&install_options(gateway_url), api_key).await
    };
    let gateway_catalog = fetch_result.map_err(catalog::with_catalog_fetch_context)?;
    validate_client_catalog(target, mode, &gateway_catalog).context(i18n::tr(
        "网关模型目录校验失败；未启动或重启客户端，也未修改配置。请检查模型目录格式及可用的文本模型",
        "Gateway model catalog validation failed; the client was not started or restarted and its configuration is unchanged. Check the catalog format and available text models.",
    ))?;
    if mode == ModelListMode::Gateway {
        let hidden = crate::gui_settings::hidden_models_for_configuration(gateway_url, api_key)?;
        catalog::validate_model_menu_catalog(&catalog::model_menu_catalog(
            &gateway_catalog,
            &hidden,
        ))?;
    }
    println!(
        "{} • {} {}",
        i18n::tr("模型目录校验通过", "Model catalog validated"),
        catalog::visible_slugs(&gateway_catalog).len(),
        i18n::tr("个可见模型", "visible models")
    );
    Ok(gateway_catalog)
}

fn validate_client_catalog(
    target: ClientTarget,
    mode: ModelListMode,
    gateway_catalog: &serde_json::Value,
) -> Result<()> {
    catalog::validate_catalog(gateway_catalog)?;
    match target {
        ClientTarget::CodexCli | ClientTarget::CodexDesktop => {
            catalog::select_default_model(gateway_catalog, None)?;
        }
        ClientTarget::ClaudeCode if mode == ModelListMode::BuiltIn => {
            crate::claude_desktop_proxy::select_claude_code_model_slots(gateway_catalog)?;
        }
        ClientTarget::ClaudeCode | ClientTarget::ClaudeDesktop => {
            select_model_slots(gateway_catalog, &SlotOverrides::default())?;
        }
    }
    Ok(())
}

pub(crate) async fn run_with_catalog(
    target: ClientTarget,
    gateway_url: &str,
    api_key: String,
    model_list_mode: ModelListMode,
    conversion_enabled: bool,
    gateway_catalog: serde_json::Value,
) -> Result<()> {
    validate_client_catalog(target, model_list_mode, &gateway_catalog)?;
    #[cfg(target_os = "macos")]
    if target == ClientTarget::ClaudeDesktop {
        crate::claude_desktop_registry::preflight()?;
    }
    let api_key = Zeroizing::new(api_key);
    let gateway = GatewayIdentity::parse(gateway_url)?;
    let hidden_models =
        crate::gui_settings::hidden_models_for_configuration(gateway_url, &api_key)?;
    let already_running = platform::running_client_processes()?
        .iter()
        .any(|(client, _)| *client == target);
    #[cfg(target_os = "macos")]
    if target == ClientTarget::ClaudeCode && already_running {
        anyhow::bail!(
            "{}",
            i18n::tr(
                "Claude Code 仍在运行；为避免覆盖当前终端配置，未写入新网关。请先关闭旧终端后重新接入",
                "Claude Code is still running; the gateway configuration was left untouched. Close the old terminal and reconnect."
            )
        );
    }
    let local =
        local_gateway::ensure_and_update(target, gateway_url, &api_key, conversion_enabled).await?;
    local_gateway::mark_reconfiguring(target);
    let local_gateway_url = local.base_url.clone();
    let local_api_key = local.client_token.clone();
    match target {
        ClientTarget::CodexCli => {
            let paths = ClientPaths::launcher_codex_cli_profile()?;
            install_codex_cli_configuration(
                &paths,
                &local_gateway_url,
                &local_api_key,
                model_list_mode,
                &hidden_models,
                &gateway_catalog,
            )
            .await?;
            if already_running {
                anyhow::bail!(
                    "{}",
                    i18n::tr(
                        "Codex CLI • 自动重启 • 失败!：已写入 Little Mungbean 独立配置；现有终端不能安全自动关闭，请退出后再次接入",
                        "Codex CLI • Auto-restart • failed!: the Little Mungbean profile has been saved. The existing terminal cannot be closed safely; exit it and connect again."
                    )
                );
            }
            let codex_home = paths.codex_home.to_string_lossy().into_owned();
            let working_directory = paths.codex_home.join("workspace");
            std::fs::create_dir_all(&working_directory).with_context(|| {
                format!(
                    "无法创建 Codex CLI 独立工作目录: {}",
                    working_directory.display()
                )
            })?;
            let proxy_environment = network_proxy::child_proxy_environment(&local_gateway_url)?;
            let mut environment = vec![("CODEX_HOME", codex_home.as_str())];
            environment.extend(
                proxy_environment
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.as_str())),
            );
            #[cfg(target_os = "macos")]
            if platform::refresh_codex_cli_app_server(&paths.codex_home, &environment).await? {
                println!(
                    "{}",
                    i18n::tr(
                        "已刷新 Codex CLI 后台服务，将使用刚写入的模型列表",
                        "Refreshed the Codex CLI background service to load the selected model list"
                    )
                );
            }
            let terminal_pid = platform::launch_terminal_client_with_environment_in_directory(
                &format!("{} - Codex CLI", gateway.display_name),
                "codex",
                &environment,
                &working_directory,
            )?;
            crate::background::remember_terminal(target, terminal_pid).await?;
            wait_for_terminal_ready(target, terminal_pid).await?;
            println!(
                "{}",
                client_started_message(target, terminal_pid, &gateway.base_url, model_list_mode)
            );
            println!(
                "{}：{}",
                i18n::tr("配置目录", "Configuration directory"),
                paths.codex_home.display()
            );
            println!(
                "{}：{}",
                i18n::tr("工作目录", "Working directory"),
                working_directory.display()
            );
            println!(
                "{}：{}",
                i18n::tr("监听地址", "Listening address"),
                local_gateway_url
            );
            println!("{}", connection_token_hint());
            wait_for_terminal_exit(ClientTarget::CodexCli, terminal_pid).await?;
        }
        ClientTarget::ClaudeCode => {
            let paths = ClientPaths::discover(None, None)?;
            let catalog = install_claude_code_configuration(
                &paths,
                &local_gateway_url,
                model_list_mode,
                &hidden_models,
                &gateway_catalog,
            )
            .await?;
            #[cfg(not(target_os = "macos"))]
            if already_running {
                anyhow::bail!(
                    "{}",
                    i18n::tr(
                        "Claude Code • 自动重启 • 失败!：配置已写入；现有终端不能安全自动关闭，请退出后再次接入",
                        "Claude Code • Auto-restart • failed!: configuration has been saved. The existing terminal cannot be closed safely; exit it and connect again."
                    )
                );
            }
            let proxy = if model_list_mode == ModelListMode::Gateway {
                let slots = select_model_slots(&catalog, &SlotOverrides::default())?;
                let source_label = catalog
                    .get("models")
                    .and_then(serde_json::Value::as_array)
                    .and_then(|models| {
                        models.iter().find_map(|model| {
                            model
                                .get("description")
                                .and_then(serde_json::Value::as_str)
                                .map(str::trim)
                                .filter(|description| description.starts_with("From "))
                                .map(ToOwned::to_owned)
                        })
                    })
                    .unwrap_or_else(|| gateway.source_label());
                let menu = DesktopModelMenu::claude_code_catalog(&catalog, source_label)?;
                Some(
                    ClaudeDesktopProxy::start(
                        &local_gateway_url,
                        local_api_key.clone(),
                        slots,
                        menu,
                    )
                    .await?,
                )
            } else {
                None
            };
            let runtime_base_url = proxy
                .as_ref()
                .map(ClaudeDesktopProxy::base_url)
                .unwrap_or_else(|| local_gateway_url.clone());
            let runtime_token = proxy
                .as_ref()
                .map_or(local_api_key.as_str(), ClaudeDesktopProxy::local_token);
            if crate::background::is_managed() {
                // Claude Code applies settings.env over its inherited environment.
                // Publish the new URL/token together before it reads those settings.
                config::save_claude_runtime_endpoint(&paths, &runtime_base_url, runtime_token)?;
            }
            #[cfg(any(windows, target_os = "macos"))]
            if crate::background::is_managed()
                && !crate::config::claude_code_gateway_configuration_matches(
                    &paths,
                    &runtime_base_url,
                    runtime_token,
                )?
            {
                config::save_claude_runtime_endpoint(&paths, &runtime_base_url, runtime_token)?;
            }
            #[cfg(any(windows, target_os = "macos"))]
            if crate::background::is_managed()
                && !config::claude_code_gateway_configuration_matches(
                    &paths,
                    &runtime_base_url,
                    runtime_token,
                )?
            {
                anyhow::bail!(
                    "{}",
                    i18n::tr(
                        "Claude Code 网关配置重复写入后仍未生效；客户端未启动，请查看运行日志。",
                        "Claude Code gateway settings did not persist after reapplying them; the client was not started. Check the runtime log."
                    )
                );
            }
            let proxy_environment = network_proxy::child_proxy_environment(&local_gateway_url)?;
            let mut environment = vec![
                ("ANTHROPIC_BASE_URL", runtime_base_url.as_str()),
                ("ANTHROPIC_AUTH_TOKEN", runtime_token),
                ("CLAUDE_GATEWAY_ALLOW_LOOPBACK", "1"),
            ];
            #[cfg(target_os = "macos")]
            environment.extend_from_slice(&platform::CLAUDE_GATEWAY_MODEL_POLICY);
            environment.extend(
                proxy_environment
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.as_str())),
            );
            let terminal_pid = platform::launch_terminal_client_with_environment(
                &format!("{} - Claude Code", gateway.display_name),
                "claude",
                &environment,
            )?;
            crate::background::remember_terminal(target, terminal_pid).await?;
            wait_for_terminal_ready(target, terminal_pid).await?;
            println!(
                "{}",
                client_started_message(target, terminal_pid, &gateway.base_url, model_list_mode)
            );
            println!(
                "{}：{}",
                i18n::tr("配置目录", "Configuration directory"),
                paths.claude_home.display()
            );
            println!(
                "{}：{}",
                i18n::tr("监听地址", "Listening address"),
                runtime_base_url
            );
            println!("{}", connection_token_hint());
            wait_for_terminal_exit(ClientTarget::ClaudeCode, terminal_pid).await?;
            if let Some(proxy) = proxy {
                if crate::background::is_managed() {
                    crate::background::retain_proxy(target, proxy);
                } else {
                    proxy.stop().await;
                }
            }
        }
        ClientTarget::CodexDesktop => {
            let paths = ClientPaths::discover(None, None)?;
            install_codex_configuration(
                &paths,
                &local_gateway_url,
                &local_api_key,
                model_list_mode,
                &hidden_models,
                &gateway_catalog,
            )
            .await?;
            // The old Electron process can keep CC Switch's inherited route
            // alive while it quits. Write once to validate the request, close
            // the old process, then write again so the final config cannot be
            // replaced during shutdown.
            #[cfg(not(any(windows, target_os = "macos")))]
            if already_running
                && !config::codex_gateway_configuration_matches(
                    &paths,
                    &local_gateway_url,
                    &local_api_key,
                )?
            {
                anyhow::bail!(
                    "{}",
                    i18n::tr(
                        "Codex Desktop 配置已被其他网关改写；未关闭客户端，请先退出其他网关后重试",
                        "Codex Desktop configuration was changed by another gateway; the client was left running. Exit the other gateway and retry"
                    )
                );
            }
            if already_running {
                desktop::close_for_gateway_attach(target)
                    .await
                    .context(i18n::tr(
                        "Codex Desktop • 自动重启 • 失败!",
                        "Codex Desktop • Auto-restart • failed!",
                    ))?;
                install_codex_configuration(
                    &paths,
                    &local_gateway_url,
                    &local_api_key,
                    model_list_mode,
                    &hidden_models,
                    &gateway_catalog,
                )
                .await?;
            }
            #[cfg(any(windows, target_os = "macos"))]
            if !config::codex_gateway_configuration_matches(
                &paths,
                &local_gateway_url,
                &local_api_key,
            )? {
                install_codex_configuration(
                    &paths,
                    &local_gateway_url,
                    &local_api_key,
                    model_list_mode,
                    &hidden_models,
                    &gateway_catalog,
                )
                .await?;
            }
            if !config::codex_gateway_configuration_matches(
                &paths,
                &local_gateway_url,
                &local_api_key,
            )? {
                anyhow::bail!(
                    "{}",
                    if cfg!(any(windows, target_os = "macos")) {
                        i18n::tr(
                            "Codex Desktop 网关配置重复写入后仍未生效；客户端未启动，请查看运行日志。",
                            "Codex Desktop gateway settings did not persist after reapplying them; the client was not started. Check the runtime log.",
                        )
                    } else {
                        i18n::tr(
                            "Codex Desktop 配置在启动前被其他网关改写；请先退出其他网关后重试",
                            "Codex Desktop configuration was changed by another gateway before launch. Exit the other gateway and retry",
                        )
                    }
                );
            }
            println!(
                "{}",
                i18n::tr(
                    "Codex Desktop 将使用 Little Mungbean 本地网关；不会清理其他网关进程",
                    "Codex Desktop will use the Little Mungbean local gateway; other gateway processes will not be terminated"
                )
            );
            if model_list_mode == ModelListMode::Gateway {
                desktop::launch_and_inject_with_catalog(
                    &paths,
                    &local_gateway_url,
                    &local_api_key,
                    &gateway.base_url,
                    &gateway_catalog,
                    &hidden_models,
                    None,
                )
                .await
                .context(i18n::tr(
                    "Codex Desktop • 自动重启 • 失败!",
                    "Codex Desktop • Auto-restart • failed!",
                ))?;
            } else {
                desktop::launch_without_injection(
                    &paths,
                    &local_gateway_url,
                    &local_api_key,
                    &gateway.base_url,
                    None,
                )
                .await
                .context(i18n::tr(
                    "Codex Desktop • 自动重启 • 失败!",
                    "Codex Desktop • Auto-restart • failed!",
                ))?;
            }
        }
        ClientTarget::ClaudeDesktop => {
            claude_desktop::launch_with_catalog(
                &local_gateway_url,
                &gateway.base_url,
                local_api_key,
                SlotOverrides::default(),
                model_list_mode.claude_desktop_proxy_mode(),
                &hidden_models,
                &gateway_catalog,
            )
            .await?;
        }
    }
    Ok(())
}

pub(crate) fn connection_token_hint() -> &'static str {
    i18n::tr(
        "连接令牌：可右击客户端复制",
        "Connection token: right-click the client card to copy",
    )
}

pub(crate) fn client_started_message(
    target: ClientTarget,
    pid: u32,
    gateway_url: &str,
    mode: ModelListMode,
) -> String {
    let pid_label = if matches!(target, ClientTarget::CodexCli | ClientTarget::ClaudeCode) {
        i18n::tr("终端 PID", "terminal PID")
    } else {
        "PID"
    };
    let model_list = match mode {
        ModelListMode::BuiltIn => i18n::tr("已注入默认模型列表。", "Default model list injected."),
        ModelListMode::Gateway => i18n::tr("已注入网关模型列表。", "Gateway model list injected."),
    };
    format!(
        "{} {} ({pid_label} {pid}) • {gateway_url} • {model_list}",
        target.title(),
        i18n::tr("已启动", "started")
    )
}

pub(crate) fn protocol_conversion_message(
    provider_label: &str,
    enabled: bool,
    client_updated: bool,
) -> String {
    format!(
        "{} • {} • {} • {}",
        provider_label,
        i18n::tr("协议转换", "Protocol conversion"),
        if enabled {
            i18n::tr("已开启", "Enabled")
        } else {
            i18n::tr("已关闭", "Disabled")
        },
        if client_updated {
            i18n::tr("客户端无需重启。", "The client does not need to restart.")
        } else {
            i18n::tr(
                "下次启动 • 连接时生效。",
                "takes effect the next time this client is started and connected.",
            )
        }
    )
}

pub async fn update_gateway(
    target: ClientTarget,
    gateway_url: &str,
    api_key: String,
    conversion_enabled: bool,
) -> Result<()> {
    let gateway = GatewayIdentity::parse(gateway_url)?;
    let provider_label = crate::gui_settings::load_history()
        .ok()
        .and_then(|history| {
            history
                .iter()
                .find(|entry| {
                    entry.gateway_url.trim_end_matches('/') == gateway.base_url
                        && entry.api_key == api_key.trim()
                        && !entry.name.trim().is_empty()
                })
                .map(|entry| entry.name.trim().to_owned())
        })
        .unwrap_or_else(|| gateway.base_url.clone());
    let expected = local_gateway::configuration_fingerprint(gateway_url, &api_key)?;
    if local_gateway::attached_configuration_fingerprint(target).as_deref()
        != Some(expected.as_str())
    {
        anyhow::bail!(
            "{}",
            i18n::tr(
                "地址或 Key 已改变，请重新配置并重启客户端",
                "The URL or Key changed; reconfigure and restart the client"
            )
        );
    }
    local_gateway::update_attached(target, gateway_url, &api_key, conversion_enabled).await?;
    println!(
        "{}",
        protocol_conversion_message(&provider_label, conversion_enabled, true)
    );
    Ok(())
}

/// Restart in account mode without registering a gateway worker or injecting a
/// model catalog. CLI uses its own restored account home, not Desktop's home.
pub async fn restart_account_client(target: ClientTarget) -> Result<()> {
    match target {
        ClientTarget::CodexDesktop => {
            platform::activate_store_codex("").await?;
        }
        ClientTarget::ClaudeDesktop => {
            platform::activate_store_claude().await?;
        }
        ClientTarget::CodexCli | ClientTarget::ClaudeCode => {
            let paths = crate::account_mode::paths(target)?;
            let home = paths.codex_home.to_string_lossy().into_owned();
            let environment = if target == ClientTarget::CodexCli {
                vec![
                    ("CODEX_HOME", home.as_str()),
                    ("OPENAI_BASE_URL", ""),
                    ("OPENAI_API_KEY", ""),
                ]
            } else {
                vec![
                    ("ANTHROPIC_BASE_URL", ""),
                    ("ANTHROPIC_AUTH_TOKEN", ""),
                    ("ANTHROPIC_API_KEY", ""),
                    ("CLAUDE_GATEWAY_ALLOW_LOOPBACK", ""),
                    ("CLAUDE_CODE_OAUTH_TOKEN", ""),
                ]
            };
            let command = if target == ClientTarget::CodexCli {
                "codex"
            } else {
                "claude"
            };
            let pid = platform::launch_terminal_client_with_environment(
                &format!(
                    "Little-Mungbean - {} - {}",
                    target.title(),
                    i18n::tr("账号模式", "Account mode")
                ),
                command,
                &environment,
            )?;
            // Retain ownership so a later Start • Connect can restart this terminal.
            crate::background::remember_terminal(target, pid).await?;
        }
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !platform::running_client_processes()?
        .iter()
        .any(|(client, _)| *client == target)
    {
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "{}",
                i18n::tr(
                    "未检测到重新启动的客户端",
                    "The restarted client was not detected"
                )
            );
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    println!(
        "{} • {}",
        target.title(),
        i18n::tr("已在账号模式下启动", "Started in account mode")
    );
    Ok(())
}

async fn wait_for_terminal_ready(target: ClientTarget, terminal_pid: u32) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !platform::running_client_processes()?
        .iter()
        .any(|(client, _)| *client == target)
    {
        if tokio::time::Instant::now() >= deadline
            || platform::running_any_process_ids(&[terminal_pid])?.is_empty()
        {
            anyhow::bail!(
                "{} • {}",
                target.title(),
                i18n::tr(
                    "自动重启 • 失败!：启动后未检测到客户端进程",
                    "Auto-restart • failed!: no client process was detected after launch."
                )
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    local_gateway::confirm_current_worker(target)?;
    Ok(())
}

async fn wait_for_terminal_exit(target: ClientTarget, terminal_pid: u32) -> Result<()> {
    loop {
        if !platform::running_any_process_ids(&[terminal_pid])?.contains(&terminal_pid) {
            println!(
                "{} {}.",
                target.title(),
                i18n::tr("终端已关闭", "terminal closed")
            );
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn install_codex_cli_configuration(
    paths: &ClientPaths,
    gateway_url: &str,
    api_key: &str,
    model_list_mode: ModelListMode,
    hidden_models: &[String],
    gateway_catalog: &serde_json::Value,
) -> Result<()> {
    let options = install_options(gateway_url);
    config::install_codex_cli_with_catalog(
        paths,
        &options,
        api_key,
        model_list_mode.uses_gateway_catalog(),
        hidden_models,
        gateway_catalog,
    )
}

async fn install_codex_configuration(
    paths: &ClientPaths,
    gateway_url: &str,
    api_key: &str,
    model_list_mode: ModelListMode,
    hidden_models: &[String],
    gateway_catalog: &serde_json::Value,
) -> Result<()> {
    let options = install_options(gateway_url);
    config::install_codex_with_catalog(
        paths,
        &options,
        api_key,
        model_list_mode.uses_gateway_catalog(),
        hidden_models,
        gateway_catalog,
    )
}

async fn install_claude_code_configuration(
    paths: &ClientPaths,
    gateway_url: &str,
    model_list_mode: ModelListMode,
    hidden_models: &[String],
    gateway_catalog: &serde_json::Value,
) -> Result<serde_json::Value> {
    let options = install_options(gateway_url);
    config::install_claude_code_with_catalog(
        paths,
        &options,
        model_list_mode.uses_gateway_catalog(),
        hidden_models,
        gateway_catalog,
    )
}

fn install_options(gateway_url: &str) -> InstallOptions {
    InstallOptions {
        gateway_url: gateway_url.to_owned(),
        default_model: None,
        dry_run: false,
        set_user_environment: true,
    }
}

pub fn key_from_environment() -> Result<String> {
    std::env::var("LITTLE_MUNGBEAN_API_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .context("内部工作进程没有收到 API Key")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_ids_round_trip() {
        for target in ClientTarget::ALL {
            assert_eq!(ClientTarget::from_id(target.id()), target);
        }
        assert_eq!(ClientTarget::from_id("unknown"), ClientTarget::CodexDesktop);
    }

    #[test]
    fn model_list_mode_accepts_current_and_legacy_ids() {
        assert_eq!(ModelListMode::from_id("built-in"), ModelListMode::BuiltIn);
        assert_eq!(ModelListMode::from_id("four-slots"), ModelListMode::BuiltIn);
        assert_eq!(ModelListMode::from_id("gateway"), ModelListMode::Gateway);
        assert_eq!(
            ModelListMode::from_id("full-catalog"),
            ModelListMode::Gateway
        );
        assert_eq!(ModelListMode::from_id("unknown"), ModelListMode::Gateway);
    }

    #[test]
    fn every_client_rejects_catalogs_without_usable_models_before_startup() {
        for target in ClientTarget::ALL {
            for mode in [ModelListMode::BuiltIn, ModelListMode::Gateway] {
                for invalid in [
                    serde_json::json!({"models": []}),
                    serde_json::json!({"models": [{"slug": "text-model", "visibility": "hide"}]}),
                    serde_json::json!({"models": [{"slug": "text-embedding-3-small"}]}),
                ] {
                    assert!(validate_client_catalog(target, mode, &invalid).is_err());
                }
                assert!(
                    validate_client_catalog(
                        target,
                        mode,
                        &serde_json::json!({"models": [{"slug": "gpt-test"}]}),
                    )
                    .is_ok()
                );
            }
        }
    }
}
