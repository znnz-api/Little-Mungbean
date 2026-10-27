use super::*;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub(super) struct Server {
    address: SocketAddr,
    response: Arc<Mutex<(u16, String)>>,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    pub(super) fn new(body: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let response = Arc::new(Mutex::new((200, body.to_owned())));
        let requests = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (reply, count, stopping) = (response.clone(), requests.clone(), stop.clone());
        let worker = std::thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("PAC test listener: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let n = match stream.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    // macOS may probe HTTPS before falling back to the HTTP
                    // PAC URL. Reject that probe and keep accepting requests.
                    if request.is_empty() && buffer[0] == 0x16 {
                        let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        break;
                    }
                    request.extend_from_slice(&buffer[..n]);
                    if request.windows(4).any(|w| w == b"\r\n\r\n") {
                        count.fetch_add(1, Ordering::Relaxed);
                        let (status, body) = reply.lock().unwrap().clone();
                        let _ = write!(
                            stream,
                            "HTTP/1.1 {status} Test\r\nContent-Type: application/x-ns-proxy-autoconfig\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        break;
                    }
                    assert!(request.len() < 64 * 1024, "oversized test request");
                }
            }
        });
        Self {
            address,
            response,
            requests,
            stop,
            worker: Some(worker),
        }
    }

    pub(super) fn settings(&self) -> SystemSettings {
        SystemSettings {
            pac_url: Some(format!("http://{}/proxy.pac", self.address)),
            ..Default::default()
        }
    }

    pub(super) fn set_response(&self, status: u16, body: &str) {
        *self.response.lock().unwrap() = (status, body.to_owned());
    }

    pub(super) fn requests(&self) -> usize {
        self.requests.load(Ordering::Relaxed)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}

fn url(value: &str) -> Url {
    Url::parse(value).unwrap()
}

#[test]
fn pac_download_preserves_proxy_order_and_explicit_direct_for_http_and_https() {
    let server = Server::new(
        "function FindProxyForURL(url, host) { return 'PROXY 127.0.0.1:18080; PROXY 127.0.0.1:18081; DIRECT'; }",
    );
    for target in [
        "http://model.example/v1/models",
        "https://model.example/v1/models",
    ] {
        assert_eq!(
            resolve(&url(target), &server.settings()).unwrap(),
            vec![
                Route::Proxy("http://127.0.0.1:18080".into()),
                Route::Proxy("http://127.0.0.1:18081".into()),
                Route::Direct,
            ]
        );
    }
}

#[test]
fn pac_domain_helpers_and_socks_work_with_native_engine() {
    let server = Server::new(
        "function FindProxyForURL(url, host) { if (dnsDomainIs(host, '.example')) return 'SOCKS 127.0.0.1:1080'; return 'DIRECT'; }",
    );
    assert_eq!(
        resolve(&url("https://model.example/"), &server.settings()).unwrap(),
        vec![Route::Proxy("socks5h://127.0.0.1:1080".into())]
    );
    assert_eq!(
        resolve(&url("https://other.invalid/"), &server.settings()).unwrap(),
        vec![Route::Direct]
    );
}

#[test]
fn invalid_script_and_empty_routes_return_errors() {
    let server = Server::new("function FindProxyForURL(url, host) { invalid javascript; }");
    let error = resolve(&url("https://model.example/"), &server.settings()).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("kCFErrorDomainCFNetwork"), "{message}");
    assert!(message.contains("code 308"), "{message}");
    server.set_response(200, "function FindProxyForURL(url, host) { return ''; }");
    assert!(resolve(&url("https://other.example/"), &server.settings()).is_err());
}

#[test]
fn download_errors_keep_native_codes_without_exposing_pac_credentials() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let settings = SystemSettings {
        pac_url: Some(format!(
            "http://audit:private-password@{address}/private-path.pac?token=private-token"
        )),
        ..Default::default()
    };
    let message = format!(
        "{:#}",
        resolve(&url("https://model.example/"), &settings).unwrap_err()
    );
    assert!(message.contains("code "), "{message}");
    for secret in [
        "audit",
        "private-password",
        "private-path",
        "private-token",
        &address.to_string(),
    ] {
        assert!(
            !message.contains(secret),
            "PAC information leaked: {message}"
        );
    }
}

#[test]
fn http_download_failure_is_not_silently_converted_to_direct() {
    let server = Server::new("unavailable");
    server.set_response(500, "unavailable");
    assert!(resolve(&url("https://model.example/"), &server.settings()).is_err());
}

#[tokio::test]
async fn native_pac_failover_drives_a_post_without_replaying_after_a_response() {
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let dead = closed.local_addr().unwrap().port();
    drop(closed);
    let backup = Server::new("ok");
    let script = Server::new(&format!(
        "function FindProxyForURL(url, host) {{ return 'PROXY 127.0.0.1:{dead}; PROXY {}; DIRECT'; }}",
        backup.address
    ));
    let settings = script.settings();
    let client = Client::with_resolver(
        || reqwest::Client::builder().connect_timeout(Duration::from_millis(350)),
        move |url| resolve(url, &settings),
        0,
    )
    .unwrap()
    .total_timeout(Duration::from_secs(5));
    let response = client
        .request(reqwest::Method::POST, "http://model.invalid/v1/responses")
        .body("billable request")
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "ok");
    assert_eq!(backup.requests(), 1);
}

#[tokio::test]
async fn native_https_connect_authentication_failure_never_uses_backup() {
    let primary = Server::new("");
    primary.set_response(407, "");
    let backup = Server::new("must not be used");
    let script = Server::new(&format!(
        "function FindProxyForURL(url, host) {{ return 'PROXY {}; PROXY {}; DIRECT'; }}",
        primary.address, backup.address
    ));
    let settings = script.settings();
    let client = Client::with_resolver(
        || reqwest::Client::builder().connect_timeout(Duration::from_millis(350)),
        move |url| resolve(url, &settings),
        0,
    )
    .unwrap()
    .total_timeout(Duration::from_secs(5));
    let error = client
        .get("https://model.invalid/v1/models")
        .send()
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Transport { error, .. } if is_proxy_auth_error(&error)));
    assert_eq!(primary.requests(), 1);
    assert_eq!(backup.requests(), 0);
}

#[test]
fn callback_never_copies_untrusted_error_description_into_logs() {
    unsafe extern "C" {
        fn CFErrorCreateWithUserInfoKeysAndValues(
            allocator: Ref,
            domain: Ref,
            code: Index,
            keys: *const Ref,
            values: *const Ref,
            count: Index,
        ) -> Ref;
        static kCFErrorLocalizedDescriptionKey: Ref;
    }
    let domain = cf_string("NSURLErrorDomain").unwrap();
    let description =
        cf_string("http://user:private-password@proxy/private.pac?token=private-token").unwrap();
    let error = Owned::new(unsafe {
        CFErrorCreateWithUserInfoKeysAndValues(
            std::ptr::null(),
            domain.0,
            -1004,
            &kCFErrorLocalizedDescriptionKey,
            &description.0,
            1,
        )
    })
    .unwrap();
    let message = pac_error(error.0).to_string();
    assert!(message.contains("NSURLErrorDomain, code -1004"));
    assert!(message.contains("Cannot connect to PAC server"));
    assert!(!message.contains("private"));
}
