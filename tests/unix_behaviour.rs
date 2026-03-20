#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

fn tino_bin() -> &'static str {
    env!("CARGO_BIN_EXE_tino")
}

fn landlock_available() -> bool {
    let output = Command::new(tino_bin())
        .args(["--landlock-warn-only", "--", "sh", "-c", "exit 0"])
        .output()
        .expect("failed to probe landlock availability");

    let require = std::env::var_os("TINO_TEST_REQUIRE_LANDLOCK").is_some();
    if !output.status.success() {
        if require {
            panic!(
                "landlock probe failed with exit status {:?}\nstderr:\n{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return false;
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let available = !stderr.contains("landlock unavailable; continuing");
    if require && !available {
        panic!("landlock required for CI but unavailable:\n{stderr}");
    }
    available
}

fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()))
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
fn expand_env_interpolates_child_arguments_without_shell() {
    let output = Command::new(tino_bin())
        .args([
            "--expand-env",
            "--",
            "/bin/echo",
            "-port=${SERVICE_PORT:-8900}",
            "${SERVICE_NAME}",
        ])
        .env_remove("SERVICE_PORT")
        .env("SERVICE_NAME", "collector")
        .output()
        .expect("failed to run tino expand-env test");

    assert!(
        output.status.success(),
        "expand-env scenario failed: {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "-port=8900 collector\n"
    );
}

#[test]
fn expand_env_leaves_unbraced_dollar_names_unchanged() {
    let output = Command::new(tino_bin())
        .args(["--expand-env", "--", "/bin/echo", "$SERVICE_PORT"])
        .env("SERVICE_PORT", "9000")
        .output()
        .expect("failed to run tino unbraced expand-env test");

    assert!(
        output.status.success(),
        "unbraced expand-env scenario failed: {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "$SERVICE_PORT\n");
}

#[test]
fn expand_env_reports_invalid_syntax() {
    let output = Command::new(tino_bin())
        .args(["--expand-env", "--", "/bin/echo", "${SERVICE_PORT"])
        .output()
        .expect("failed to run tino invalid expand-env test");

    assert!(
        !output.status.success(),
        "expected invalid expansion syntax to fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("missing closing '}'"),
        "expected missing-brace error\n{stderr}"
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

#[test]
fn landlock_allows_writes_within_allowlist() {
    if !landlock_available() {
        return;
    }

    let root = unique_temp_dir("tino-landlock-allow");
    let allowed_dir = root.join("allowed");
    std::fs::create_dir_all(&allowed_dir).expect("create allowed dir");

    let status = Command::new(tino_bin())
        .args([
            "--landlock",
            "--landlock-writable",
            allowed_dir.to_str().expect("allowed dir utf-8"),
            "--",
            "sh",
            "-c",
            r#"set -e; echo ok > "$ALLOWED/ok""#,
        ])
        .env("ALLOWED", &allowed_dir)
        .status()
        .expect("run tino landlock allow test");

    assert!(
        status.success(),
        "expected write within allowlist to succeed"
    );
    assert!(
        allowed_dir.join("ok").exists(),
        "expected allowlisted file to be created"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn landlock_denies_writes_outside_allowlist() {
    if !landlock_available() {
        return;
    }

    let root = unique_temp_dir("tino-landlock-deny");
    let allowed_dir = root.join("allowed");
    let outside_dir = root.join("outside");
    std::fs::create_dir_all(&allowed_dir).expect("create allowed dir");
    std::fs::create_dir_all(&outside_dir).expect("create outside dir");

    let status = Command::new(tino_bin())
        .args([
            "--landlock",
            "--landlock-writable",
            allowed_dir.to_str().expect("allowed dir utf-8"),
            "--",
            "sh",
            "-c",
            r#"set -e; echo ok > "$ALLOWED/ok"; echo denied > "$OUTSIDE/deny""#,
        ])
        .env("ALLOWED", &allowed_dir)
        .env("OUTSIDE", &outside_dir)
        .status()
        .expect("run tino landlock deny test");

    assert!(
        !status.success(),
        "expected write outside allowlist to fail, got {status:?}"
    );
    assert!(
        allowed_dir.join("ok").exists(),
        "expected allowlisted file to be created"
    );
    assert!(
        !outside_dir.join("deny").exists(),
        "expected file outside allowlist to be denied"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn landlock_profile_file_is_honored() {
    if !landlock_available() {
        return;
    }

    let root = unique_temp_dir("tino-landlock-profile");
    let allowed_dir = root.join("allowed");
    std::fs::create_dir_all(&allowed_dir).expect("create allowed dir");

    let profile = root.join("landlock.profile");
    std::fs::write(
        &profile,
        format!("# tino landlock allowlist\n{}\n\n", allowed_dir.display()),
    )
    .expect("write landlock profile");

    let status = Command::new(tino_bin())
        .args([
            "--landlock",
            "--landlock-profile",
            profile.to_str().expect("profile utf-8"),
            "--",
            "sh",
            "-c",
            r#"set -e; echo ok > "$ALLOWED/ok""#,
        ])
        .env("ALLOWED", &allowed_dir)
        .status()
        .expect("run tino landlock profile test");

    assert!(
        status.success(),
        "expected write within allowlist to succeed"
    );
    assert!(
        allowed_dir.join("ok").exists(),
        "expected allowlisted file to be created"
    );

    let _ = std::fs::remove_dir_all(&root);
}
