//! Resolve outbound proxy policy before selecting a connection pool. reqwest's
//! custom proxy callback loses URL paths and (0.12.28) ignores its NO_PROXY field.
use anyhow::{Context, Result, bail};
use reqwest::{ClientBuilder, IntoUrl, Request, Response};
use serde::Serialize;
use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use url::Url;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(test)]
mod tests;
#[cfg(windows)]
mod windows;

const LOCAL_NO_PROXY: &str = "127.0.0.1,localhost,::1,127.0.0.0/8";
const NETWORK_RETRY_DELAY: Duration = Duration::from_millis(500);
const PAC_CACHE_TTL: Duration = Duration::from_secs(60);

// These strings can contain passwords; Debug must always redact them.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Route {
    Direct,
    Proxy(String),
}
impl fmt::Debug for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Direct => f.write_str("Direct"),
            Self::Proxy(raw) => {
                let address = Url::parse(raw)
                    .ok()
                    .map(|url| {
                        format!(
                            "{}://{}:{}",
                            url.scheme(),
                            url.host_str().unwrap_or("?"),
                            url.port_or_known_default().unwrap_or(1080)
                        )
                    })
                    .unwrap_or_else(|| "invalid proxy".into());
                f.debug_tuple("Proxy").field(&address).finish()
            }
        }
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
struct FixedProxies {
    http: Option<String>,
    https: Option<String>,
    all: Option<String>,
}
impl FixedProxies {
    fn for_url(&self, url: &Url) -> Option<&str> {
        match url.scheme() {
            "http" => self.http.as_deref(),
            "https" => self.https.as_deref(),
            _ => None,
        }
        .or(self.all.as_deref())
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
struct SystemSettings {
    fixed: FixedProxies,
    bypass: Bypass,
    pac_url: Option<String>,
    auto_detect: bool,
}

#[derive(Clone, Default, PartialEq, Eq)]
struct Settings {
    environment: FixedProxies,
    bypass: Bypass,
    system: SystemSettings,
}
impl Settings {
    fn resolve_current(url: &Url) -> Result<Vec<Route>> {
        let environment = std::env::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
            .collect();
        let mut settings =
            Self::from_environment_and_system(environment, SystemSettings::default())?;
        // An explicit environment route must not depend on an unrelated or
        // unavailable OS proxy configuration.
        if is_loopback(url)
            || settings.bypass.contains(url)
            || settings.environment.for_url(url).is_some()
        {
            return settings.resolve(url);
        }
        settings.system = system_settings()?;
        settings.resolve(url)
    }
    fn read() -> Result<Self> {
        let environment = std::env::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
            .collect();
        Self::from_environment_and_system(environment, system_settings()?)
    }
    fn from_environment_and_system(
        env: HashMap<String, String>,
        system: SystemSettings,
    ) -> Result<Self> {
        let value = |upper: &str, lower: &str| {
            env.get(upper)
                .filter(|v| !v.trim().is_empty())
                .or_else(|| env.get(lower).filter(|v| !v.trim().is_empty()))
                .map(|v| normalize_proxy(v, "http"))
                .transpose()
        };
        Ok(Self {
            environment: FixedProxies {
                http: value("HTTP_PROXY", "http_proxy")?,
                https: value("HTTPS_PROXY", "https_proxy")?,
                all: value("ALL_PROXY", "all_proxy")?,
            },
            bypass: Bypass::parse(&format!(
                "{LOCAL_NO_PROXY},{}",
                env.get("NO_PROXY")
                    .or_else(|| env.get("no_proxy"))
                    .map(String::as_str)
                    .unwrap_or_default()
            )),
            system,
        })
    }
    fn resolve(&self, url: &Url) -> Result<Vec<Route>> {
        if is_loopback(url) || self.bypass.contains(url) {
            return Ok(vec![Route::Direct]);
        }
        if let Some(proxy) = self.environment.for_url(url) {
            return Ok(vec![Route::Proxy(proxy.to_owned())]);
        }
        if self.system.bypass.contains(url) {
            return Ok(vec![Route::Direct]);
        }
        if self.system.pac_url.is_some() || self.system.auto_detect {
            return resolve_auto_cached(url, &self.system);
        }
        Ok(vec![
            self.system
                .fixed
                .for_url(url)
                .map(|p| Route::Proxy(p.to_owned()))
                .unwrap_or(Route::Direct),
        ])
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
struct Bypass {
    patterns: Vec<String>,
}
impl Bypass {
    fn parse(raw: &str) -> Self {
        Self {
            patterns: raw
                .split([',', ';'])
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_ascii_lowercase)
                .collect(),
        }
    }
    fn contains(&self, url: &Url) -> bool {
        let Some(host) = url.host_str() else {
            return false;
        };
        let host = host
            .trim_matches(['[', ']'])
            .trim_end_matches('.')
            .to_ascii_lowercase();
        self.patterns.iter().any(|pattern| {
            if pattern == "*" {
                return true;
            }
            if pattern == "<local>" {
                return !host.contains('.') && host.parse::<IpAddr>().is_err();
            }
            if let Some((network, prefix)) = pattern.split_once('/') {
                return cidr_contains(network, prefix, &host);
            }
            let (pattern, port) = split_bypass_port(pattern);
            if port.is_some_and(|port| url.port_or_known_default() != Some(port)) {
                return false;
            }
            let pattern = pattern.trim_matches(['[', ']']).trim_end_matches('.');
            if pattern.contains('*') || pattern.contains('?') {
                return wildcard_matches(pattern.as_bytes(), host.as_bytes());
            }
            let domain = pattern.trim_start_matches('.');
            if domain.parse::<IpAddr>().is_ok() {
                return host == domain;
            }
            host == domain
                || (host.ends_with(domain)
                    && host
                        .as_bytes()
                        .get(host.len().saturating_sub(domain.len() + 1))
                        == Some(&b'.'))
        })
    }
    fn environment_value(&self) -> String {
        self.patterns.join(",")
    }
}

fn split_bypass_port(pattern: &str) -> (&str, Option<u16>) {
    if pattern.starts_with('[') {
        if let Some((host, port)) = pattern.rsplit_once("]:") {
            return (&host[1..], port.parse().ok());
        }
    } else if pattern.matches(':').count() == 1
        && let Some((host, port)) = pattern.rsplit_once(':')
        && let Ok(port) = port.parse()
    {
        return (host, Some(port));
    }
    (pattern, None)
}
fn cidr_contains(network: &str, prefix: &str, host: &str) -> bool {
    match (
        network.parse::<IpAddr>(),
        host.parse::<IpAddr>(),
        prefix.parse::<u32>(),
    ) {
        (Ok(IpAddr::V4(network)), Ok(IpAddr::V4(host)), Ok(bits @ 0..=32)) => {
            let mask = u32::MAX.checked_shl(32 - bits).unwrap_or(0);
            u32::from(network) & mask == u32::from(host) & mask
        }
        (Ok(IpAddr::V6(network)), Ok(IpAddr::V6(host)), Ok(bits @ 0..=128)) => {
            let mask = u128::MAX.checked_shl(128 - bits).unwrap_or(0);
            u128::from(network) & mask == u128::from(host) & mask
        }
        _ => false,
    }
}
fn wildcard_matches(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t, mut star, mut retry) = (0, 0, None, 0);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            retry = t;
        } else if let Some(at) = star {
            retry += 1;
            t = retry;
            p = at + 1;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}
fn is_loopback(url: &Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    })
}

fn normalize_proxy(raw: &str, default_scheme: &str) -> Result<String> {
    let raw = raw.trim();
    let candidate = if raw.contains("://") {
        raw.to_owned()
    } else {
        format!("{default_scheme}://{raw}")
    };
    let url = Url::parse(&candidate)
        .map_err(|_| anyhow::anyhow!("代理地址格式无效 / Invalid proxy address"))?;
    if !matches!(
        url.scheme(),
        "http" | "https" | "socks4" | "socks4a" | "socks5" | "socks5h"
    ) || url.host_str().is_none()
        || url.port() == Some(0)
        || url.path() != "/" && !url.path().is_empty()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!(
            "代理地址需使用 HTTP、HTTPS 或 SOCKS，并且不能包含路径 / Invalid proxy protocol or path"
        );
    }
    Ok(url.to_string().trim_end_matches('/').to_owned())
}
fn parse_fixed_proxies(raw: &str) -> Result<FixedProxies> {
    let mut fixed = FixedProxies::default();
    for entry in raw.split(';').map(str::trim).filter(|v| !v.is_empty()) {
        if let Some((scheme, address)) = entry.split_once('=')
            && matches!(
                scheme.trim().to_ascii_lowercase().as_str(),
                "http" | "https" | "socks" | "socks5"
            )
        {
            let scheme = scheme.trim().to_ascii_lowercase();
            let proxy = normalize_proxy(
                address,
                if scheme.starts_with("socks") {
                    "socks5h"
                } else {
                    "http"
                },
            )?;
            match scheme.as_str() {
                "http" => fixed.http = Some(proxy),
                "https" => fixed.https = Some(proxy),
                _ => fixed.all = Some(proxy),
            }
        } else {
            fixed.all = Some(normalize_proxy(entry, "http")?);
        }
    }
    Ok(fixed)
}
struct CachedAuto {
    settings: SystemSettings,
    resolved_at: Instant,
    routes: Vec<Route>,
}
#[cfg(target_os = "macos")]
pub(crate) fn refresh_macos_pac() {
    macos::refresh_cache();
}

#[cfg(target_os = "macos")]
fn resolve_auto_cached(url: &Url, settings: &SystemSettings) -> Result<Vec<Route>> {
    macos::resolve_cached(url, settings)
}

#[cfg(not(target_os = "macos"))]
fn resolve_auto_cached(url: &Url, settings: &SystemSettings) -> Result<Vec<Route>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedAuto>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    // PAC may inspect URL paths, so caching only the origin is incorrect.
    let key = url.as_str().to_owned();
    if let Ok(cache) = cache.lock()
        && let Some(cached) = cache.get(&key)
        && cached.settings == *settings
        && cached.resolved_at.elapsed() < PAC_CACHE_TTL
    {
        return Ok(cached.routes.clone());
    }
    let routes = resolve_system_auto(url, settings)
        .context("自动代理解析失败 / Automatic proxy resolution failed")?;
    if let Ok(mut cache) = cache.lock() {
        if cache.len() >= 128 {
            cache.clear();
        }
        cache.insert(
            key,
            CachedAuto {
                settings: settings.clone(),
                resolved_at: Instant::now(),
                routes: routes.clone(),
            },
        );
    }
    Ok(routes)
}
#[cfg(windows)]
fn system_settings() -> Result<SystemSettings> {
    windows::settings()
}
#[cfg(windows)]
fn resolve_system_auto(url: &Url, settings: &SystemSettings) -> Result<Vec<Route>> {
    windows::resolve(url, settings)
}
#[cfg(target_os = "macos")]
fn system_settings() -> Result<SystemSettings> {
    macos::settings()
}
#[cfg(target_os = "macos")]
fn resolve_system_auto(url: &Url, settings: &SystemSettings) -> Result<Vec<Route>> {
    macos::resolve(url, settings)
}
#[cfg(not(any(windows, target_os = "macos")))]
fn system_settings() -> Result<SystemSettings> {
    Ok(SystemSettings::default())
}
#[cfg(not(any(windows, target_os = "macos")))]
fn resolve_system_auto(_url: &Url, _settings: &SystemSettings) -> Result<Vec<Route>> {
    Ok(vec![Route::Direct])
}

type Factory = dyn Fn() -> ClientBuilder + Send + Sync;
type Resolver = dyn Fn(&Url) -> Result<Vec<Route>> + Send + Sync;

#[derive(Clone)]
pub struct Client {
    inner: Arc<ClientInner>,
}
struct ClientInner {
    template: reqwest::Client,
    factory: Option<Box<Factory>>,
    clients: Mutex<HashMap<Route, reqwest::Client>>,
    resolver: Box<Resolver>,
    redirects: usize,
    timeout: Option<Duration>,
}
impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NetworkClient")
    }
}
impl Client {
    pub fn new(factory: impl Fn() -> ClientBuilder + Send + Sync + 'static) -> Result<Self> {
        Self::with_resolver(
            factory,
            |url| {
                if is_loopback(url) {
                    Ok(vec![Route::Direct])
                } else {
                    Settings::resolve_current(url)
                }
            },
            10,
        )
    }
    fn with_resolver(
        factory: impl Fn() -> ClientBuilder + Send + Sync + 'static,
        resolver: impl Fn(&Url) -> Result<Vec<Route>> + Send + Sync + 'static,
        redirects: usize,
    ) -> Result<Self> {
        let template = factory()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()?;
        Ok(Self {
            inner: Arc::new(ClientInner {
                template,
                factory: Some(Box::new(factory)),
                clients: Mutex::new(HashMap::new()),
                resolver: Box::new(resolver),
                redirects,
                timeout: None,
            }),
        })
    }
    pub fn redirect_limit(mut self, limit: usize) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("configure before sharing a client")
            .redirects = limit;
        self
    }
    pub fn total_timeout(mut self, timeout: Duration) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("configure before sharing a client")
            .timeout = Some(timeout);
        self
    }
    pub fn direct(client: reqwest::Client) -> Self {
        Self {
            inner: Arc::new(ClientInner {
                template: client,
                factory: None,
                clients: Mutex::new(HashMap::new()),
                resolver: Box::new(|_| Ok(vec![Route::Direct])),
                redirects: 0,
                timeout: None,
            }),
        }
    }
    pub fn get(&self, url: impl IntoUrl) -> RequestBuilder {
        self.request(reqwest::Method::GET, url)
    }
    pub fn request(&self, method: reqwest::Method, url: impl IntoUrl) -> RequestBuilder {
        RequestBuilder {
            client: self.clone(),
            request: self.inner.template.request(method, url),
        }
    }
    fn client_for(&self, route: &Route) -> Result<reqwest::Client> {
        let Some(factory) = &self.inner.factory else {
            return Ok(self.inner.template.clone());
        };
        let mut clients =
            self.inner.clients.lock().map_err(|_| {
                anyhow::anyhow!("代理连接池不可用 / Proxy connection pool unavailable")
            })?;
        if let Some(client) = clients.get(route) {
            return Ok(client.clone());
        }
        let mut builder = factory()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never());
        if let Route::Proxy(proxy) = route {
            builder = builder.proxy(reqwest::Proxy::all(proxy)?);
        }
        let client = builder.build()?;
        if clients.len() >= 16 {
            clients.clear();
        }
        clients.insert(route.clone(), client.clone());
        Ok(client)
    }
    async fn execute(
        &self,
        mut request: Request,
        deadline: Option<tokio::time::Instant>,
    ) -> std::result::Result<Response, Error> {
        for redirects in 0..=self.inner.redirects {
            let resolver = self.inner.clone();
            let url = request.url().clone();
            let routes = tokio::task::spawn_blocking(move || (resolver.resolver)(&url))
                .await
                .map_err(|_| Error::policy("代理设置读取失败 / Could not read proxy settings"))?
                .map_err(Error::Policy)?;
            if routes.is_empty() {
                return Err(Error::policy("未找到可用代理路由 / No usable proxy route"));
            }
            let mut response = None;
            for (index, route) in routes.iter().enumerate() {
                let mut attempt = request.try_clone().ok_or_else(|| {
                    Error::policy("请求体不支持安全重放 / Request body cannot be replayed safely")
                })?;
                if let Some(deadline) = deadline {
                    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                    if remaining.is_zero() {
                        return Err(Error::Timeout);
                    }
                    *attempt.timeout_mut() = Some(remaining);
                }
                match self
                    .client_for(route)
                    .map_err(Error::Policy)?
                    .execute(attempt)
                    .await
                {
                    Ok(result) => {
                        response = Some(result);
                        break;
                    }
                    // Only connection failures precede user request bytes.
                    // Never replay on response, body, stream errors or 407.
                    Err(error)
                        if error.is_connect()
                            && !is_proxy_auth_error(&error)
                            && index + 1 < routes.len() =>
                    {
                        tracing::warn!(route = ?route, "代理连接失败，尝试下一条 PAC 路由 / Proxy connection failed; trying next PAC route");
                    }
                    Err(error) => {
                        return Err(Error::Transport {
                            error: error.without_url(),
                            route: format!("{route:?}"),
                        });
                    }
                }
            }
            let response =
                response.ok_or_else(|| Error::policy("代理连接失败 / Proxy connection failed"))?;
            if !is_redirect(response.status()) || self.inner.redirects == 0 {
                return Ok(response);
            }
            let Some(location) = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
            else {
                return Ok(response);
            };
            if redirects == self.inner.redirects {
                return Err(Error::policy("重定向次数过多 / Too many redirects"));
            }
            let next = request
                .url()
                .join(location)
                .map_err(|_| Error::policy("无效的重定向地址 / Invalid redirect URL"))?;
            if !matches!(next.scheme(), "http" | "https")
                || !next.username().is_empty()
                || next.password().is_some()
                || request.url().scheme() == "https" && next.scheme() == "http"
            {
                return Err(Error::policy(
                    "拒绝不安全的重定向 / Unsafe redirect rejected",
                ));
            }
            if next.origin() != request.url().origin() {
                for header in [
                    "authorization",
                    "cookie",
                    "proxy-authorization",
                    "x-api-key",
                    "x-goog-api-key",
                    "host",
                ] {
                    request.headers_mut().remove(header);
                }
            }
            if response.status() == reqwest::StatusCode::SEE_OTHER
                && request.method() != reqwest::Method::HEAD
                || matches!(
                    response.status(),
                    reqwest::StatusCode::MOVED_PERMANENTLY | reqwest::StatusCode::FOUND
                ) && request.method() == reqwest::Method::POST
            {
                *request.method_mut() = reqwest::Method::GET;
                *request.body_mut() = None;
                for header in [
                    "content-length",
                    "content-type",
                    "transfer-encoding",
                    "content-encoding",
                ] {
                    request.headers_mut().remove(header);
                }
            }
            *request.url_mut() = next;
        }
        unreachable!()
    }
}
fn is_redirect(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

fn is_proxy_auth_error(error: &reqwest::Error) -> bool {
    // These connector error enums are private in the locked hyper-util API.
    // CONNECT 407 and SOCKS authentication errors are connection errors too,
    // but must never cause a switch to a different proxy or DIRECT.
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(error) = cause {
        let message = error.to_string();
        if [
            "proxy authorization required",
            "credentials not accepted",
            "server does not support user/pass authentication",
            "server implements authentication incorrectly",
        ]
        .iter()
        .any(|text| message.contains(text))
        {
            return true;
        }
        cause = error.source();
    }
    false
}
impl From<reqwest::Client> for Client {
    fn from(client: reqwest::Client) -> Self {
        Self::direct(client)
    }
}

pub struct RequestBuilder {
    client: Client,
    request: reqwest::RequestBuilder,
}
impl RequestBuilder {
    pub fn header<K, V>(mut self, key: K, value: V) -> Self
    where
        reqwest::header::HeaderName: TryFrom<K>,
        <reqwest::header::HeaderName as TryFrom<K>>::Error: Into<http::Error>,
        reqwest::header::HeaderValue: TryFrom<V>,
        <reqwest::header::HeaderValue as TryFrom<V>>::Error: Into<http::Error>,
    {
        self.request = self.request.header(key, value);
        self
    }
    pub fn query<T: Serialize + ?Sized>(mut self, query: &T) -> Self {
        self.request = self.request.query(query);
        self
    }
    pub fn body(mut self, body: impl Into<reqwest::Body>) -> Self {
        self.request = self.request.body(body);
        self
    }
    pub fn try_clone(&self) -> Option<Self> {
        Some(Self {
            client: self.client.clone(),
            request: self.request.try_clone()?,
        })
    }
    pub async fn send(self) -> std::result::Result<Response, Error> {
        let request = self.request.build().map_err(Error::Request)?;
        let timeout = request.timeout().copied().or(self.client.inner.timeout);
        let deadline = timeout.map(|timeout| tokio::time::Instant::now() + timeout);
        let operation = self.client.execute(request, deadline);
        if let Some(deadline) = deadline {
            tokio::time::timeout_at(deadline, operation)
                .await
                .map_err(|_| Error::Timeout)?
        } else {
            operation.await
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Policy(anyhow::Error),
    Request(reqwest::Error),
    Transport {
        error: reqwest::Error,
        route: String,
    },
    Timeout,
}
impl Error {
    fn policy(message: &str) -> Self {
        Self::Policy(anyhow::anyhow!("{message}"))
    }
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Policy(_) | Self::Request(_) => false,
            Self::Transport { error, .. } => !is_proxy_auth_error(error),
            Self::Timeout => true,
        }
    }
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Timeout)
            || matches!(self, Self::Request(error) | Self::Transport { error, .. } if error.is_timeout())
    }
    pub fn is_connect(&self) -> bool {
        matches!(self, Self::Request(error) | Self::Transport { error, .. } if error.is_connect())
    }
    pub fn without_url(self) -> Self {
        match self {
            Self::Request(error) => Self::Request(error.without_url()),
            Self::Transport { error, route } => Self::Transport {
                error: error.without_url(),
                route,
            },
            other => other,
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Policy(error) => write!(f, "{error:#}"),
            Self::Request(error) => error.fmt(f),
            Self::Transport { error, route } => {
                write!(f, "网络路由 / Network route: {route}; {error}")
            }
            Self::Timeout => f.write_str("网络请求超时 / Network request timed out"),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Policy(error) => Some(error.as_ref()),
            Self::Request(error) => Some(error),
            Self::Transport { error, .. } => Some(error),
            Self::Timeout => None,
        }
    }
}

/// Only for non-billable, replay-safe discovery requests.
pub async fn send_with_network_retry(
    request: RequestBuilder,
) -> std::result::Result<Response, Error> {
    let retry = request.try_clone();
    match request.send().await {
        Ok(response) => Ok(response),
        Err(error) if !error.is_retryable() => Err(error),
        Err(error) => {
            let Some(retry) = retry else {
                return Err(error);
            };
            tokio::time::sleep(NETWORK_RETRY_DELAY).await;
            retry.send().await
        }
    }
}

pub fn child_proxy_environment(target_url: &str) -> Result<Vec<(String, String)>> {
    let settings = Settings::read()?;
    let url = Url::parse(target_url)?;
    child_environment(&settings, &url)
}

fn child_environment(settings: &Settings, url: &Url) -> Result<Vec<(String, String)>> {
    let mut bypass = settings.bypass.clone();
    bypass
        .patterns
        .extend(settings.system.bypass.patterns.clone());
    let mut result = vec![
        ("NO_PROXY".to_owned(), bypass.environment_value()),
        ("no_proxy".to_owned(), bypass.environment_value()),
    ];
    // Environment variables cannot encode per-URL PAC rules. Model traffic
    // from loopback clients is resolved independently by our local gateway.
    for (upper, lower, scheme) in [
        ("HTTP_PROXY", "http_proxy", "http"),
        ("HTTPS_PROXY", "https_proxy", "https"),
    ] {
        let mut target = url.clone();
        let _ = target.set_scheme(scheme);
        let value = if let Some(value) = settings.environment.for_url(&target) {
            Some(value.to_owned())
        } else if !is_loopback(url)
            && (settings.system.pac_url.is_some() || settings.system.auto_detect)
        {
            match settings.resolve(&target)?.into_iter().next() {
                Some(Route::Proxy(value)) => Some(value),
                _ => None,
            }
        } else {
            settings.system.fixed.for_url(&target).map(str::to_owned)
        };
        if let Some(value) = value {
            result.push((upper.to_owned(), value.clone()));
            result.push((lower.to_owned(), value));
        }
    }
    Ok(result)
}
