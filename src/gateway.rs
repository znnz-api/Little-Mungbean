use anyhow::{Context, Result, bail};
use url::Url;

// Public builds start with an empty endpoint so users choose their own gateway.
pub const DEFAULT_GATEWAY_URL: &str = "";

/// Append a canonical /v1 resource without duplicating provider API prefixes.
/// Arbitrary reverse-proxy prefixes retain the existing /prefix/v1 behavior.
pub fn upstream_api_path(base: &Url, incoming: &str) -> String {
    let path = base.path().trim_end_matches('/');
    let explicit_api_root = path.ends_with("/v1")
        || (base.host_str() == Some("generativelanguage.googleapis.com")
            && path == "/v1beta/openai")
        || (base.host_str() == Some("ark.cn-beijing.volces.com") && path == "/api/v3");
    if explicit_api_root && let Some(resource) = incoming.strip_prefix("/v1/") {
        format!("{path}/{resource}")
    } else {
        format!("{path}{incoming}")
    }
}
pub const MAINLAND_ACCESS_ERROR: &str = "接口地址无法在中国大陆地区正常解析访问";
pub const MAINLAND_ACCESS_ERROR_EN: &str =
    "The gateway address cannot be resolved or accessed normally from mainland China";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayIdentity {
    pub base_url: String,
    pub display_name: String,
    pub is_znnz: bool,
}

impl GatewayIdentity {
    pub fn parse(value: &str) -> Result<Self> {
        let mut url = Url::parse(value.trim()).context(crate::i18n::tr(
            "接口地址格式无效",
            "Invalid gateway URL format",
        ))?;
        let host = url.host_str().context(crate::i18n::tr(
            "接口地址缺少主机名",
            "Gateway URL is missing a host",
        ))?;
        if url.scheme() != "https" && !is_loopback(host) {
            bail!(
                "{}",
                crate::i18n::tr(
                    "接口地址必须使用 HTTPS；只有本机回环地址允许 HTTP",
                    "Gateway URLs must use HTTPS; only local loopback addresses may use HTTP"
                )
            );
        }

        url.set_query(None);
        url.set_fragment(None);
        let path = url.path().trim_end_matches('/').to_owned();
        url.set_path(&path);

        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        let is_znnz = matches!(host.as_str(), "znnz.net" | "www.znnz.net" | "api.znnz.net");
        let display_name = if is_znnz {
            "znnz.net".to_owned()
        } else if let Some(port) = url.port() {
            format!("{host}:{port}")
        } else {
            host
        };

        Ok(Self {
            base_url: url.to_string().trim_end_matches('/').to_owned(),
            display_name,
            is_znnz,
        })
    }

    pub fn source_label(&self) -> String {
        // Use the normalized endpoint rather than a branded host label. This
        // keeps model provenance accurate for znnz.net and custom gateways,
        // while never exposing the API key or query/fragment data.
        format!("From {}", self.base_url)
    }
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_api_roots_preserve_resource_paths() {
        for (base, expected) in [
            ("https://api.openai.com/v1", "/v1/models"),
            ("https://api.anthropic.com/v1", "/v1/models"),
            (
                "https://generativelanguage.googleapis.com/v1beta/openai",
                "/v1beta/openai/models",
            ),
            ("https://ark.cn-beijing.volces.com/api/v3", "/api/v3/models"),
            (
                "https://dashscope.aliyuncs.com/compatible-mode/v1",
                "/compatible-mode/v1/models",
            ),
            ("https://example.test/custom", "/custom/v1/models"),
        ] {
            let base = Url::parse(base).unwrap();
            assert_eq!(upstream_api_path(&base, "/v1/models"), expected);
            assert_eq!(
                upstream_api_path(&base, "/v1/chat/completions"),
                expected.replace("models", "chat/completions")
            );
        }
    }

    #[test]
    fn identifies_default_and_custom_gateways() {
        let znnz = GatewayIdentity::parse("https://api.znnz.net/v1/").unwrap();
        assert_eq!(znnz.base_url, "https://api.znnz.net/v1");
        assert_eq!(znnz.display_name, "znnz.net");
        assert_eq!(znnz.source_label(), "From https://api.znnz.net/v1");
        assert!(znnz.is_znnz);

        let custom = GatewayIdentity::parse("https://gateway.example.com/api/").unwrap();
        assert_eq!(custom.base_url, "https://gateway.example.com/api");
        assert_eq!(custom.display_name, "gateway.example.com");
        assert_eq!(
            custom.source_label(),
            "From https://gateway.example.com/api"
        );
        assert!(!custom.is_znnz);
    }

    #[test]
    fn rejects_insecure_remote_http() {
        assert!(GatewayIdentity::parse("http://gateway.example.com").is_err());
        assert!(GatewayIdentity::parse("http://127.0.0.1:4000").is_ok());
    }
}
