use super::*;
use ::windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, HGLOBAL};
use ::windows::Win32::Networking::WinHttp::*;
use ::windows::core::{Owned, PCWSTR, PWSTR};
use std::ffi::c_void;
use std::sync::Condvar;

struct Handle(*mut c_void);
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            let _ = unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

fn take_string(pointer: PWSTR) -> Result<Option<String>> {
    if pointer.is_null() {
        return Ok(None);
    }
    // WinHTTP allocates these strings with GlobalAlloc, including on errors.
    let _allocation = unsafe { Owned::new(HGLOBAL(pointer.0.cast())) };
    let value = unsafe { PCWSTR(pointer.0).to_string() }
        .context("Windows 代理设置编码无效 / Invalid Windows proxy settings")?;
    Ok((!value.trim().is_empty()).then(|| value.trim().to_owned()))
}

pub(super) fn settings() -> Result<SystemSettings> {
    let mut config = WINHTTP_CURRENT_USER_IE_PROXY_CONFIG::default();
    let status = unsafe { WinHttpGetIEProxyConfigForCurrentUser(&mut config) };
    let pac_url = take_string(config.lpszAutoConfigUrl)?;
    let proxy = take_string(config.lpszProxy)?;
    let bypass = take_string(config.lpszProxyBypass)?;
    if let Err(error) = status {
        if error.code() == ::windows::core::HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0) {
            return Ok(SystemSettings::default());
        }
        return Err(error).context(
            "无法读取 Windows 当前连接的代理设置 / Cannot read Windows connection proxy settings",
        );
    }
    Ok(SystemSettings {
        fixed: parse_fixed_proxies(proxy.as_deref().unwrap_or_default())?,
        bypass: Bypass::parse(bypass.as_deref().unwrap_or_default()),
        pac_url,
        auto_detect: config.fAutoDetect.as_bool(),
    })
}

#[derive(Default)]
struct Completion {
    result: Mutex<Option<u32>>,
    changed: Condvar,
}

unsafe extern "system" fn proxy_callback(
    _handle: *mut c_void,
    context: usize,
    status: u32,
    information: *mut c_void,
    length: u32,
) {
    if context == 0 {
        return;
    }
    if status == WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING {
        // WinHTTP guarantees no more callbacks after HANDLE_CLOSING. The raw
        // Arc keeps the context alive even if our blocking wait timed out.
        drop(unsafe { Arc::from_raw(context as *const Completion) });
        return;
    }
    let error = if status == WINHTTP_CALLBACK_STATUS_GETPROXYFORURL_COMPLETE {
        Some(0)
    } else if status == WINHTTP_CALLBACK_STATUS_REQUEST_ERROR
        && !information.is_null()
        && length as usize >= size_of::<WINHTTP_ASYNC_RESULT>()
    {
        Some(unsafe { (*(information as *const WINHTTP_ASYNC_RESULT)).dwError })
    } else {
        None
    };
    if let Some(error) = error {
        let completion = unsafe { &*(context as *const Completion) };
        if let Ok(mut result) = completion.result.lock() {
            *result = Some(error);
            completion.changed.notify_all();
        }
    }
}

/// The extended WinHTTP result preserves the ordered proxy list, including
/// DIRECT entries. The older WinHttpGetProxyForUrl API loses that information.
pub(super) fn resolve(url: &Url, settings: &SystemSettings) -> Result<Vec<Route>> {
    let agent = "Little-Mungbean\0".encode_utf16().collect::<Vec<_>>();
    let session = Handle(unsafe {
        WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            WINHTTP_ACCESS_TYPE_NO_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            WINHTTP_FLAG_ASYNC,
        )
    });
    if session.0.is_null() {
        bail!("无法创建 Windows 自动代理会话 / Cannot create Windows automatic proxy session");
    }
    unsafe { WinHttpSetTimeouts(session.0, 5_000, 5_000, 5_000, 5_000) }
        .context("无法设置自动代理超时 / Cannot set automatic proxy timeout")?;
    // Configure the session before creating the resolver, which inherits this
    // callback. Also request handle closure so its context outlives cancellation.
    let previous = unsafe {
        WinHttpSetStatusCallback(
            session.0,
            Some(proxy_callback),
            WINHTTP_CALLBACK_FLAG_GETPROXYFORURL_COMPLETE
                | WINHTTP_CALLBACK_FLAG_REQUEST_ERROR
                | WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING,
            0,
        )
    };
    if previous.is_some_and(|callback| callback as usize == usize::MAX) {
        bail!("无法设置自动代理回调 / Cannot set automatic proxy callback");
    }
    let mut raw_resolver = std::ptr::null_mut();
    let code = unsafe { WinHttpCreateProxyResolver(session.0, &mut raw_resolver) };
    if code != 0 {
        bail!("Windows 自动代理初始化失败 / Automatic proxy initialization failed ({code})");
    }
    let resolver = Handle(raw_resolver);
    let completion = Arc::new(Completion::default());
    let context = Arc::into_raw(completion.clone()) as usize;
    let set_context = unsafe {
        WinHttpSetOption(
            Some(resolver.0),
            WINHTTP_OPTION_CONTEXT_VALUE,
            Some(&context.to_ne_bytes()),
        )
    };
    if let Err(error) = set_context {
        drop(unsafe { Arc::from_raw(context as *const Completion) });
        return Err(error).context("无法设置自动代理上下文 / Cannot set automatic proxy context");
    }
    let target = url
        .as_str()
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let pac = settings
        .pac_url
        .as_ref()
        .map(|url| url.encode_utf16().chain(Some(0)).collect::<Vec<_>>());
    let options = WINHTTP_AUTOPROXY_OPTIONS {
        dwFlags: (if pac.is_some() {
            WINHTTP_AUTOPROXY_CONFIG_URL
        } else {
            0
        }) | (if settings.auto_detect {
            WINHTTP_AUTOPROXY_AUTO_DETECT
        } else {
            0
        }),
        dwAutoDetectFlags: if settings.auto_detect {
            WINHTTP_AUTO_DETECT_TYPE_DHCP | WINHTTP_AUTO_DETECT_TYPE_DNS_A
        } else {
            0
        },
        lpszAutoConfigUrl: pac
            .as_ref()
            .map(|value| PCWSTR(value.as_ptr()))
            .unwrap_or(PCWSTR::null()),
        // This concerns downloading PAC, not forwarding model credentials.
        fAutoLogonIfChallenged: true.into(),
        ..Default::default()
    };
    let code = unsafe {
        WinHttpGetProxyForUrlEx(resolver.0, PCWSTR(target.as_ptr()), &options, Some(context))
    };
    if code != 0 && code != ERROR_IO_PENDING.0 {
        return automatic_error(code, url, settings);
    }
    let result = completion
        .result
        .lock()
        .map_err(|_| anyhow::anyhow!("自动代理状态不可用 / Automatic proxy state unavailable"))?;
    let (result, timeout) = completion
        .changed
        .wait_timeout_while(result, Duration::from_secs(15), |result| result.is_none())
        .map_err(|_| anyhow::anyhow!("自动代理状态不可用 / Automatic proxy state unavailable"))?;
    if timeout.timed_out() && result.is_none() {
        bail!("Windows PAC 解析超时 / Windows PAC resolution timed out");
    }
    let code = result.unwrap_or(1);
    if code != 0 {
        return automatic_error(code, url, settings);
    }
    drop(result);
    let mut proxies = WINHTTP_PROXY_RESULT::default();
    let code = unsafe { WinHttpGetProxyResult(resolver.0, &mut proxies) };
    if code != 0 {
        bail!("无法读取 Windows PAC 结果 / Cannot read Windows PAC result ({code})");
    }
    struct FreeResult(WINHTTP_PROXY_RESULT);
    impl Drop for FreeResult {
        fn drop(&mut self) {
            unsafe { WinHttpFreeProxyResult(&mut self.0) };
        }
    }
    let proxies = FreeResult(proxies);
    let entries = if proxies.0.cEntries == 0 {
        &[][..]
    } else {
        if proxies.0.pEntries.is_null() {
            bail!("Windows PAC 返回无效列表 / Invalid Windows PAC list");
        }
        unsafe { std::slice::from_raw_parts(proxies.0.pEntries, proxies.0.cEntries as usize) }
    };
    let mut routes = Vec::new();
    for entry in entries {
        let route = if !entry.fProxy.as_bool() || entry.fBypass.as_bool() {
            Route::Direct
        } else {
            if entry.pwszProxy.is_null() {
                bail!("PAC 返回空代理地址 / PAC returned an empty proxy address");
            }
            let host = unsafe { PCWSTR(entry.pwszProxy.0).to_string() }
                .context("PAC 代理地址编码无效 / Invalid PAC proxy address")?;
            let scheme = match entry.ProxyScheme {
                WINHTTP_INTERNET_SCHEME_HTTP => "http",
                WINHTTP_INTERNET_SCHEME_HTTPS => "https",
                WINHTTP_INTERNET_SCHEME_SOCKS => "socks5h",
                _ => bail!("PAC 返回不支持的协议 / Unsupported PAC proxy protocol"),
            };
            let host = if host.contains(':') && !host.starts_with('[') {
                format!("[{host}]")
            } else {
                host
            };
            Route::Proxy(normalize_proxy(
                &format!("{scheme}://{host}:{}", entry.ProxyPort),
                scheme,
            )?)
        };
        if !routes.contains(&route) {
            routes.push(route);
        }
    }
    if routes.is_empty() {
        bail!("Windows PAC 未返回路由 / Windows PAC returned no route");
    }
    Ok(routes)
}

fn automatic_error(code: u32, url: &Url, settings: &SystemSettings) -> Result<Vec<Route>> {
    // Windows enables "automatically detect settings" on ordinary direct
    // networks. No discovered PAC is a normal outcome, not a broken script.
    if code == ERROR_WINHTTP_AUTODETECTION_FAILED && settings.pac_url.is_none() {
        return Ok(vec![
            settings
                .fixed
                .for_url(url)
                .map(|proxy| Route::Proxy(proxy.to_owned()))
                .unwrap_or(Route::Direct),
        ]);
    }
    bail!("Windows PAC 解析失败 / Windows PAC resolution failed ({code})")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_wpad_uses_manual_proxy_or_direct_but_broken_explicit_pac_does_not() {
        let url = Url::parse("https://model.example/v1/models").unwrap();
        let mut settings = SystemSettings {
            auto_detect: true,
            ..Default::default()
        };
        assert_eq!(
            automatic_error(ERROR_WINHTTP_AUTODETECTION_FAILED, &url, &settings).unwrap(),
            vec![Route::Direct]
        );
        settings.fixed = parse_fixed_proxies("https=127.0.0.1:7890").unwrap();
        assert_eq!(
            automatic_error(ERROR_WINHTTP_AUTODETECTION_FAILED, &url, &settings).unwrap(),
            vec![Route::Proxy("http://127.0.0.1:7890".into())]
        );
        settings.pac_url = Some("https://pac.example/proxy.pac".into());
        assert!(automatic_error(ERROR_WINHTTP_AUTODETECTION_FAILED, &url, &settings).is_err());
        assert!(automatic_error(ERROR_WINHTTP_BAD_AUTO_PROXY_SCRIPT, &url, &settings).is_err());
    }
}
