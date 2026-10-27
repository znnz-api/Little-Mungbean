//! Public official MCP Registry adapter, with bounded paginated requests.
use crate::{i18n, network_proxy};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

pub const REGISTRY_URL: &str = "https://registry.modelcontextprotocol.io/v0.1/servers";
#[derive(Clone, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub title: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub website_url: Option<String>,
    pub config: Option<Value>,
    pub requires_setup: bool,
    pub setup_note: String,
}
impl Entry {
    pub fn transport(&self) -> &'static str {
        if self
            .config
            .as_ref()
            .is_some_and(|v| v.get("command").is_some())
        {
            "stdio"
        } else {
            "HTTP"
        }
    }
    pub fn local_name(&self) -> String {
        let title = self.title.trim();
        let name = if title.is_empty() {
            self.name.rsplit('/').next().unwrap_or(&self.name)
        } else {
            title
        };
        name.replace(
            |c: char| !c.is_alphanumeric() && !matches!(c, '-' | '_' | '.'),
            "-",
        )
    }
}
#[derive(Default, Serialize, Deserialize)]
pub struct Page {
    pub entries: Vec<Entry>,
    pub cursor: Option<String>,
    /// The broader API query used after a name fallback; keep it for pagination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_query: Option<String>,
}

pub fn load_cache() -> Option<Page> {
    let path = crate::platform_paths::data_root()
        .ok()?
        .join("gui/mcp-registry-cache.json");
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub async fn fetch(cursor: Option<&str>) -> Result<Page> {
    fetch_search(cursor, "", None).await
}

pub async fn fetch_search(
    cursor: Option<&str>,
    search: &str,
    continuation_query: Option<&str>,
) -> Result<Page> {
    let client =
        network_proxy::Client::new(|| reqwest::Client::builder().timeout(Duration::from_secs(25)))?
            .total_timeout(Duration::from_secs(25));
    let page = search_page(&client, REGISTRY_URL, cursor, search, continuation_query).await?;
    if cursor.is_none()
        && search.trim().is_empty()
        && let Ok(root) = crate::platform_paths::data_root()
    {
        let _ = crate::backup::atomic_write(
            &root.join("gui/mcp-registry-cache.json"),
            &serde_json::to_vec(&page)?,
        );
    }
    Ok(page)
}

fn fallback_keyword(search: &str) -> Option<String> {
    let search = search.trim().to_lowercase();
    let keyword = search
        .split(|c: char| !c.is_alphanumeric())
        .filter(|part| part.chars().count() >= 3)
        .filter(|part| {
            !matches!(
                *part,
                "mcp" | "server" | "www" | "com" | "org" | "net" | "http" | "https"
            )
        })
        .max_by_key(|part| part.chars().count())?;
    (keyword != search).then(|| keyword.to_owned())
}

fn matches_search_name(entry: &Entry, search: &str) -> bool {
    let normalize = |text: &str| {
        text.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let query = normalize(search);
    !query.is_empty()
        && [&entry.title, &entry.name]
            .iter()
            .any(|name| normalize(name).contains(&query))
}

async fn search_page(
    client: &network_proxy::Client,
    endpoint: &str,
    cursor: Option<&str>,
    search: &str,
    continuation_query: Option<&str>,
) -> Result<Page> {
    let search = search.trim();
    let mut query = if cursor.is_some() {
        continuation_query.unwrap_or(search)
    } else {
        search
    }
    .to_owned();
    let mut page = request_page(client, endpoint, cursor, &query).await?;
    // Retry a confirmed empty first search with one meaningful name token.
    // HTTP/network failures propagate directly rather than looking like no results.
    if cursor.is_none()
        && page.entries.is_empty()
        && page.cursor.is_none()
        && let Some(keyword) = fallback_keyword(search)
    {
        query = keyword;
        page = request_page(client, endpoint, None, &query).await?;
    }
    if query != search {
        for attempt in 0..3 {
            page.entries
                .retain(|entry| matches_search_name(entry, search));
            if !page.entries.is_empty() || page.cursor.is_none() || attempt == 2 {
                break;
            }
            page = request_page(client, endpoint, page.cursor.as_deref(), &query).await?;
        }
        page.search_query = Some(query);
    }
    Ok(page)
}

async fn request_page(
    client: &network_proxy::Client,
    endpoint: &str,
    cursor: Option<&str>,
    search: &str,
) -> Result<Page> {
    let mut request = client
        .get(endpoint)
        .query(&[("limit", "30"), ("version", "latest")]);
    let search = search.trim();
    if !search.is_empty() {
        request = request.query(&[("search", search)]);
    }
    if let Some(cursor) = cursor {
        request = request.query(&[("cursor", cursor)]);
    }
    let response = request
        .send()
        .await
        .context(i18n::tr(
            "无法连接 MCP 官方市场，请检查网络代理",
            "Cannot connect to the official MCP Registry; check your proxy",
        ))?
        .error_for_status()?;
    ensure!(
        response.content_length().unwrap_or(0) <= 8 * 1024 * 1024,
        "Registry response too large"
    );
    let mut response = response;
    let mut bytes = vec![];
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= 8 * 1024 * 1024,
            "Registry response too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    parse_page(serde_json::from_slice(&bytes)?)
}

pub fn parse_page(value: Value) -> Result<Page> {
    let servers = value["servers"]
        .as_array()
        .context("Invalid MCP Registry response")?;
    let mut entries = vec![];
    for row in servers {
        let status = row
            .pointer("/_meta/io.modelcontextprotocol.registry~1official/status")
            .and_then(Value::as_str)
            .unwrap_or("active");
        if status != "active" {
            continue;
        }
        let server = &row["server"];
        let Some(name) = server["name"].as_str() else {
            continue;
        };
        let (config, note) = prepare_config(server);
        let requires_setup = config
            .as_ref()
            .is_some_and(|c| c.to_string().contains("<required:"));
        entries.push(Entry {
            name: name.into(),
            title: server["title"]
                .as_str()
                .unwrap_or_else(|| name.rsplit('/').next().unwrap_or(name))
                .into(),
            description: server["description"].as_str().unwrap_or_default().into(),
            website_url: parse_website_url(server["websiteUrl"].as_str()),
            config,
            requires_setup,
            setup_note: note,
        });
    }
    Ok(Page {
        entries,
        cursor: value
            .pointer("/metadata/nextCursor")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        search_query: None,
    })
}

fn parse_website_url(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    let parsed = url::Url::parse(value).ok()?;
    (matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some())
        .then(|| value.to_owned())
}

fn input(v: &Value) -> String {
    v["value"]
        .as_str()
        .or_else(|| v["default"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("<required:{}>", v["name"].as_str().unwrap_or("value")))
}
fn prepare_config(server: &Value) -> (Option<Value>, String) {
    if let Some(remotes) = server["remotes"].as_array() {
        for remote in remotes {
            if !matches!(remote["type"].as_str(), Some("streamable-http" | "sse")) {
                continue;
            }
            let Some(raw) = remote["url"].as_str() else {
                continue;
            };
            let mut url = raw.to_string();
            if let Some(variables) = remote["variables"].as_object() {
                for (name, spec) in variables {
                    url = url.replace(
                        &format!("{{{name}}}"),
                        &input(&json!({"name":name, "value":spec.get("default")})),
                    );
                }
            }
            let mut config =
                json!({"type":if remote["type"] == "sse" { "sse" } else { "http" },"url":url});
            if let Some(headers) = remote["headers"].as_array() {
                let mut values = serde_json::Map::new();
                for header in headers {
                    if (header["isRequired"] == true
                        || header.get("value").is_some()
                        || header.get("default").is_some())
                        && let Some(name) = header["name"].as_str()
                    {
                        values.insert(name.into(), json!(input(header)));
                    }
                }
                if !values.is_empty() {
                    config["headers"] = json!(values);
                }
            }
            return (Some(config), i18n::tr("远程 MCP；Claude Desktop 使用 mcp-remote 桥接，需要 Node.js。OAuth 请在客户端完成。", "Remote MCP; Claude Desktop uses mcp-remote and needs Node.js. Complete OAuth in the client.").into());
        }
    }
    if let Some(packages) = server["packages"].as_array() {
        for package in packages {
            let Some(id) = package["identifier"].as_str() else {
                continue;
            };
            if package
                .pointer("/transport/type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind != "stdio")
            {
                continue;
            }
            let kind = package["registryType"].as_str().unwrap_or_default();
            let (command, mut args) = match kind {
                "npm" => ("npx", vec![json!("-y")]),
                "pypi" => ("uvx", vec![]),
                _ => continue,
            };
            if let Some(runtime_args) = package["runtimeArguments"].as_array() {
                append_args(&mut args, runtime_args);
            }
            let version = package["version"]
                .as_str()
                .filter(|s| !s.is_empty() && *s != "latest");
            args.push(json!(match version {
                Some(v) => format!("{id}{}{v}", if kind == "npm" { "@" } else { "==" }),
                None => id.into(),
            }));
            if let Some(package_args) = package["packageArguments"].as_array() {
                append_args(&mut args, package_args);
            }
            let mut config = json!({"command":command,"args":args});
            if let Some(vars) = package["environmentVariables"].as_array() {
                let mut env = serde_json::Map::new();
                for v in vars {
                    if (v["isRequired"] == true
                        || v.get("value").is_some()
                        || v.get("default").is_some())
                        && let Some(name) = v["name"].as_str()
                    {
                        env.insert(name.into(), json!(input(v)));
                    }
                }
                if !env.is_empty() {
                    config["env"] = json!(env);
                }
            }
            return (Some(config), if kind == "npm" { i18n::tr("需要 Node.js / npx；首次连接时客户端会下载该 MCP。", "Requires Node.js / npx; the client downloads this MCP on first connection.") } else { i18n::tr("需要 Python / uvx；首次连接时客户端会下载该 MCP。", "Requires Python / uvx; the client downloads this MCP on first connection.") }.into());
        }
    }
    (
        None,
        i18n::tr(
            "此服务未提供可用的 stdio / HTTP 安装配置",
            "This server has no supported stdio / HTTP installation configuration",
        )
        .into(),
    )
}
fn append_args(out: &mut Vec<Value>, args: &[Value]) {
    for arg in args {
        if arg["isRequired"] != true
            && arg.get("value").is_none()
            && arg.get("default").is_none()
            && arg.get("variables").is_none()
        {
            continue;
        }
        if arg["type"] == "named"
            && let Some(name) = arg["name"].as_str()
        {
            out.push(json!(name));
        }
        let mut value = input(arg);
        if let Some(variables) = arg["variables"].as_object() {
            for (name, spec) in variables {
                value = value.replace(
                    &format!("{{{name}}}"),
                    spec["default"]
                        .as_str()
                        .unwrap_or(&format!("<required:{name}>")),
                );
            }
        }
        out.push(json!(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn mock_registry(
        replies: Vec<(&'static str, Option<&'static str>, u16, Value)>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            for (search, cursor, status, body) in replies {
                let (mut socket, _) =
                    tokio::time::timeout(Duration::from_secs(5), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let mut request = Vec::new();
                while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                    let mut buffer = [0; 1024];
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert!(count > 0 && request.len() < 8192);
                    request.extend_from_slice(&buffer[..count]);
                }
                let request = String::from_utf8(request).unwrap();
                let target = request.split_whitespace().nth(1).unwrap();
                let url = url::Url::parse(&format!("http://{address}{target}")).unwrap();
                let params = url
                    .query_pairs()
                    .collect::<std::collections::HashMap<_, _>>();
                assert_eq!(
                    params
                        .get("search")
                        .map(|value| value.as_ref())
                        .unwrap_or(""),
                    search
                );
                assert_eq!(params.get("cursor").map(|value| value.as_ref()), cursor);
                let body = body.to_string();
                socket.write_all(format!("HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        (format!("http://{address}/servers"), task)
    }

    #[test]
    fn name_fallback_extracts_a_meaningful_token_and_checks_original_name() {
        assert_eq!(fallback_keyword("2kw.ai").as_deref(), Some("2kw"));
        assert_eq!(fallback_keyword("Example MCP").as_deref(), Some("example"));
        assert_eq!(fallback_keyword("filesystem"), None);
        assert_eq!(fallback_keyword("mcp-server"), None);
        assert_eq!(fallback_keyword("ai.io"), None);
        assert_eq!(fallback_keyword("..."), None);
        let entry = Entry {
            name: "ai.2kw/mcp-server".into(),
            title: "2kw.ai".into(),
            description: "A service called other.example".into(),
            website_url: None,
            config: None,
            requires_setup: false,
            setup_note: String::new(),
        };
        assert!(matches_search_name(&entry, "2KW.AI"));
        assert!(!matches_search_name(&entry, "other.example"));
    }

    #[tokio::test]
    async fn name_fallback_filters_candidates_and_keeps_its_query_for_pagination() {
        let (endpoint, server) = mock_registry(vec![
            ("2kw.ai", None, 200, json!({"servers":[]})),
            ("2kw", None, 200, json!({"servers":[
                {"server":{"name":"ai.2kw/mcp-server","title":"2kw.ai"}},
                {"server":{"name":"io.other/2kw","title":"Other service","description":"Mentions 2kw.ai"}}
            ],"metadata":{"nextCursor":"opaque:page/2"}})),
            ("2kw", Some("opaque:page/2"), 200, json!({"servers":[{"server":{"name":"ai.2kw/tools","title":"2kw.ai Tools"}}]})),
        ]).await;
        let client = network_proxy::Client::direct(
            reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
        );
        let page = search_page(&client, &endpoint, None, "2kw.ai", None)
            .await
            .unwrap();
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].title, "2kw.ai");
        assert_eq!(page.search_query.as_deref(), Some("2kw"));
        let next = search_page(
            &client,
            &endpoint,
            page.cursor.as_deref(),
            "2kw.ai",
            page.search_query.as_deref(),
        )
        .await
        .unwrap();
        assert_eq!(next.entries[0].title, "2kw.ai Tools");
        assert!(next.cursor.is_none());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn name_fallback_leaves_successful_searches_and_network_errors_alone() {
        for status in [200, 503] {
            let (endpoint, server) = mock_registry(vec![(
                "2kw.ai",
                None,
                status,
                json!({"servers":[{"server":{"name":"example/2kw.ai","title":"2kw.ai"}}]}),
            )])
            .await;
            let client = network_proxy::Client::direct(
                reqwest::Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_secs(5))
                    .build()
                    .unwrap(),
            );
            let result = search_page(&client, &endpoint, None, "2kw.ai", None).await;
            if status == 200 {
                let page = result.unwrap();
                assert_eq!(page.entries.len(), 1);
                assert!(page.search_query.is_none());
            } else {
                assert!(result.is_err());
            }
            server.await.unwrap();
        }
    }

    #[tokio::test]
    #[ignore = "uses the public MCP Registry over the network"]
    async fn official_registry_title_fallback_smoke() {
        let page = fetch_search(None, "2kw.ai", None).await.unwrap();
        assert!(
            page.entries
                .iter()
                .any(|entry| entry.title.eq_ignore_ascii_case("2kw.ai"))
        );
        assert_eq!(page.search_query.as_deref(), Some("2kw"));
    }

    #[test]
    fn install_name_prefers_market_title_over_generic_registry_id() {
        let mut entry = Entry {
            name: "ai.2kw/mcp-server".into(),
            title: "2kw.ai".into(),
            description: String::new(),
            website_url: None,
            config: None,
            requires_setup: true,
            setup_note: String::new(),
        };
        assert_eq!(entry.local_name(), "2kw.ai");
        entry.title = "  ".into();
        assert_eq!(entry.local_name(), "mcp-server");
        entry.title = "Example MCP".into();
        assert_eq!(entry.local_name(), "Example-MCP");
    }

    #[test]
    fn registry_maps_packages_required_secrets_remote_headers_and_pagination() {
        let page=parse_page(json!({"servers":[
            {"server":{"name":"io.example/files","description":"Read files","packages":[{"registryType":"npm","identifier":"@example/files","version":"1.2.3","packageArguments":[{"type":"positional","name":"path","isRequired":true}],"environmentVariables":[{"name":"TOKEN","isRequired":true}]}]}},
            {"server":{"name":"io.example/remote","websiteUrl":"https://example.com/remote","remotes":[{"type":"streamable-http","url":"https://example.com/mcp","headers":[{"name":"Authorization","isRequired":true}]}]}},
            {"server":{"name":"io.example/removed"},"_meta":{"io.modelcontextprotocol.registry/official":{"status":"deleted"}}}
        ],"metadata":{"nextCursor":"next-page"}})).unwrap();
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.cursor.as_deref(), Some("next-page"));
        assert_eq!(
            page.entries[1].website_url.as_deref(),
            Some("https://example.com/remote")
        );
        let npm = page.entries[0].config.as_ref().unwrap();
        assert_eq!(npm["args"][1], "@example/files@1.2.3");
        assert_eq!(npm["env"]["TOKEN"], "<required:TOKEN>");
        assert!(page.entries[0].requires_setup);
        let remote = page.entries[1].config.as_ref().unwrap();
        assert_eq!(
            remote["headers"]["Authorization"],
            "<required:Authorization>"
        );
        assert!(parse_page(json!({"error":"unavailable"})).is_err());
    }

    #[tokio::test]
    #[ignore = "uses the public MCP Registry over the network"]
    async fn official_registry_contract_smoke() {
        let page = fetch(None).await.unwrap();
        assert!(!page.entries.is_empty());
        for entry in &page.entries {
            if let Some(config) = &entry.config
                && !entry.requires_setup
            {
                crate::mcp::validate_config(config.clone())
                    .unwrap_or_else(|e| panic!("{}: {e}", entry.name));
            }
        }
        if let Some(cursor) = page.cursor {
            let next = fetch(Some(&cursor)).await.unwrap();
            assert!(!next.entries.is_empty());
            assert_ne!(page.entries[0].name, next.entries[0].name);
        }
    }
}
