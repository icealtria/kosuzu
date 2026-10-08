//! Screenshot via grim+slurp only (grim+slurp required on PATH).

use std::fmt;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// cap per attempt; guards against compositor/portal hangs
const TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenshotResult {
    pub path: PathBuf,
    /// user already picked a region externally (slurp): skip the in-app crop window
    pub region_selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenshotError {
    /// user cancelled; does not trigger fallback
    Cancelled,
    Failed(String),
}

impl fmt::Display for ScreenshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => write!(f, "screenshot cancelled"),
            Self::Failed(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ScreenshotError {}

fn tmp_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    PathBuf::from(format!("/tmp/kosuzu-shot-{nanos:x}.png"))
}

pub fn take_screenshot(_backend: &str, _interactive: bool) -> Result<ScreenshotResult, ScreenshotError> {
    take_grim()
}

pub(crate) enum RunOutcome {
    /// (exit code, stdout, stderr)
    Exited(ExitStatus, String, String),
    TimedOut,
    SpawnError(std::io::Error),
}

/// run a child process with a timeout, capturing stdout/stderr; kill on timeout.
pub(crate) fn run_with_timeout(mut cmd: Command, timeout: Duration) -> RunOutcome {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return RunOutcome::SpawnError(e),
    };
    let stdout = child.stdout.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf);
            buf
        })
    });
    let stderr = child.stderr.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf);
            buf
        })
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return RunOutcome::TimedOut;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => break None,
        }
    };
    let Some(status) = status else {
        return RunOutcome::SpawnError(std::io::Error::other("wait failed"));
    };
    let out = stdout
        .and_then(|h| h.join().ok())
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let err = stderr
        .and_then(|h| h.join().ok())
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    RunOutcome::Exited(status, out, err)
}

fn take_grim() -> Result<ScreenshotResult, ScreenshotError> {
    let (status, out, _) = match run_with_timeout(Command::new("slurp"), TIMEOUT) {
        RunOutcome::Exited(st, o, e) => (st, o, e),
        RunOutcome::TimedOut => return Err(ScreenshotError::Cancelled),
        RunOutcome::SpawnError(e) => {
            return Err(ScreenshotError::Failed(format!("failed to spawn slurp: {e}")));
        }
    };
    let geom = out.lines().next().unwrap_or("").trim().to_string();
    if !status.success() || geom.is_empty() {
        return Err(ScreenshotError::Cancelled);
    }

    let path = tmp_path();
    match run_with_timeout(
        {
            let mut c = Command::new("grim");
            c.args(["-g", &geom]).arg(&path);
            c
        },
        TIMEOUT,
    ) {
        RunOutcome::Exited(status, _, _) if status.success() && path.is_file() => {
            Ok(ScreenshotResult { path, region_selected: true })
        }
        RunOutcome::Exited(..) => Err(ScreenshotError::Failed(
            "grim screenshot failed (compositor may not support screencopy)".into(),
        )),
        RunOutcome::TimedOut => Err(ScreenshotError::Failed("grim screenshot timed out".into())),
        RunOutcome::SpawnError(e) => Err(ScreenshotError::Failed(format!("failed to spawn grim: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// full chain needs manual region selection; run manually:
    /// `cargo test -- --ignored take_screenshot_live --nocapture`
    #[test]
    #[ignore = "interactive: needs manual region selection in slurp/niri UI"]
    fn take_screenshot_live() {
        let r = take_screenshot("auto", true).expect("complete a screenshot manually");
        assert!(r.path.is_file());
        println!("result: {:?} region_selected={}", r.path, r.region_selected);
    }
}
