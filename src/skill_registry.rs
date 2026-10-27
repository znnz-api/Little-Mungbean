//! ClawHub public read API. Skill content is data and is never executed here.
use crate::{
    backup, i18n, network_proxy,
    skills::{Bundle, Origin},
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

const BASE: &str = "https://clawhub.ai";
static RETRY_AT: Mutex<Option<Instant>> = Mutex::new(None);
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub owner: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub version: String,
}
impl Entry {
    pub fn key(&self) -> String {
        format!("{}/{}", self.owner, self.slug)
    }
    pub fn source_url(&self) -> String {
        let mut url = url::Url::parse(BASE).unwrap();
        url.path_segments_mut()
            .unwrap()
            .extend([self.owner.as_str(), self.slug.as_str()]);
        url.to_string()
    }
}
#[derive(Default, Serialize, Deserialize)]
pub struct Page {
    pub entries: Vec<Entry>,
    pub cursor: Option<String>,
}
fn api(path: &[&str]) -> url::Url {
    let mut url = url::Url::parse(BASE).unwrap();
    url.path_segments_mut()
        .unwrap()
        .extend(["api", "v1"])
        .extend(path);
    url
}
async fn client() -> Result<network_proxy::Client> {
    Ok(network_proxy::Client::new(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .https_only(true)
            .user_agent("Little-Mungbean/1.1 (Agent Skills directory)")
    })?
    .redirect_limit(5)
    .total_timeout(Duration::from_secs(30)))
}
async fn get(client: &network_proxy::Client, url: url::Url, limit: usize) -> Result<Vec<u8>> {
    if let Some(until) = *RETRY_AT.lock().unwrap() {
        ensure!(
            Instant::now() >= until,
            "{}",
            i18n::tr(
                "ClawHub 请求限流，请稍后再试",
                "ClawHub rate limit; try again later"
            )
        );
    }
    let mut response = client.get(url.clone()).send().await.with_context(|| {
        format!(
            "{}: {url}",
            i18n::tr(
                "无法连接 Skill 市场，请检查本机网络代理",
                "Cannot connect to the Skill registry; check your proxy"
            )
        )
    })?;
    if response.status().as_u16() == 429 {
        let seconds = response
            .headers()
            .get("retry-after")
            .and_then(|s| s.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(60)
            .clamp(1, 86400);
        *RETRY_AT.lock().unwrap() = Some(Instant::now() + Duration::from_secs(seconds));
        bail!("ClawHub HTTP 429 • Retry after {seconds}s");
    }
    let status = response.status();
    ensure!(
        response.content_length().unwrap_or(0) <= limit as u64,
        "Registry response exceeds size limit"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= limit,
            "Registry response exceeds size limit"
        );
        bytes.extend(chunk);
    }
    ensure!(
        status.is_success(),
        "ClawHub HTTP {status}: {}",
        String::from_utf8_lossy(&bytes[..bytes.len().min(1024)])
    );
    Ok(bytes)
}
pub fn cached() -> Option<Page> {
    let path = crate::platform_paths::data_root()
        .ok()?
        .join("gui/skills-market-cache.json");
    let age = std::fs::metadata(&path)
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .ok()?;
    if age > Duration::from_secs(86400) {
        return None;
    }
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}
pub async fn fetch(query: &str, cursor: Option<&str>) -> Result<Page> {
    let mut url = api(&[if query.trim().is_empty() {
        "skills"
    } else {
        "search"
    }]);
    url.query_pairs_mut()
        .append_pair("limit", "30")
        .append_pair("nonSuspiciousOnly", "true");
    if query.trim().is_empty() {
        url.query_pairs_mut().append_pair("sort", "downloads");
        if let Some(c) = cursor {
            url.query_pairs_mut().append_pair("cursor", c);
        }
    } else {
        url.query_pairs_mut().append_pair("q", query.trim());
    }
    let client = client().await?;
    let value: Value = serde_json::from_slice(&get(&client, url, 2 * 1024 * 1024).await?)?;
    let page = parse_page(&value)?;
    if query.trim().is_empty() && cursor.is_none() {
        let _ = backup::atomic_write(
            &crate::platform_paths::data_root()?.join("gui/skills-market-cache.json"),
            &serde_json::to_vec(&page)?,
        );
    }
    Ok(page)
}
fn parse_page(value: &Value) -> Result<Page> {
    let items = value
        .get("items")
        .or_else(|| value.get("results"))
        .and_then(Value::as_array)
        .context("Invalid ClawHub catalog")?;
    let mut entries = Vec::new();
    for item in items {
        if item
            .pointer("/install/kind")
            .and_then(Value::as_str)
            .is_some_and(|s| s != "clawhub")
        {
            continue;
        }
        let str_value = |field| {
            item.get(field)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        let owner = str_value("ownerHandle");
        let slug = str_value("slug");
        if owner.is_empty() || slug.is_empty() {
            continue;
        }
        let title = str_value("displayName");
        let version = item
            .get("version")
            .and_then(Value::as_str)
            .or_else(|| {
                item.pointer("/latestVersion/version")
                    .and_then(Value::as_str)
            })
            .or_else(|| item.pointer("/tags/latest").and_then(Value::as_str))
            .unwrap_or("")
            .to_owned();
        let entry = Entry {
            owner,
            title: if title.is_empty() {
                slug.clone()
            } else {
                title
            },
            slug,
            description: str_value("summary"),
            version,
        };
        if !entries.iter().any(|e: &Entry| e.key() == entry.key()) {
            entries.push(entry);
        }
    }
    Ok(Page {
        entries,
        cursor: value
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}
pub async fn download(entry: &Entry) -> Result<(Bundle, Origin)> {
    let client = client().await?;
    // Resolve moderation and pin a real version before downloading.
    let mut detail = api(&["skills", &entry.slug]);
    detail.query_pairs_mut().append_pair("owner", &entry.owner);
    let value: Value = serde_json::from_slice(&get(&client, detail, 2 * 1024 * 1024).await?)?;
    ensure!(
        !value
            .pointer("/moderation/isSuspicious")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && !value
                .pointer("/moderation/isMalwareBlocked")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        "ClawHub has flagged this Skill; installation is unavailable"
    );
    let owner = value
        .pointer("/owner/handle")
        .and_then(Value::as_str)
        .context("ClawHub owner is missing")?;
    ensure!(
        owner.eq_ignore_ascii_case(&entry.owner),
        "ClawHub owner changed; refresh the catalog"
    );
    let version = if entry.version.is_empty() {
        value
            .pointer("/latestVersion/version")
            .and_then(Value::as_str)
            .unwrap_or("")
    } else {
        &entry.version
    };
    let mut url = api(&["download"]);
    url.query_pairs_mut()
        .append_pair("slug", &entry.slug)
        .append_pair("owner", owner);
    if !version.is_empty() {
        url.query_pairs_mut().append_pair("version", version);
    }
    let bytes = get(&client, url, 20 * 1024 * 1024).await?;
    let (bundle, pinned_version) = if bytes.starts_with(b"PK") {
        ensure!(
            !version.is_empty(),
            "Hosted Skill version is missing; refresh the registry"
        );
        (Bundle::from_zip(&bytes, None)?, version.to_owned())
    } else {
        let handoff: Value =
            serde_json::from_slice(&bytes).context("Unknown ClawHub download format")?;
        ensure!(
            handoff.get("sourceRef").and_then(Value::as_str) == Some("public-github"),
            "Unsupported Skill download source"
        );
        let archive = handoff
            .get("archiveUrl")
            .and_then(Value::as_str)
            .context("Missing GitHub archive URL")?;
        let url = url::Url::parse(archive)?;
        ensure!(
            url.scheme() == "https"
                && matches!(url.host_str(), Some("github.com" | "codeload.github.com"))
                && url.username().is_empty()
                && url.password().is_none(),
            "Unsupported Skill archive host"
        );
        let sub = handoff
            .get("path")
            .and_then(Value::as_str)
            .context("Missing Skill path")?;
        let commit = handoff
            .get("commit")
            .and_then(Value::as_str)
            .context("Missing Skill commit")?;
        ensure!(
            commit.len() == 40
                && commit.bytes().all(|b| b.is_ascii_hexdigit())
                && url.path().contains(commit),
            "Skill archive is not pinned to its reviewed commit"
        );
        let archive_bytes = get(&client, url, 20 * 1024 * 1024).await?;
        (
            Bundle::from_zip(&archive_bytes, Some(sub))?,
            format!("github:{commit}"),
        )
    };
    Ok((
        bundle,
        Origin {
            owner: owner.to_owned(),
            slug: entry.slug.clone(),
            version: pinned_version,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_keeps_owner_identity_and_versions() {
        let p = parse_page(&serde_json::json!({"items":[{"ownerHandle":"a","slug":"pdf","displayName":"PDF","summary":"Files","latestVersion":{"version":"1.0"}},{"ownerHandle":"b","slug":"pdf","tags":{"latest":"2.0"}}],"nextCursor":"next"})).unwrap();
        assert_eq!(p.entries.len(), 2);
        assert_ne!(p.entries[0].key(), p.entries[1].key());
        assert_eq!(p.entries[1].version, "2.0");
        assert_eq!(p.cursor.as_deref(), Some("next"));
    }
    #[test]
    fn search_drops_non_skill_handoffs_and_encodes_links() {
        let p = parse_page(&serde_json::json!({"results":[{"ownerHandle":"a","slug":"pdf","install":{"kind":"clawhub"}},{"ownerHandle":"a","slug":"other","install":{"kind":"plugin"}}]})).unwrap();
        assert_eq!(p.entries.len(), 1);
        assert_eq!(p.entries[0].source_url(), "https://clawhub.ai/a/pdf");
    }
    #[tokio::test]
    #[ignore = "Explicit public-network integration test; never installs to real clients"]
    async fn live_directory_search_and_full_download() {
        let p = fetch("", None).await.unwrap();
        assert!(!p.entries.is_empty());
        let p = fetch("pdf", None).await.unwrap();
        assert!(!p.entries.is_empty());
        let entry = Entry {
            owner: "awspace".into(),
            slug: "pdf".into(),
            title: "PDF".into(),
            description: String::new(),
            version: "0.1.0".into(),
        };
        let (bundle, origin) = download(&entry).await.unwrap();
        assert_eq!(origin.key(), "awspace/pdf");
        assert!(bundle.files.len() > 1);
    }
}
