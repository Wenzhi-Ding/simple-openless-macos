//! Temporary system-output mute while recording.
pub struct AudioMuteGuard {
    inner: Option<platform::PlatformMuteGuard>,
}
impl AudioMuteGuard {
    pub fn activate() -> Result<Self, String> {
        Ok(Self {
            inner: Some(platform::activate()?),
        })
    }
}
impl Drop for AudioMuteGuard {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            inner.restore();
        }
    }
}
mod platform {
    use std::process::Command;
    pub struct PlatformMuteGuard {
        was_muted: bool,
    }
    pub fn activate() -> Result<PlatformMuteGuard, String> {
        let was_muted = output_muted()?;
        if !was_muted {
            set_output_muted(true)?;
        }
        Ok(PlatformMuteGuard { was_muted })
    }
    impl PlatformMuteGuard {
        pub fn restore(self) {
            if let Err(err) = set_output_muted(self.was_muted) {
                log::warn!("[audio-mute] restore output mute failed: {err}");
            }
        }
    }
    fn output_muted() -> Result<bool, String> {
        let output = Command::new("osascript")
            .args(["-e", "output muted of (get volume settings)"])
            .output()
            .map_err(|e| format!("query output mute failed: {e}"))?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim() == "true")
    }
    fn set_output_muted(muted: bool) -> Result<(), String> {
        let script = if muted {
            "set volume output muted true"
        } else {
            "set volume output muted false"
        };
        let output = Command::new("osascript")
            .args(["-e", script])
            .output()
            .map_err(|e| format!("set output mute failed: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    }
}
