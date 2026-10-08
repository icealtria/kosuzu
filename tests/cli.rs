//! CLI smoke test. help/bad-args/quit finish locally in `main()`, before GApplication,
//! so they run offline with no window; single-instance forwarding is `#[ignore]`d.

use std::process::Command;
use std::time::{Duration, Instant};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_kosuzu"))
}

#[test]
fn help_flag_prints_usage_locally() {
    let out = bin().arg("--help").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Selection/screenshot"), "help text: {stdout}");
    assert!(stdout.contains("--screenshot-ocr"), "help text: {stdout}");
}

#[test]
fn unknown_flag_fails_with_usage_error() {
    let out = bin().arg("--bogus").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--bogus"), "error text: {stderr}");
}

#[test]
fn quit_succeeds_without_instance() {
    let out = bin().arg("--quit").output().unwrap();
    assert!(out.status.success());
}

/// wait for exit with a timeout floor
fn wait_exit(child: &mut std::process::Child, seconds: u64) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("process did not exit within {seconds}s");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// real-window smoke: bare launch -> remote --text forwarded to primary -> --quit.
#[test]
#[ignore = "pops a real window and does network translation; run manually"]
fn single_instance_forwards_and_quit_exits() {
    let mut primary = bin().spawn().unwrap();
    std::thread::sleep(Duration::from_millis(1500)); // wait for the primary to claim its D-Bus name

    let forwarded = bin().args(["--text", "hello world"]).output().unwrap();
    assert!(
        forwarded.status.success(),
        "remote invocation should return immediately: {}",
        String::from_utf8_lossy(&forwarded.stderr)
    );

    let quit = bin().arg("--quit").output().unwrap();
    assert!(quit.status.success());
    assert!(wait_exit(&mut primary, 10).success());
}

/// real-window smoke: --image runs the OCR flow without crashing, --quit cleans up.
#[test]
#[ignore = "pops a real window and does network translation; run manually"]
fn image_action_runs_in_window_then_quits() {
    let fixture = format!("{}/fixtures/shot.png", env!("CARGO_MANIFEST_DIR"));
    let mut primary = bin().args(["--image", &fixture]).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(4000)); // OCR (tiny model) + possible auto-translate

    assert!(
        primary.try_wait().unwrap().is_none(),
        "OCR/translate flow must not exit the process"
    );

    let quit = bin().arg("--quit").output().unwrap();
    assert!(quit.status.success());
    assert!(wait_exit(&mut primary, 10).success());
}
