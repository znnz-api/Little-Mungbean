use super::*;
use serde_json::{Value, json};

#[test]
fn helper_keeps_gateway_credentials_out_of_arguments_and_forwards_unsets() {
    let args = vec![
        "--remote-debugging-port=54321".into(),
        "space ' 中文 $()".into(),
    ];
    let helper = desktop_launch_helper_command(
        Path::new("/unused/helper"),
        Path::new("/unused/Client.app/Contents/MacOS/Client"),
        &args,
        |command| {
            configure_codex_desktop_environment(
                command,
                Path::new("/isolated/home 中文"),
                Some(("http://127.0.0.1:54321/", "test-token-not-an-argument")),
            );
        },
    );
    let command = helper.as_std();
    let arguments = command
        .get_args()
        .map(|s| s.to_str().unwrap())
        .collect::<Vec<_>>();
    assert!(
        arguments
            .windows(2)
            .any(|a| a == ["--target-arg", args[0].as_str()])
    );
    assert!(
        arguments
            .windows(2)
            .any(|a| a == ["--target-arg", args[1].as_str()])
    );
    assert!(
        arguments
            .windows(2)
            .any(|a| a == ["--unset-env", "CODEX_APP_SERVER_CHATGPT_BASE_URL"])
    );
    assert!(!arguments.iter().any(|s| s.contains("test-token")));
    let environment = command
        .get_envs()
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(
        environment[std::ffi::OsStr::new("OPENAI_API_KEY")],
        Some(std::ffi::OsStr::new("test-token-not-an-argument"))
    );
    assert_eq!(
        environment[std::ffi::OsStr::new("CODEX_HOME")],
        Some(std::ffi::OsStr::new("/isolated/home 中文"))
    );
}

#[test]
fn launch_environment_preserves_values_and_blocks_launchd_route_inheritance() {
    let env = desktop_launch_environment(
        [
            ("OPENAI_API_KEY", "fake-token"),
            ("CODEX_HOME", "/home 中文"),
            ("REMOVE_ME", "foreign"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into())),
        &[
            "REMOVE_ME".into(),
            "CODEX_APP_SERVER_CHATGPT_BASE_URL".into(),
        ],
    )
    .unwrap();
    assert_eq!(env["OPENAI_API_KEY"], "fake-token");
    assert_eq!(env["CODEX_HOME"], "/home 中文");
    assert_eq!(env["REMOVE_ME"], "");
    assert_eq!(env["CODEX_APP_SERVER_CHATGPT_BASE_URL"], "");
}

#[test]
fn launch_rejects_non_bundle_executables() {
    assert!(run_detached_desktop(Path::new("/bin/sleep"), &[], &[]).is_err());
}

#[test]
fn launch_services_separates_responsibility_and_preserves_arguments_and_environment() {
    const CHILD: &str = "LITTLE_MUNGBEAN_LAUNCH_SERVICES_TEST";
    if std::env::var_os(CHILD).is_none() {
        let root = std::env::temp_dir().join(format!("mungbean-launch-{}", rand::random::<u64>()));
        fs::create_dir_all(&root).unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "platform::macos_impl::desktop_launch_tests::launch_services_separates_responsibility_and_preserves_arguments_and_environment", "--nocapture"])
            .env(CHILD, &root)
            .env("LITTLE_MUNGBEAN_LAUNCH_FIXTURE", &root)
            .env("CODEX_HOME", root.join("home 中文"))
            .env("OPENAI_BASE_URL", "http://127.0.0.1:54321")
            .env("OPENAI_API_KEY", "isolated-test-token")
            .env("LITTLE_MUNGBEAN_TEST_CLEAR", "foreign-route")
            .output().unwrap();
        let _ = fs::write(root.join("stop"), b"");
        let _ = fs::remove_dir_all(&root);
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let root = PathBuf::from(std::env::var_os(CHILD).unwrap())
        .canonicalize()
        .unwrap();
    let bundle = root.join("Isolated launch 中文.app");
    let executable = bundle.join("Contents/MacOS/Probe");
    fs::create_dir_all(executable.parent().unwrap()).unwrap();
    // A real AppKit check-in is required for LaunchServices to report readiness.
    // The fixture registers without opening windows or using a real client.
    let source = root.join("probe.m");
    fs::write(&source, include_str!("macos_launch_fixture.m")).unwrap();
    let compiled = Command::new("/usr/bin/clang")
        .arg(&source)
        .args(["-framework", "AppKit", "-o"])
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let identifier = format!(
        "local.little-mungbean.launch-test-{}",
        rand::random::<u64>()
    );
    fs::write(
        bundle.join("Contents/Info.plist"),
        format!(
            r#"<?xml version="1.0"?><plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>{identifier}</string>
<key>CFBundleExecutable</key><string>Probe</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>LSBackgroundOnly</key><true/>
</dict></plist>"#
        ),
    )
    .unwrap();
    let arguments = vec![
        "--remote-debugging-port=54321".into(),
        "space ' 中文 $()".into(),
    ];
    let pid = run_detached_desktop(
        &executable,
        &arguments,
        &["LITTLE_MUNGBEAN_TEST_CLEAR".into()],
    )
    .unwrap();
    struct StopProbe(PathBuf, PathBuf, u32);
    impl Drop for StopProbe {
        fn drop(&mut self) {
            let _ = fs::write(self.0.join("stop"), b"");
            let deadline = Instant::now() + Duration::from_secs(3);
            while process_matches_image(self.2, &self.1) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            if process_matches_image(self.2, &self.1) {
                unsafe {
                    libc::kill(self.2 as i32, libc::SIGTERM);
                }
            }
        }
    }
    let _stop = StopProbe(root.clone(), executable, pid);
    let deadline = Instant::now() + Duration::from_secs(15);
    let path = root.join("launched.json");
    while !path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let probe: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(probe["pid"], pid);
    assert_eq!(
        probe["arguments"].as_array().unwrap()[1..],
        arguments.iter().map(|s| json!(s)).collect::<Vec<_>>()
    );
    assert_eq!(probe["CODEX_HOME"], std::env::var("CODEX_HOME").unwrap());
    assert_eq!(probe["OPENAI_BASE_URL"], "http://127.0.0.1:54321");
    assert_eq!(probe["OPENAI_API_KEY"], "isolated-test-token");
    assert_eq!(probe["CLEARED_ROUTE"], "");
    // Diagnostic-only private query; production uses public NSWorkspace APIs.
    unsafe {
        let symbol = libc::dlsym(
            libc::RTLD_DEFAULT,
            c"responsibility_get_pid_responsible_for_pid".as_ptr(),
        );
        assert!(!symbol.is_null());
        let responsible: unsafe extern "C" fn(i32) -> i32 = std::mem::transmute(symbol);
        assert_eq!(
            responsible(pid as i32),
            pid as i32,
            "the app must own its resource responsibility"
        );
        assert_ne!(
            responsible(pid as i32),
            responsible(std::process::id() as i32)
        );
    }
}
