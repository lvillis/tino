#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

fn tino_bin() -> &'static str {
    env!("CARGO_BIN_EXE_tino")
}

#[test]
fn license_flag_prints_license() {
    let output = Command::new(tino_bin())
        .arg("--license")
        .output()
        .expect("failed to run tino --license");

    assert!(output.status.success(), "license flag exited with failure");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("MIT License"),
        "license text missing MIT header\n{}",
        stdout
    );
}

#[test]
fn missing_command_exits_with_error() {
    let status = Command::new(tino_bin())
        .status()
        .expect("failed to run tino without args");

    assert_eq!(
        status.code(),
        Some(1),
        "expected exit code 1 when CMD is missing"
    );
}

#[test]
fn remap_exit_zeroes_expected_codes() {
    let status = Command::new(tino_bin())
        .args(["-e", "3", "--", "sh", "-c", "exit 3"])
        .status()
        .expect("failed to run tino remap test");

    assert!(
        status.success(),
        "expected tino to map exit code 3 to success, got {:?}",
        status.code()
    );
}

#[test]
fn signal_forwarding_reaches_child() {
    use nix::{
        sys::signal::{Signal, kill},
        unistd::Pid,
    };
    let mut child = Command::new(tino_bin())
        .stdout(Stdio::piped())
        .args([
            "--",
            "sh",
            "-c",
            "trap 'exit 42' TERM; printf 'ready\\n'; while true; do sleep 1; done",
        ])
        .spawn()
        .expect("failed to spawn tino signal test");

    let mut stdout = BufReader::new(child.stdout.take().expect("signal test stdout"));
    let mut ready = String::new();
    stdout
        .read_line(&mut ready)
        .expect("read readiness marker for signal test");

    assert_eq!(ready.trim_end(), "ready", "unexpected readiness marker");
    drop(stdout);
    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).expect("failed to send SIGTERM");

    let status = child.wait().expect("failed to wait on tino signal test");
    assert_eq!(
        status.code(),
        Some(42),
        "expected child to receive forwarded SIGTERM"
    );
}

#[test]
fn warn_on_reap_emits_warning() {
    let output = Command::new(tino_bin())
        .args(["-w", "--", "sh", "-c", "(sleep 0.1 &) && exit 0"])
        .output()
        .expect("failed to run tino warning test");

    assert!(
        output.status.success(),
        "warn-on-reap scenario failed: {:?}",
        output.status.code()
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("reaped secondary PID"),
        "expected warning about secondary PID\n{stderr}"
    );
}

#[test]
fn pgroup_kill_escalates_after_grace() {
    use nix::{
        sys::signal::{Signal, kill},
        unistd::Pid,
    };
    let mut child = Command::new(tino_bin())
        .stdout(Stdio::piped())
        .args([
            "-g",
            "-t",
            "50",
            "--",
            "sh",
            "-c",
            "trap '' TERM; printf 'ready\\n'; while true; do sleep 1; done",
        ])
        .spawn()
        .expect("failed to spawn tino pgroup test");

    let mut stdout = BufReader::new(child.stdout.take().expect("pgroup test stdout"));
    let mut ready = String::new();
    stdout
        .read_line(&mut ready)
        .expect("read readiness marker for pgroup test");
    assert_eq!(ready.trim_end(), "ready", "unexpected readiness marker");
    drop(stdout);
    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).expect("failed to send SIGTERM");

    let status = child.wait().expect("failed to wait on tino pgroup test");
    assert_eq!(
        status.code(),
        Some(137),
        "expected escalation to SIGKILL reflected in exit code"
    );
}
