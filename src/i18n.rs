#[cfg(not(test))]
use std::sync::OnceLock;

pub const LANGUAGE_ENV: &str = "LITTLE_MUNGBEAN_LAUNCHER_LANG";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    ZhCn,
    En,
}

impl Language {
    #[cfg_attr(test, allow(dead_code))]
    pub fn detect() -> Self {
        std::env::var(LANGUAGE_ENV)
            .ok()
            .and_then(|value| Self::from_tag(&value))
            .unwrap_or_else(|| {
                if crate::platform::user_prefers_chinese() {
                    Self::ZhCn
                } else {
                    Self::En
                }
            })
    }

    pub fn from_tag(value: &str) -> Option<Self> {
        let value = value.trim().to_ascii_lowercase().replace('_', "-");
        if value == "zh" || value.starts_with("zh-") {
            Some(Self::ZhCn)
        } else if value == "en" || value.starts_with("en-") {
            Some(Self::En)
        } else {
            None
        }
    }

    pub const fn tag(self) -> &'static str {
        match self {
            Self::ZhCn => "zh-CN",
            Self::En => "en",
        }
    }

    pub const fn text(self, zh_cn: &'static str, en: &'static str) -> &'static str {
        match self {
            Self::ZhCn => zh_cn,
            Self::En => en,
        }
    }
}

#[cfg(not(test))]
pub fn language() -> Language {
    static LANGUAGE: OnceLock<Language> = OnceLock::new();
    *LANGUAGE.get_or_init(Language::detect)
}

#[cfg(test)]
pub fn language() -> Language {
    // Existing behavior tests assert the long-standing Chinese wording. Keep
    // them deterministic on English GitHub runners; English selection itself
    // is covered through the pure Language helpers below.
    Language::ZhCn
}

pub fn tr(zh_cn: &'static str, en: &'static str) -> &'static str {
    language().text(zh_cn, en)
}

/// Format an internal error for user-facing runtime logs. English mode drops
/// untranslated context frames while retaining English OS, network, and parser
/// causes. This prevents a deeply nested `anyhow` context from leaking Chinese
/// launcher text into an otherwise English log.
pub fn runtime_error(error: &anyhow::Error) -> String {
    runtime_error_text(&format!("{error:#}"))
}

/// Operation names belong to the log header, not to the error body. Remove
/// only known launcher context frames; URLs and arbitrary technical details
/// containing colons must remain intact.
pub fn runtime_log_error(error: &anyhow::Error) -> String {
    let mut details = Vec::new();
    for cause in error.chain() {
        let detail = strip_operation_context(&cause.to_string());
        if !detail.is_empty() && !details.contains(&detail) {
            details.push(detail);
        }
    }
    runtime_log_error_text(&details.join("\n"))
}

pub fn runtime_log_error_text(detail: &str) -> String {
    runtime_error_text_with_separator(&strip_operation_context(detail), language(), "\n")
}

fn strip_operation_context(detail: &str) -> String {
    detail
        .lines()
        .filter_map(|line| {
            let mut text = line.trim();
            while let Some(rest) = crate::gui_worker::ClientTarget::ALL
                .into_iter()
                .find_map(|target| text.strip_prefix(target.title()))
                .and_then(|rest| rest.trim_start().strip_prefix('•'))
            {
                let rest = rest.trim_start();
                let Some(body) = [
                    "启动 & 连接 • 失败!",
                    "启动 • 连接 • 失败!",
                    "自动重启 • 失败!",
                    "自动启动 • 失败!",
                    "Start & Connect • Failed!",
                    "Start • Connect • Failed!",
                    "Auto-restart • failed!",
                    "Auto-restart • Failed!",
                    "Auto-start • failed!",
                    "Auto-start • Failed!",
                ]
                .into_iter()
                .find_map(|prefix| rest.strip_prefix(prefix)) else {
                    break;
                };
                let body = body.trim_start();
                if body.is_empty() {
                    text = "";
                    break;
                }
                let Some(body) = body.strip_prefix([':', '：']) else {
                    break;
                };
                text = body.trim_start();
            }
            (!text.is_empty()).then(|| text.to_owned())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn runtime_error_block(detail: &str) -> String {
    let lines = detail
        .lines()
        .map(|line| format!("    {}", line.trim()))
        .collect::<Vec<_>>()
        .join("\n");
    format!("\n{}:\n{lines}", tr("错误", "Error"))
}

pub fn runtime_error_text(detail: &str) -> String {
    runtime_error_text_for_language(detail, language())
}

fn runtime_error_text_for_language(detail: &str, language: Language) -> String {
    runtime_error_text_with_separator(detail, language, " ")
}

fn runtime_error_text_with_separator(detail: &str, language: Language, separator: &str) -> String {
    if language == Language::ZhCn {
        return detail.to_owned();
    }

    let detail = detail
        .lines()
        .map(strip_cjk)
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .map(|line| {
            line.trim_start_matches([':', '：', ';', '；', ',', '，', '.', '。'])
                .trim_start()
                .to_owned()
        })
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(separator);
    if detail.is_empty() {
        "The operation failed.".to_owned()
    } else {
        detail
    }
}

fn strip_cjk(value: &str) -> String {
    value
        .chars()
        .filter(|character| {
            !matches!(
                character,
                '\u{3400}'..='\u{4DBF}'
                    | '\u{4E00}'..='\u{9FFF}'
                    | '\u{F900}'..='\u{FAFF}'
                    | '\u{20000}'..='\u{2FA1F}'
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_errors_drop_operation_context_and_indent_every_cause() {
        for target in crate::gui_worker::ClientTarget::ALL {
            let error = anyhow::anyhow!("当前平台不支持 Claude Desktop Windows 注册表配置")
                .context(format!("{} • 自动重启 • 失败!", target.title()));
            let detail = runtime_log_error(&error);
            assert_eq!(detail, "当前平台不支持 Claude Desktop Windows 注册表配置");
            assert_eq!(
                runtime_error_block(&detail),
                "\n错误:\n    当前平台不支持 Claude Desktop Windows 注册表配置"
            );
            let error = anyhow::anyhow!("connection refused: http://127.0.0.1:53601/responses")
                .context(format!("{} • Auto-restart • failed!", target.title()))
                .context("Failed to connect to the local listener");
            assert_eq!(
                runtime_log_error(&error),
                "Failed to connect to the local listener\nconnection refused: http://127.0.0.1:53601/responses"
            );
        }
        assert_eq!(
            runtime_log_error_text("Claude Desktop • 自动重启 • 失败!: underlying: detail"),
            "underlying: detail"
        );
        assert_eq!(
            runtime_log_error_text("Claude Desktop unavailable: http://127.0.0.1:1234"),
            "Claude Desktop unavailable: http://127.0.0.1:1234"
        );
        assert_eq!(
            runtime_error_block("first\n    second\r\nthird"),
            "\n错误:\n    first\n    second\n    third"
        );
    }

    #[test]
    fn language_tags_follow_chinese_or_english_policy() {
        assert_eq!(Language::from_tag("zh-CN"), Some(Language::ZhCn));
        assert_eq!(Language::from_tag("zh_TW"), Some(Language::ZhCn));
        assert_eq!(Language::from_tag("en-US"), Some(Language::En));
        assert_eq!(Language::from_tag("fr-FR"), None);
    }

    #[test]
    fn localized_text_selects_expected_branch() {
        assert_eq!(Language::ZhCn.text("中文", "English"), "中文");
        assert_eq!(Language::En.text("中文", "English"), "English");
    }

    #[test]
    fn english_log_errors_preserve_lines_without_chinese_context() {
        let detail = "未翻译的上下文\nFailed to fetch the model catalog.\nThe local network cannot reach the gateway endpoint: https://example.com:8443 (https://example.com:8443/v1/models)\nCheck the gateway URL, API key, local network status, and firewall.";
        let expected = detail.split_once('\n').unwrap().1;
        assert_eq!(
            runtime_error_text_with_separator(detail, Language::En, "\n"),
            expected
        );
        assert_eq!(
            runtime_error_text_for_language(detail, Language::En),
            expected.replace('\n', " ")
        );
    }

    #[test]
    fn runtime_error_text_keeps_english_technical_details() {
        assert_eq!(
            strip_cjk("无法连接 gateway: connect timed out")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            "gateway: connect timed out"
        );
        assert_eq!(
            strip_cjk("connect timed out: https://api.example.com"),
            "connect timed out: https://api.example.com"
        );
        assert_eq!(
            runtime_error_text_for_language(
                "Codex Desktop • Auto-restart • failed!: gateway connection timed out",
                Language::En
            ),
            "Codex Desktop • Auto-restart • failed!: gateway connection timed out"
        );
        assert_eq!(
            runtime_error_text_for_language("启动失败", Language::En),
            "The operation failed."
        );
    }
}
