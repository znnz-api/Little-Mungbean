use super::*;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::{fs::PermissionsExt, process::CommandExt};

struct Fixture {
    root: PathBuf,
    child: std::process::Child,
    _master: File,
    session: TerminalRestartSession,
}

impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("mungbean-terminal-{:016x}", rand::random::<u64>()));
        fs::create_dir_all(&root).unwrap();
        let (master, slave, tty) = unsafe {
            let (mut master, mut slave) = (-1, -1);
            let mut name = [0 as libc::c_char; 256];
            assert_eq!(
                libc::openpty(
                    &mut master,
                    &mut slave,
                    name.as_mut_ptr(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut()
                ),
                0
            );
            let tty = std::ffi::CStr::from_ptr(name.as_ptr())
                .to_str()
                .unwrap()
                .to_owned();
            (File::from_raw_fd(master), File::from_raw_fd(slave), tty)
        };
        let mut command = Command::new("/bin/sleep");
        command
            .arg("60")
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave.try_clone().unwrap());
        unsafe {
            libc::fcntl(master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(slave.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
            command.pre_exec(|| {
                if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY.into(), 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !process_matches_image(child.id(), Path::new("/bin/sleep")) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        let session = TerminalRestartSession {
            pid: child.id(),
            started: identity(child.id()).unwrap(),
            target: ClientTarget::CodexCli,
            window: OwnedTerminalWindow { id: 4242, tty },
            image: PathBuf::from(process_image_path(child.id()).unwrap()),
        };
        assert!(session.matches_live_process().unwrap());
        Self {
            root,
            child,
            _master: master,
            session,
        }
    }

    fn write_record(&self, session: &TerminalRestartSession, version: u8) {
        crate::backup::atomic_write(
            &state_path(&self.root, ClientTarget::CodexCli).unwrap(),
            &serde_json::to_vec(&SavedTerminal {
                version,
                session: session.clone(),
            })
            .unwrap(),
        )
        .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn recovery_survives_a_complete_launcher_process_restart() {
    const STEP: &str = "MUNGBEAN_TERMINAL_RECOVERY_STEP";
    if let Ok(step) = std::env::var(STEP) {
        let root = PathBuf::from(std::env::var_os("MUNGBEAN_TERMINAL_TEST_ROOT").unwrap());
        let expected: TerminalRestartSession =
            serde_json::from_str(&std::env::var("MUNGBEAN_TERMINAL_TEST_SESSION").unwrap())
                .unwrap();
        match step.as_str() {
            "save" => expected.save_at(&root).unwrap(),
            "recover" => assert_eq!(
                TerminalRestartSession::recover_at(&root, ClientTarget::CodexCli).unwrap(),
                Some(expected)
            ),
            "forget" => expected.forget_saved_at(&root).unwrap(),
            _ => panic!("unexpected test step"),
        }
        return;
    }
    let fixture = Fixture::new();
    for step in ["save", "recover", "forget"] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "platform::macos_impl::terminal_state::tests::recovery_survives_a_complete_launcher_process_restart", "--nocapture"])
            .env(STEP, step).env("MUNGBEAN_TERMINAL_TEST_ROOT", &fixture.root)
            .env("MUNGBEAN_TERMINAL_TEST_SESSION", serde_json::to_string(&fixture.session).unwrap())
            .output().unwrap();
        assert!(
            output.status.success(),
            "{step}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if step == "save" {
            let path = state_path(&fixture.root, ClientTarget::CodexCli).unwrap();
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    assert!(
        TerminalRestartSession::recover_at(&fixture.root, ClientTarget::CodexCli)
            .unwrap()
            .is_none()
    );
}

#[test]
fn recovery_rejects_reused_pid_changed_executable_and_changed_tty() {
    let mut fixture = Fixture::new();
    for field in ["started", "image", "tty"] {
        let mut stale = fixture.session.clone();
        match field {
            "started" => stale.started = "previous process at this PID".into(),
            "image" => stale.image = PathBuf::from("/bin/cat"),
            "tty" => stale.window.tty = "/dev/ttysINVALID".into(),
            _ => unreachable!(),
        }
        fixture.write_record(&stale, 1);
        assert!(
            TerminalRestartSession::recover_at(&fixture.root, ClientTarget::CodexCli)
                .unwrap()
                .is_none()
        );
        assert!(
            !state_path(&fixture.root, ClientTarget::CodexCli)
                .unwrap()
                .exists()
        );
        assert!(
            fixture.child.try_wait().unwrap().is_none(),
            "a stale record must not terminate a process"
        );
    }
    fixture.session.save_at(&fixture.root).unwrap();
    fixture.child.kill().unwrap();
    fixture.child.wait().unwrap();
    assert!(
        TerminalRestartSession::recover_at(&fixture.root, ClientTarget::CodexCli)
            .unwrap()
            .is_none()
    );
}

#[test]
fn corrupt_or_mismatched_records_cannot_authorize_terminal_management() {
    let mut fixture = Fixture::new();
    let path = state_path(&fixture.root, ClientTarget::CodexCli).unwrap();
    fs::write(&path, "broken").unwrap();
    assert!(
        TerminalRestartSession::recover_at(&fixture.root, ClientTarget::CodexCli)
            .unwrap()
            .is_none()
    );
    fixture.write_record(&fixture.session, 2);
    assert!(
        TerminalRestartSession::recover_at(&fixture.root, ClientTarget::CodexCli)
            .unwrap()
            .is_none()
    );
    let mut wrong_target = fixture.session.clone();
    wrong_target.target = ClientTarget::ClaudeCode;
    fixture.write_record(&wrong_target, 1);
    assert!(
        TerminalRestartSession::recover_at(&fixture.root, ClientTarget::CodexCli)
            .unwrap()
            .is_none()
    );
    assert!(TerminalRestartSession::recover_at(&fixture.root, ClientTarget::CodexDesktop).is_err());
    assert!(fixture.child.try_wait().unwrap().is_none());
}

#[test]
fn old_session_cleanup_keeps_a_newer_launch_record() {
    let fixture = Fixture::new();
    let mut newer = fixture.session.clone();
    newer.window.id += 1;
    fixture.write_record(&newer, 1);
    fixture.session.forget_saved_at(&fixture.root).unwrap();
    assert_eq!(
        TerminalRestartSession::recover_at(&fixture.root, ClientTarget::CodexCli).unwrap(),
        Some(newer)
    );
}

#[test]
fn recovered_session_protects_unrelated_clients_and_accepts_its_descendants() {
    let fixture = Fixture::new();
    fixture.session.save_at(&fixture.root).unwrap();
    let restored = TerminalRestartSession::recover_at(&fixture.root, ClientTarget::CodexCli)
        .unwrap()
        .unwrap();
    let mut processes = vec![
        Process {
            pid: restored.pid,
            parent: 1,
            target: None,
        },
        Process {
            pid: restored.pid + 1,
            parent: restored.pid,
            target: Some(ClientTarget::CodexCli),
        },
    ];
    assert!(restored.verify_client_processes(&processes).is_ok());
    processes.push(Process {
        pid: restored.pid + 2,
        parent: 1,
        target: Some(ClientTarget::CodexCli),
    });
    assert!(restored.verify_client_processes(&processes).is_err());
    processes[2].target = Some(ClientTarget::ClaudeCode);
    assert!(restored.verify_client_processes(&processes).is_ok());
}
