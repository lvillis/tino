#![cfg(unix)]

use std::process::Command;

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
