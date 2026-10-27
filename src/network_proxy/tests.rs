use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};

fn url(value: &str) -> Url {
    Url::parse(value).unwrap()
}
fn proxy(port: u16) -> Route {
    Route::Proxy(format!("http://127.0.0.1:{port}"))
}
fn env(values: &[(&str, &str)]) -> HashMap<String, String> {
    values
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}
fn read_headers(stream: &TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
            break;
        }
        headers.push_str(&line);
    }
    headers
}
fn accept(listener: &TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                return stream;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(error) => panic!("test listener: {error}"),
        }
    }
}
fn server(response: impl Into<String>) -> (SocketAddr, std::thread::JoinHandle<String>) {
    let response = response.into();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || {
        let mut stream = accept(&listener);
        let request = read_headers(&stream);
        stream.write_all(response.as_bytes()).unwrap();
        request
    });
    (address, worker)
}
fn client(resolver: impl Fn(&Url) -> Result<Vec<Route>> + Send + Sync + 'static) -> Client {
    Client::with_resolver(
        || reqwest::Client::builder().connect_timeout(Duration::from_millis(350)),
        resolver,
        5,
    )
    .unwrap()
    .total_timeout(Duration::from_secs(3))
}

#[test]
fn fixed_proxies_preserve_http_https_and_socks_separately() {
    let fixed =
        parse_fixed_proxies("http=127.0.0.1:8080;https=127.0.0.1:8081;socks=127.0.0.1:1080")
            .unwrap();
    assert_eq!(
        fixed.for_url(&url("http://example.test")),
        Some("http://127.0.0.1:8080")
    );
    assert_eq!(
        fixed.for_url(&url("https://example.test")),
        Some("http://127.0.0.1:8081")
    );
    assert_eq!(fixed.all.as_deref(), Some("socks5h://127.0.0.1:1080"));
    let fixed = parse_fixed_proxies("127.0.0.1:7890").unwrap();
    assert_eq!(
        fixed.for_url(&url("https://example.test")),
        Some("http://127.0.0.1:7890")
    );
}

#[test]
fn proxy_validation_supports_socks_and_redacts_bad_credentials() {
    for scheme in ["http", "https", "socks4", "socks4a", "socks5", "socks5h"] {
        assert!(
            normalize_proxy(&format!("{scheme}://name:password@127.0.0.1:1080"), "http").is_ok()
        );
    }
    for value in [
        "ftp://name:secret@proxy:21",
        "http://name:secret@proxy:0",
        "http://name:secret@proxy/path",
    ] {
        let error = normalize_proxy(value, "http").unwrap_err();
        assert!(!format!("{error:?}").contains("secret"));
    }
    assert!(
        !format!("{:?}", Route::Proxy("http://name:secret@proxy:8080".into())).contains("secret")
    );
}

#[test]
fn bypass_handles_domains_wildcards_ports_cidr_ipv6_and_local_hosts() {
    let bypass = Bypass::parse(
        "example.com;*.internal;10.0.0.0/8, [::1], 2001:db8::/32, service.test:8443, <local>",
    );
    for target in [
        "https://example.com",
        "https://a.example.com",
        "https://a.internal",
        "http://10.2.3.4",
        "http://[::1]",
        "https://[2001:db8::1]",
        "https://service.test:8443",
        "http://printer",
    ] {
        assert!(bypass.contains(&url(target)), "should bypass {target}");
    }
    for target in [
        "https://notexample.com",
        "http://110.2.3.4",
        "https://service.test:443",
        "https://[2001:db9::1]",
        "https://elsewhere.test",
    ] {
        assert!(!bypass.contains(&url(target)), "should proxy {target}");
    }
    assert!(Bypass::parse("*").contains(&url("https://any.test")));
}

#[test]
fn environment_precedence_is_per_protocol_and_loopback_always_bypasses() {
    let system = SystemSettings {
        fixed: parse_fixed_proxies("http=system:8000;https=system:8443").unwrap(),
        ..Default::default()
    };
    let settings = Settings::from_environment_and_system(
        env(&[
            ("HTTP_PROXY", "http://environment:9000"),
            ("NO_PROXY", "skip.test"),
        ]),
        system,
    )
    .unwrap();
    assert_eq!(
        settings.resolve(&url("http://example.test")).unwrap(),
        vec![Route::Proxy("http://environment:9000".into())]
    );
    assert_eq!(
        settings.resolve(&url("https://example.test")).unwrap(),
        vec![Route::Proxy("http://system:8443".into())]
    );
    for target in [
        "http://localhost",
        "http://127.0.0.1",
        "http://127.8.9.1",
        "http://[::1]",
        "https://skip.test",
    ] {
        assert_eq!(settings.resolve(&url(target)).unwrap(), vec![Route::Direct]);
    }
}

#[test]
fn pac_bypass_is_applied_before_evaluating_an_unreachable_script() {
    let settings = Settings::from_environment_and_system(
        env(&[("no_proxy", "skip.test")]),
        SystemSettings {
            bypass: Bypass::parse("*.internal;<local>"),
            pac_url: Some("http://127.0.0.1:1/broken.pac".into()),
            ..Default::default()
        },
    )
    .unwrap();
    for target in ["http://skip.test", "http://a.internal", "http://printer"] {
        assert_eq!(settings.resolve(&url(target)).unwrap(), vec![Route::Direct]);
    }
}

#[test]
fn child_process_environment_preserves_separate_proxies_and_loopback_bypass() {
    let settings = Settings::from_environment_and_system(
        env(&[("no_proxy", "company.test")]),
        SystemSettings {
            fixed: parse_fixed_proxies("http=proxy-http:8080;https=proxy-https:8081").unwrap(),
            bypass: Bypass::parse("*.internal"),
            ..Default::default()
        },
    )
    .unwrap();
    let values: HashMap<_, _> = child_environment(&settings, &url("http://127.0.0.1:1234"))
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(values["HTTP_PROXY"], "http://proxy-http:8080");
    assert_eq!(values["HTTPS_PROXY"], "http://proxy-https:8081");
    assert_eq!(values["http_proxy"], values["HTTP_PROXY"]);
    assert_eq!(values["https_proxy"], values["HTTPS_PROXY"]);
    for value in [
        "127.0.0.1",
        "localhost",
        "::1",
        "company.test",
        "*.internal",
    ] {
        assert!(values["NO_PROXY"].split(',').any(|item| item == value));
    }
    assert_eq!(values["NO_PROXY"], values["no_proxy"]);
}

#[tokio::test]
async fn same_client_uses_updated_proxy_settings_on_the_next_request() {
    let (first, first_worker) =
        server("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nfirst");
    let (second, second_worker) =
        server("HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecond");
    let route = Arc::new(Mutex::new(proxy(first.port())));
    let source = route.clone();
    let client = client(move |_| Ok(vec![source.lock().unwrap().clone()]));
    assert_eq!(
        client
            .get("http://model.invalid/v1/models")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "first"
    );
    *route.lock().unwrap() = proxy(second.port());
    assert_eq!(
        client
            .get("http://model.invalid/v1/models")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "second"
    );
    first_worker.join().unwrap();
    second_worker.join().unwrap();
}

#[tokio::test]
async fn failed_connection_tries_the_next_pac_proxy_before_sending_post() {
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let closed_port = closed.local_addr().unwrap().port();
    drop(closed);
    let (backup, worker) =
        server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
    let client = client(move |_| Ok(vec![proxy(closed_port), proxy(backup.port())]));
    let response = client
        .request(reqwest::Method::POST, "http://model.invalid/v1/responses")
        .body("billable request")
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "ok");
    assert!(worker.join().unwrap().starts_with("POST "));
}

#[tokio::test]
async fn pac_direct_fallback_is_only_used_when_explicitly_listed() {
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let closed_port = closed.local_addr().unwrap().port();
    drop(closed);
    let (direct, worker) =
        server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
    let client = client(move |_| Ok(vec![proxy(closed_port), Route::Direct]));
    assert!(
        client
            .get(format!("http://{direct}/models"))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    worker.join().unwrap();
    let client = self::client(move |_| Ok(vec![proxy(closed_port)]));
    assert!(
        client
            .get(format!("http://{direct}/models"))
            .send()
            .await
            .unwrap_err()
            .is_connect()
    );
}

#[tokio::test]
async fn never_fails_over_after_response_or_after_request_bytes_were_sent() {
    for response in [
        "HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        "HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        "",
    ] {
        let (primary, worker) = server(response);
        let backup = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = backup.local_addr().unwrap().port();
        let client = client(move |_| Ok(vec![proxy(primary.port()), proxy(port)]));
        let result = client
            .request(reqwest::Method::POST, "http://model.invalid/v1/responses")
            .body("only once")
            .send()
            .await;
        if response.is_empty() {
            assert!(!result.unwrap_err().is_connect());
        } else {
            assert!(!result.unwrap().status().is_success());
        }
        assert!(worker.join().unwrap().starts_with("POST "));
        backup.set_nonblocking(true).unwrap();
        assert!(
            matches!(backup.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
    }
}

#[tokio::test]
async fn https_connect_authentication_failure_does_not_try_a_backup_route() {
    let (primary, worker) = server(
        "HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );
    let backup = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = backup.local_addr().unwrap().port();
    let client = client(move |_| Ok(vec![proxy(primary.port()), proxy(port)]));
    let error = client
        .get("https://model.invalid/v1/models")
        .send()
        .await
        .unwrap_err();
    assert!(matches!(&error, Error::Transport { error, .. } if is_proxy_auth_error(error)));
    assert!(
        worker
            .join()
            .unwrap()
            .starts_with("CONNECT model.invalid:443 ")
    );
    backup.set_nonblocking(true).unwrap();
    assert!(
        matches!(backup.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

#[tokio::test]
async fn redirects_recheck_full_url_policy_and_remove_cross_origin_secrets() {
    let (first, first_worker) = server(
        "HTTP/1.1 302 Found\r\nLocation: http://next.invalid/new/path?x=1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );
    let (second, second_worker) =
        server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let capture = seen.clone();
    let client = client(move |url| {
        capture.lock().unwrap().push(url.as_str().to_owned());
        Ok(vec![proxy(if url.host_str() == Some("first.invalid") {
            first.port()
        } else {
            second.port()
        })])
    });
    client
        .get("http://first.invalid/original/path")
        .header("authorization", "Bearer secret")
        .header("x-api-key", "secret")
        .header("x-goog-api-key", "secret")
        .header("cookie", "secret")
        .send()
        .await
        .unwrap();
    assert!(first_worker.join().unwrap().contains("secret"));
    let next = second_worker.join().unwrap();
    assert!(!next.contains("secret"));
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            "http://first.invalid/original/path",
            "http://next.invalid/new/path?x=1"
        ]
    );
}

#[tokio::test]
async fn proxy_resolution_errors_are_reported_without_falling_back_to_direct() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = client(|_| bail!("PAC parse failure"));
    let error = client
        .get(format!("http://{}/models", listener.local_addr().unwrap()))
        .send()
        .await
        .unwrap_err();
    assert!(error.to_string().contains("PAC parse failure"));
    listener.set_nonblocking(true).unwrap();
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

#[tokio::test]
async fn overall_timeout_includes_proxy_resolution() {
    let client = client(|_| {
        std::thread::sleep(Duration::from_millis(150));
        Ok(vec![Route::Direct])
    });
    // Construct another client because configuration must precede sharing.
    let client = Client::with_resolver(
        reqwest::Client::builder,
        move |url| (client.inner.resolver)(url),
        0,
    )
    .unwrap()
    .total_timeout(Duration::from_millis(20));
    assert!(
        client
            .get("http://example.invalid/")
            .send()
            .await
            .unwrap_err()
            .is_timeout()
    );
}

#[tokio::test]
async fn socks5h_resolves_the_target_at_the_proxy_and_supports_authentication() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let worker = std::thread::spawn(move || {
        let mut stream = accept(&listener);
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut header = [0u8; 2];
        stream.read_exact(&mut header).unwrap();
        assert_eq!(header[0], 5);
        let mut methods = vec![0u8; header[1] as usize];
        stream.read_exact(&mut methods).unwrap();
        assert!(methods.contains(&2));
        stream.write_all(&[5, 2]).unwrap();
        stream.read_exact(&mut header).unwrap();
        assert_eq!(header[0], 1);
        let mut username = vec![0u8; header[1] as usize];
        stream.read_exact(&mut username).unwrap();
        assert_eq!(username, b"audit");
        let mut length = [0];
        stream.read_exact(&mut length).unwrap();
        let mut password = vec![0; length[0] as usize];
        stream.read_exact(&mut password).unwrap();
        assert_eq!(password, b"password");
        stream.write_all(&[1, 0]).unwrap();
        let mut connect = [0u8; 4];
        stream.read_exact(&mut connect).unwrap();
        assert_eq!(connect, [5, 1, 0, 3]);
        stream.read_exact(&mut length).unwrap();
        let mut domain = vec![0u8; length[0] as usize];
        stream.read_exact(&mut domain).unwrap();
        assert_eq!(domain, b"unresolvable.invalid");
        let mut port = [0u8; 2];
        stream.read_exact(&mut port).unwrap();
        assert_eq!(u16::from_be_bytes(port), 80);
        stream
            .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 80])
            .unwrap();
        let headers = read_headers(&stream);
        assert!(headers.starts_with("GET /models "));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .unwrap();
    });
    let client = client(move |_| {
        Ok(vec![Route::Proxy(format!(
            "socks5h://audit:password@127.0.0.1:{port}"
        ))])
    });
    assert_eq!(
        client
            .get("http://unresolvable.invalid/models")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "ok"
    );
    worker.join().unwrap();
}

#[tokio::test]
async fn discovery_retry_does_not_retry_policy_errors_or_http_failures() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let client = client(move |_| {
        counter.fetch_add(1, Ordering::Relaxed);
        bail!("PAC invalid")
    });
    assert!(
        send_with_network_retry(client.get("http://example.invalid"))
            .await
            .is_err()
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    let (address, worker) =
        server("HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let client = self::client(|_| Ok(vec![Route::Direct]));
    assert_eq!(
        send_with_network_retry(client.get(format!("http://{address}")))
            .await
            .unwrap()
            .status(),
        401
    );
    worker.join().unwrap();
}

#[cfg(windows)]
#[test]
fn windows_pac_preserves_order_direct_fallback_and_full_url_paths() {
    let body = "function FindProxyForURL(url, host) { if (shExpMatch(url, '*/special*')) return 'PROXY 127.0.0.1:18080; PROXY 127.0.0.1:18081; DIRECT'; return 'DIRECT'; }";
    let (address, worker) = server(format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/x-ns-proxy-autoconfig\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    let settings = SystemSettings {
        pac_url: Some(format!("http://{address}/proxy.pac")),
        ..Default::default()
    };
    let routes =
        super::windows::resolve(&url("http://model.example/special?x=1"), &settings).unwrap();
    assert_eq!(routes, vec![proxy(18080), proxy(18081), Route::Direct]);
    worker.join().unwrap();
}
