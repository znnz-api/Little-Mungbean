//! Persist only terminals launched by this app; a new GUI process can reconnect
//! them after validating the live process, without guessing ownership from names.
use super::*;

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedTerminal {
    version: u8,
    session: TerminalRestartSession,
}

fn state_path(root: &Path, target: ClientTarget) -> Result<PathBuf> {
    if !matches!(target, ClientTarget::CodexCli | ClientTarget::ClaudeCode) {
        bail!("Only CLI terminal sessions can be saved");
    }
    Ok(root.join(format!("managed-{}.json", target.id())))
}

impl TerminalRestartSession {
    fn state_root() -> Result<PathBuf> {
        Ok(crate::platform_paths::data_root()?.join("terminal-sessions"))
    }

    pub fn recover(target: ClientTarget) -> Result<Option<Self>> {
        Self::recover_at(&Self::state_root()?, target)
    }

    pub(super) fn save(&self) -> Result<()> {
        self.save_at(&Self::state_root()?)
    }

    fn save_at(&self, root: &Path) -> Result<()> {
        self.validate_record(self.target)?;
        if !self.matches_live_process()? {
            bail!("The terminal process exited before its identity could be saved");
        }
        crate::backup::atomic_write(
            &state_path(root, self.target)?,
            &serde_json::to_vec(&SavedTerminal {
                version: 1,
                session: self.clone(),
            })?,
        )
        .context("Unable to save the managed terminal identity")
    }

    fn recover_at(root: &Path, target: ClientTarget) -> Result<Option<Self>> {
        let path = state_path(root, target)?;
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).context("Unable to read the managed terminal identity");
            }
        };
        let Ok(saved) = serde_json::from_slice::<SavedTerminal>(&bytes) else {
            return Ok(None);
        };
        if saved.version != 1 || saved.session.validate_record(target).is_err() {
            return Ok(None);
        }
        if !saved.session.matches_live_process()? {
            // A dead/reused PID or changed executable/TTY cannot authorize a quit.
            saved.session.forget_saved_at(root)?;
            return Ok(None);
        }
        Ok(Some(saved.session))
    }

    fn validate_record(&self, target: ClientTarget) -> Result<()> {
        state_path(Path::new("."), target)?;
        if self.target != target
            || self.pid == 0
            || self.started.is_empty()
            || !self.image.is_absolute()
        {
            bail!("Invalid managed terminal process identity");
        }
        OwnedTerminalWindow::from_launch(&format!("{}\n{}\n", self.window.id, self.window.tty))?;
        Ok(())
    }

    pub(super) fn matches_live_process(&self) -> Result<bool> {
        if identity(self.pid).ok().as_ref() != Some(&self.started)
            || !process_matches_image(self.pid, &self.image)
        {
            return Ok(false);
        }
        let output = Command::new("/bin/ps")
            .args(["-p", &self.pid.to_string(), "-o", "tty="])
            .output()?;
        Ok(output.status.success()
            && String::from_utf8_lossy(&output.stdout).trim()
                == self.window.tty.trim_start_matches("/dev/"))
    }

    pub(super) fn forget_saved(&self) -> Result<()> {
        self.forget_saved_at(&Self::state_root()?)
    }

    fn forget_saved_at(&self, root: &Path) -> Result<()> {
        let path = state_path(root, self.target)?;
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        // An old monitor must never delete a newer launch's ownership record.
        if serde_json::from_slice::<SavedTerminal>(&bytes)
            .is_ok_and(|saved| saved.version == 1 && saved.session == *self)
        {
            fs::remove_file(&path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "macos_terminal_tests.rs"]
mod tests;
