use crate::claude_desktop_proxy::{
    ClaudeDesktopModelMode, ClaudeDesktopProxy, DesktopModelMenu, ModelSlots, SlotOverrides,
    select_model_slots,
};
use crate::{catalog, claude_desktop_registry as registry, i18n, platform};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::time::Duration;
use tracing::{info, warn};
use zeroize::Zeroizing;

pub(crate) const DISCOVERY_CLIENT_VERSION: &str = "claude-desktop-1.24012.9";
const START_TIMEOUT: Duration = Duration::from_secs(30);
const WATCH_INTERVAL: Duration = Duration::from_secs(1);
const MAX_MISSED_POLLS: usize = 2;

pub async fn diagnose(gateway_url: &str, api_key: Option<&str>, online: bool) -> Result<()> {
    println!("little-mungbean-client Claude Desktop 只读诊断\n");

    match registry::status() {
        Ok(status) => {
            println!(
                "注册表策略键:       {}",
                if status.policy_key_exists {
                    "存在"
                } else {
                    "不存在"
                }
            );
            println!(
                "受管注册表值:       {}",
                if status.managed_values_present.is_empty() {
                    "未设置".to_owned()
                } else {
                    status.managed_values_present.join(", ")
                }
            );
            println!(
                "待恢复加密备份:     {}",
                if status.pending_backup {
                    "存在"
                } else {
                    "不存在"
                }
            );
            println!("备份位置:           {}", status.backup_path.display());
        }
        Err(error) => println!("注册表检查:         失败: {error:#}"),
    }

    match platform::running_claude_desktop_processes() {
        Ok(processes) if processes.is_empty() => println!("运行中的 Desktop:  未检测到"),
        Ok(processes) => println!("运行中的 Desktop:  {}", processes.join("；")),
        Err(error) => println!("运行中的 Desktop:  检查失败: {error:#}"),
    }

    if online {
        let key = api_key.context("在线诊断需要 API Key")?;
        println!("\n在线检查:");
        let catalog = catalog::fetch_catalog(gateway_url, key, DISCOVERY_CLIENT_VERSION).await?;
        let visible = catalog::visible_slugs(&catalog);
        let slots = select_model_slots(&catalog, &SlotOverrides::default())?;
        println!("  网关模型目录:     正常，{} 个可见模型", visible.len());
        print_slots(&slots);
    }

    println!("\n诊断未修改注册表、Claude 数据目录或历史任务，也未输出完整 API Key。");
    Ok(())
}

pub async fn launch(
    gateway_url: &str,
    api_key: String,
    overrides: SlotOverrides,
    mode: ClaudeDesktopModelMode,
) -> Result<()> {
    launch_with_hidden_models(gateway_url, api_key, overrides, mode, &[]).await
}

pub async fn launch_with_hidden_models(
    gateway_url: &str,
    api_key: String,
    overrides: SlotOverrides,
    mode: ClaudeDesktopModelMode,
    hidden_models: &[String],
) -> Result<()> {
    let api_key = Zeroizing::new(api_key);
    println!(
        "{}",
        i18n::tr(
            "正在拉取网关模型目录……",
            "Fetching the gateway model catalog…"
        )
    );
    let catalog = catalog::fetch_catalog(gateway_url, &api_key, DISCOVERY_CLIENT_VERSION).await?;
    launch_with_catalog(
        gateway_url,
        gateway_url,
        api_key.to_string(),
        overrides,
        mode,
        hidden_models,
        &catalog,
    )
    .await
}

pub async fn launch_with_catalog(
    gateway_url: &str,
    display_gateway_url: &str,
    api_key: String,
    overrides: SlotOverrides,
    mode: ClaudeDesktopModelMode,
    hidden_models: &[String],
    catalog: &Value,
) -> Result<()> {
    catalog::validate_catalog(catalog).context(i18n::tr(
        "网关模型目录尚未通过校验",
        "The gateway model catalog has not passed validation",
    ))?;
    // Model/override validation must finish before an existing client is closed.
    let slots = select_model_slots(catalog, &overrides)?;
    let running = platform::running_claude_desktop_processes()?;
    if !running.is_empty() {
        crate::desktop::close_for_gateway_attach(crate::gui_worker::ClientTarget::ClaudeDesktop)
            .await
            .context(i18n::tr(
                "Claude Desktop • 自动重启 • 失败!",
                "Claude Desktop • Auto-restart • failed!",
            ))?;
        println!(
            "{}",
            i18n::tr(
                "已正常关闭正在运行的 Claude Desktop，准备应用网关配置。",
                "Claude Desktop exited normally; applying the gateway configuration."
            )
        );
    }
    if registry::restore_pending()? {
        println!(
            "{}",
            i18n::tr(
                "检测到上次异常退出遗留的加密备份，已先恢复原 Claude Desktop 配置。",
                "Recovered the original Claude Desktop settings from a backup left by an earlier abnormal exit."
            )
        );
    }
    // Older builds could leave only the loopback policy after account-mode
    // recovery. Clear that recognizable stale policy before taking the new
    // snapshot, otherwise Claude Desktop may keep using an obsolete route and
    // ignore the newly injected model catalog.
    registry::clear_stale_gateway_policy()?;

    let api_key = Zeroizing::new(api_key);
    let menu = match mode {
        ClaudeDesktopModelMode::FourSlots => DesktopModelMenu::four_slots(),
        ClaudeDesktopModelMode::FullCatalog => {
            let gateway = crate::gateway::GatewayIdentity::parse(gateway_url)?;
            DesktopModelMenu::full_catalog_with_source(catalog, &slots, gateway.source_label())?
        }
    };
    let inference_models = menu
        .with_hidden_models(hidden_models)
        .registry_models_json()?;
    let discovery_enabled = match menu.mode() {
        ClaudeDesktopModelMode::FourSlots => None,
        ClaudeDesktopModelMode::FullCatalog => Some(false),
    };
    let proxy = ClaudeDesktopProxy::start(gateway_url, api_key.to_string(), slots, menu).await?;
    registry::capture_and_store()?;
    if let Err(error) = registry::write_temporary_gateway(
        &proxy.base_url(),
        proxy.local_token(),
        &inference_models,
        discovery_enabled,
    ) {
        let restore = registry::restore_pending();
        proxy.stop().await;
        if let Err(restore_error) = restore {
            return Err(error).context(format!(
                "写入临时 Claude Desktop 配置失败，且恢复原配置也失败: {restore_error:#}"
            ));
        }
        return Err(error);
    }
    if let Err(error) = registry::validate_temporary_gateway(
        &proxy.base_url(),
        proxy.local_token(),
        &inference_models,
        discovery_enabled,
    ) {
        let restore = registry::restore_pending();
        proxy.stop().await;
        if let Err(restore_error) = restore {
            return Err(error).context(format!(
                "Claude Desktop 网关策略校验失败，且恢复原配置也失败: {restore_error:#}"
            ));
        }
        return Err(error);
    }

    let session_result = run_desktop_session(
        display_gateway_url,
        &proxy.base_url(),
        if mode == ClaudeDesktopModelMode::FullCatalog {
            crate::gui_worker::ModelListMode::Gateway
        } else {
            crate::gui_worker::ModelListMode::BuiltIn
        },
    )
    .await
    .context(i18n::tr(
        "Claude Desktop • 自动重启 • 失败!",
        "Claude Desktop • Auto-restart • failed!",
    ));
    if crate::background::is_managed() && session_result.is_ok() {
        crate::background::retain_proxy(crate::gui_worker::ClientTarget::ClaudeDesktop, proxy);
        println!(
            "{}",
            i18n::tr(
                "Claude Desktop 已退出；网关保持运行，可直接重新打开客户端。",
                "Claude Desktop closed; the gateway remains available when the client reopens."
            )
        );
        return Ok(());
    }
    proxy.stop().await;
    let restore_result = registry::restore_pending();

    match (session_result, restore_result) {
        (Ok(()), Ok(_)) => {
            println!(
                "{}",
                i18n::tr(
                    "Claude Desktop 已退出；本地代理已停止，原配置配置已恢复。",
                    "Claude Desktop exited. The local proxy stopped and the original settings were restored."
                )
            );
            Ok(())
        }
        (Err(error), Ok(_)) => Err(error),
        (Ok(()), Err(restore_error)) => Err(restore_error)
            .context("Claude Desktop 已退出，但原配置配置恢复失败；请运行 claude-desktop restore"),
        (Err(error), Err(restore_error)) => Err(error).context(format!(
            "同时无法恢复原配置配置: {restore_error:#}。请运行 claude-desktop restore"
        )),
    }
}

pub fn restore() -> Result<()> {
    if !platform::running_claude_desktop_processes()?.is_empty() {
        bail!("Claude Desktop 仍在运行。请先从托盘退出，再恢复配置配置。");
    }
    if registry::restore_pending()? {
        println!("已恢复 Claude Desktop 原配置配置，并删除加密备份。");
    } else {
        println!("没有检测到待恢复的 Claude Desktop 注册表备份。");
    }
    Ok(())
}

async fn run_desktop_session(
    display_gateway_url: &str,
    listening_address: &str,
    model_list_mode: crate::gui_worker::ModelListMode,
) -> Result<()> {
    #[cfg(target_os = "macos")]
    let _activation_pid = platform::activate_claude_gateway().await?;
    #[cfg(not(target_os = "macos"))]
    let _activation_pid = platform::activate_store_claude().await?;

    let deadline = tokio::time::Instant::now() + START_TIMEOUT;
    loop {
        let pids = platform::running_claude_desktop_process_ids()?;
        if !pids.is_empty() {
            registry::ensure_third_party_mode()?;
            crate::local_gateway::confirm_current_worker(
                crate::gui_worker::ClientTarget::ClaudeDesktop,
            )?;
            let pid = pids.first().copied().unwrap_or_default();
            println!(
                "{}",
                crate::gui_worker::client_started_message(
                    crate::gui_worker::ClientTarget::ClaudeDesktop,
                    pid,
                    display_gateway_url,
                    model_list_mode,
                )
            );
            println!(
                "{}",
                i18n::tr(
                    "Claude Desktop 已自动接入 Little Mungbean 本地网关。",
                    "Claude Desktop is connected to the Little Mungbean local gateway."
                )
            );
            println!(
                "{}：{}",
                i18n::tr("配置目录", "Configuration directory"),
                registry::configuration_directory()?.display()
            );
            println!(
                "{}：{}",
                i18n::tr("监听地址", "Listening address"),
                listening_address
            );
            println!("{}", crate::gui_worker::connection_token_hint());
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "{}",
                if i18n::language() == i18n::Language::ZhCn {
                    format!(
                        "启动后 {} 秒内未检测到官方 Claude Desktop 进程。已停止启动并将恢复原配置。",
                        START_TIMEOUT.as_secs()
                    )
                } else {
                    format!(
                        "The official Claude Desktop process was not detected within {} seconds after launch. Startup was stopped and the original settings will be restored.",
                        START_TIMEOUT.as_secs()
                    )
                }
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let mut missed = 0usize;
    loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal.context(i18n::tr("监听 Ctrl+C 失败", "Failed to listen for Ctrl+C"))?;
                warn!(
                    "{}",
                    i18n::tr(
                        "收到 Ctrl+C；本地代理将停止并恢复配置。仍在运行的 Claude Desktop 需要重新启动后才能继续调用网关。",
                        "Received Ctrl+C; the local proxy will stop and the configuration will be restored. Restart any still-running Claude Desktop instance before using the gateway again."
                    )
                );
                return Ok(());
            }
            _ = tokio::time::sleep(WATCH_INTERVAL) => {}
        }

        let pids = platform::running_claude_desktop_process_ids()?;
        if pids.is_empty() {
            missed += 1;
            if missed >= MAX_MISSED_POLLS {
                info!(
                    "{}",
                    i18n::tr(
                        "Claude Desktop 已退出，正在停止本地代理并恢复配置",
                        "Claude Desktop exited; stopping the local proxy and restoring the registry"
                    )
                );
                return Ok(());
            }
        } else {
            missed = 0;
            registry::ensure_third_party_mode()?;
        }
    }
}

fn print_slots(slots: &ModelSlots) {
    println!("  Haiku:  {}", slots.haiku);
    println!("  Sonnet: {}", slots.sonnet);
    println!("  Opus:   {}", slots.opus);
    println!("  Fable:  {}", slots.fable);
}
