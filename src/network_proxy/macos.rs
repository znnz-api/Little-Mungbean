//! CFNetwork evaluates PAC with the same system JavaScript engine as macOS.
//! All CF objects and the private run loop stay on the blocking resolver thread.
use super::*;
use std::ffi::{c_char, c_void};

mod cache;
#[cfg(test)]
mod tests;

pub(super) fn refresh_cache() {
    cache::refresh();
}

pub(super) fn resolve_cached(url: &Url, settings: &SystemSettings) -> Result<Vec<Route>> {
    cache::resolve_cached(url, settings)
}

type Ref = *const c_void;
type Index = isize;
#[repr(C)]
struct StreamContext {
    version: Index,
    info: *mut c_void,
    retain: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
    release: Option<unsafe extern "C" fn(*const c_void)>,
    copy_description: Option<unsafe extern "C" fn(*const c_void) -> Ref>,
}
type Callback = unsafe extern "C" fn(*mut c_void, Ref, Ref);

#[link(name = "CFNetwork", kind = "framework")]
unsafe extern "C" {
    fn CFNetworkCopySystemProxySettings() -> Ref;
    fn CFNetworkCopyProxiesForURL(target: Ref, settings: Ref) -> Ref;
    fn CFNetworkExecuteProxyAutoConfigurationURL(
        pac: Ref,
        target: Ref,
        callback: Callback,
        context: *mut StreamContext,
    ) -> Ref;
    fn CFNetworkExecuteProxyAutoConfigurationScript(
        script: Ref,
        target: Ref,
        callback: Callback,
        context: *mut StreamContext,
    ) -> Ref;
    static kCFProxyTypeKey: Ref;
    static kCFProxyTypeNone: Ref;
    static kCFProxyTypeHTTP: Ref;
    static kCFProxyTypeHTTPS: Ref;
    static kCFProxyTypeSOCKS: Ref;
    static kCFProxyTypeAutoConfigurationURL: Ref;
    static kCFProxyTypeAutoConfigurationJavaScript: Ref;
    static kCFProxyAutoConfigurationURLKey: Ref;
    static kCFProxyAutoConfigurationJavaScriptKey: Ref;
    static kCFProxyHostNameKey: Ref;
    static kCFProxyPortNumberKey: Ref;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: Ref);
    fn CFRetain(value: Ref) -> Ref;
    fn CFErrorGetDomain(error: Ref) -> Ref;
    fn CFErrorGetCode(error: Ref) -> Index;
    fn CFEqual(left: Ref, right: Ref) -> u8;
    fn CFGetTypeID(value: Ref) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFArrayGetTypeID() -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFBooleanGetTypeID() -> usize;
    fn CFBooleanGetValue(value: Ref) -> u8;
    fn CFStringCreateWithBytes(
        allocator: Ref,
        bytes: *const u8,
        length: Index,
        encoding: u32,
        external: u8,
    ) -> Ref;
    fn CFStringGetLength(value: Ref) -> Index;
    fn CFStringGetMaximumSizeForEncoding(length: Index, encoding: u32) -> Index;
    fn CFStringGetCString(value: Ref, buffer: *mut c_char, size: Index, encoding: u32) -> u8;
    fn CFDictionaryGetValue(dictionary: Ref, key: Ref) -> Ref;
    fn CFNumberGetValue(number: Ref, kind: Index, value: *mut c_void) -> u8;
    fn CFArrayGetCount(array: Ref) -> Index;
    fn CFArrayGetValueAtIndex(array: Ref, index: Index) -> Ref;
    fn CFURLCreateWithString(allocator: Ref, string: Ref, base: Ref) -> Ref;
    fn CFRunLoopGetCurrent() -> Ref;
    fn CFRunLoopAddSource(run_loop: Ref, source: Ref, mode: Ref);
    fn CFRunLoopRemoveSource(run_loop: Ref, source: Ref, mode: Ref);
    fn CFRunLoopRunInMode(mode: Ref, seconds: f64, return_after_source: u8) -> i32;
    fn CFRunLoopSourceInvalidate(source: Ref);
    static kCFRunLoopDefaultMode: Ref;
}

struct Owned(Ref);
impl Owned {
    fn new(value: Ref) -> Result<Self> {
        if value.is_null() {
            bail!("macOS 代理设置不可用 / macOS proxy settings unavailable");
        }
        Ok(Self(value))
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}

fn cf_string(value: &str) -> Result<Owned> {
    Owned::new(unsafe {
        CFStringCreateWithBytes(
            std::ptr::null(),
            value.as_ptr(),
            value.len() as Index,
            0x08000100,
            0,
        )
    })
}
fn string(value: Ref) -> Option<String> {
    if value.is_null() || unsafe { CFGetTypeID(value) != CFStringGetTypeID() } {
        return None;
    }
    let size = unsafe { CFStringGetMaximumSizeForEncoding(CFStringGetLength(value), 0x08000100) }
        .checked_add(1)?;
    if !(1..=1024 * 1024).contains(&size) {
        return None;
    }
    let mut buffer = vec![0u8; size as usize];
    if unsafe { CFStringGetCString(value, buffer.as_mut_ptr().cast(), size, 0x08000100) } == 0 {
        return None;
    }
    let end = buffer.iter().position(|&byte| byte == 0)?;
    String::from_utf8(buffer[..end].to_vec()).ok()
}
fn dictionary_value(dictionary: Ref, key: Ref) -> Ref {
    if dictionary.is_null() || unsafe { CFGetTypeID(dictionary) != CFDictionaryGetTypeID() } {
        return std::ptr::null();
    }
    unsafe { CFDictionaryGetValue(dictionary, key) }
}
fn field(dictionary: Ref, key: &str) -> Result<Ref> {
    Ok(dictionary_value(dictionary, cf_string(key)?.0))
}
fn number(value: Ref) -> Option<i32> {
    if value.is_null() {
        return None;
    }
    if unsafe { CFGetTypeID(value) == CFBooleanGetTypeID() } {
        return Some(i32::from(unsafe { CFBooleanGetValue(value) } != 0));
    }
    if unsafe { CFGetTypeID(value) != CFNumberGetTypeID() } {
        return None;
    }
    let mut result = 0i32;
    (unsafe { CFNumberGetValue(value, 3, (&mut result as *mut i32).cast()) } != 0).then_some(result)
}
fn array(value: Ref) -> Vec<Ref> {
    if value.is_null() || unsafe { CFGetTypeID(value) != CFArrayGetTypeID() } {
        return Vec::new();
    }
    (0..unsafe { CFArrayGetCount(value) })
        .map(|index| unsafe { CFArrayGetValueAtIndex(value, index) })
        .collect()
}
fn cf_url(value: &str) -> Result<Owned> {
    let string = cf_string(value)?;
    Owned::new(unsafe { CFURLCreateWithString(std::ptr::null(), string.0, std::ptr::null()) })
}

pub(super) fn settings() -> Result<SystemSettings> {
    let dictionary = Owned::new(unsafe { CFNetworkCopySystemProxySettings() })?;
    let d = dictionary.0;
    let mut result = SystemSettings::default();
    for (prefix, scheme) in [("HTTP", "http"), ("HTTPS", "http"), ("SOCKS", "socks5h")] {
        if number(field(d, &format!("{prefix}Enable"))?) != Some(1) {
            continue;
        }
        let host = string(field(d, &format!("{prefix}Proxy"))?)
            .context("macOS 缺少代理主机 / Missing macOS proxy host")?;
        let port = number(field(d, &format!("{prefix}Port"))?)
            .filter(|port| (1..=65535).contains(port))
            .context("macOS 代理端口无效 / Invalid macOS proxy port")?;
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host
        };
        let proxy = normalize_proxy(&format!("{scheme}://{host}:{port}"), scheme)?;
        match prefix {
            "HTTP" => result.fixed.http = Some(proxy),
            "HTTPS" => result.fixed.https = Some(proxy),
            _ => result.fixed.all = Some(proxy),
        }
    }
    result.bypass.patterns = array(field(d, "ExceptionsList")?)
        .into_iter()
        .filter_map(string)
        .map(|v| v.to_ascii_lowercase())
        .collect();
    if number(field(d, "ExcludeSimpleHostnames")?) == Some(1) {
        result.bypass.patterns.push("<local>".into());
    }
    if number(field(d, "ProxyAutoConfigEnable")?) == Some(1) {
        result.pac_url =
            string(field(d, "ProxyAutoConfigURLString")?).filter(|url| !url.trim().is_empty());
        if result.pac_url.is_none() {
            bail!("macOS 自动代理脚本地址为空 / Missing macOS PAC URL");
        }
    }
    result.auto_detect = number(field(d, "ProxyAutoDiscoveryEnable")?) == Some(1);
    Ok(result)
}

struct Completion {
    result: Option<Result<Owned>>,
}

fn pac_error(error: Ref) -> anyhow::Error {
    let domain = string(unsafe { CFErrorGetDomain(error) }).unwrap_or_default();
    let code = unsafe { CFErrorGetCode(error) };
    // CFError descriptions/userInfo may contain PAC URLs, credentials, or even
    // script contents. Keep native codes, but use only controlled descriptions.
    let domain = match domain.as_str() {
        "NSURLErrorDomain" => "NSURLErrorDomain",
        "kCFErrorDomainCFNetwork" => "kCFErrorDomainCFNetwork",
        "NSPOSIXErrorDomain" | "kCFErrorDomainPOSIX" => "POSIX",
        "NSOSStatusErrorDomain" | "kCFErrorDomainOSStatus" => "OSStatus",
        _ => "Other",
    };
    let reason = match (domain, code) {
        ("NSURLErrorDomain", -1001) => "下载超时 / Download timed out",
        ("NSURLErrorDomain", -1003 | -1006) => "域名解析失败 / DNS resolution failed",
        ("NSURLErrorDomain", -1004) => "无法连接脚本服务器 / Cannot connect to PAC server",
        ("NSURLErrorDomain", -1005 | -1009) => "网络连接不可用 / Network connection unavailable",
        ("NSURLErrorDomain", -1011 | -1017) => "脚本服务器响应无效 / Invalid PAC server response",
        ("NSURLErrorDomain", -1012 | -1013) => {
            "脚本下载需要认证 / PAC download authentication failed"
        }
        ("NSURLErrorDomain", -1022) => {
            "系统安全策略拒绝下载 / Download rejected by system security policy"
        }
        ("NSURLErrorDomain", -1206..=-1200) => {
            "TLS 或证书验证失败 / TLS or certificate validation failed"
        }
        ("kCFErrorDomainCFNetwork", 308) => "脚本语法或执行失败 / PAC script execution failed",
        ("kCFErrorDomainCFNetwork", 309) => "脚本下载需要认证 / PAC download authentication failed",
        _ => "系统自动代理操作失败 / System automatic proxy operation failed",
    };
    anyhow::anyhow!(
        "macOS PAC 下载或执行失败 / macOS PAC download or execution failed ({domain}, code {code}): {reason}"
    )
}

unsafe extern "C" fn pac_callback(context: *mut c_void, proxies: Ref, error: Ref) {
    let completion = unsafe { &mut *(context as *mut Completion) };
    completion.result = Some(if !error.is_null() {
        Err(pac_error(error))
    } else if proxies.is_null() {
        Err(anyhow::anyhow!(
            "macOS PAC 下载或执行失败 / macOS PAC download or execution failed"
        ))
    } else {
        Owned::new(unsafe { CFRetain(proxies) })
    });
}

fn execute_pac(value: Ref, target: Ref, is_script: bool) -> Result<Owned> {
    let mut completion = Completion { result: None };
    let mut context = StreamContext {
        version: 0,
        info: (&mut completion as *mut Completion).cast(),
        retain: None,
        release: None,
        copy_description: None,
    };
    let source = Owned::new(unsafe {
        if is_script {
            CFNetworkExecuteProxyAutoConfigurationScript(value, target, pac_callback, &mut context)
        } else {
            CFNetworkExecuteProxyAutoConfigurationURL(value, target, pac_callback, &mut context)
        }
    })?;
    let run_loop = unsafe { CFRunLoopGetCurrent() };
    let mode = unsafe { kCFRunLoopDefaultMode };
    unsafe { CFRunLoopAddSource(run_loop, source.0, mode) };
    let deadline = Instant::now() + Duration::from_secs(15);
    while completion.result.is_none() && Instant::now() < deadline {
        unsafe {
            CFRunLoopRunInMode(
                mode,
                deadline
                    .saturating_duration_since(Instant::now())
                    .as_secs_f64(),
                1,
            )
        };
    }
    // Invalidate before releasing the stack callback context, also on timeout.
    unsafe {
        CFRunLoopRemoveSource(run_loop, source.0, mode);
        CFRunLoopSourceInvalidate(source.0);
    }
    completion.result.unwrap_or_else(|| {
        Err(anyhow::anyhow!(
            "macOS PAC 解析超时 / macOS PAC resolution timed out"
        ))
    })
}

fn routes_from_array(proxies: Ref, target: Ref, depth: usize) -> Result<Vec<Route>> {
    if depth > 2 {
        bail!("macOS PAC 递归配置无效 / Invalid recursive macOS PAC configuration");
    }
    let mut routes = Vec::new();
    for proxy in array(proxies) {
        let kind = dictionary_value(proxy, unsafe { kCFProxyTypeKey });
        if kind.is_null() {
            bail!("macOS 代理类型缺失 / Missing macOS proxy type");
        }
        let same = |other| unsafe { CFEqual(kind, other) != 0 };
        let mut resolved = if same(unsafe { kCFProxyTypeNone }) {
            vec![Route::Direct]
        } else if same(unsafe { kCFProxyTypeAutoConfigurationURL })
            || same(unsafe { kCFProxyTypeAutoConfigurationJavaScript })
        {
            let script = same(unsafe { kCFProxyTypeAutoConfigurationJavaScript });
            let value = dictionary_value(proxy, unsafe {
                if script {
                    kCFProxyAutoConfigurationJavaScriptKey
                } else {
                    kCFProxyAutoConfigurationURLKey
                }
            });
            if value.is_null() {
                bail!("macOS PAC 配置为空 / Empty macOS PAC configuration");
            }
            let result = execute_pac(value, target, script)?;
            routes_from_array(result.0, target, depth + 1)?
        } else {
            // CFNetwork's HTTPS type means an HTTP CONNECT tunnel for an
            // HTTPS destination, not TLS between us and the proxy itself.
            let scheme = if same(unsafe { kCFProxyTypeHTTP }) || same(unsafe { kCFProxyTypeHTTPS })
            {
                "http"
            } else if same(unsafe { kCFProxyTypeSOCKS }) {
                "socks5h"
            } else {
                bail!("macOS 返回不支持的代理类型 / Unsupported macOS proxy type");
            };
            let host = string(dictionary_value(proxy, unsafe { kCFProxyHostNameKey }))
                .context("macOS PAC 代理地址为空 / Missing macOS PAC proxy host")?;
            let port = number(dictionary_value(proxy, unsafe { kCFProxyPortNumberKey }))
                .filter(|port| (1..=65535).contains(port))
                .context("macOS PAC 端口无效 / Invalid macOS PAC port")?;
            let host = if host.contains(':') && !host.starts_with('[') {
                format!("[{host}]")
            } else {
                host
            };
            vec![Route::Proxy(normalize_proxy(
                &format!("{scheme}://{host}:{port}"),
                scheme,
            )?)]
        };
        for route in resolved.drain(..) {
            if !routes.contains(&route) {
                routes.push(route);
            }
        }
    }
    if routes.is_empty() {
        bail!("macOS PAC 未返回路由 / macOS PAC returned no route");
    }
    Ok(routes)
}

pub(super) fn resolve(url: &Url, settings: &SystemSettings) -> Result<Vec<Route>> {
    let target = cf_url(url.as_str())?;
    let proxies = if let Some(pac) = &settings.pac_url {
        let pac = cf_url(pac)?;
        execute_pac(pac.0, target.0, false)?
    } else {
        let system = Owned::new(unsafe { CFNetworkCopySystemProxySettings() })?;
        Owned::new(unsafe { CFNetworkCopyProxiesForURL(target.0, system.0) })?
    };
    routes_from_array(proxies.0, target.0, 0)
}
