#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

fn tino_bin() -> &'static str {
    env!("CARGO_BIN_EXE_tino")
}

fn landlock_available() -> bool {
    let output = Command::new(tino_bin())
        .args(["--write-warn-only", "--", "sh", "-c", "exit 0"])
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
    let available = !stderr.contains("write restriction unavailable; continuing");
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
fn explain_reports_effective_configuration() {
    let root = unique_temp_dir("tino-explain");
    let allowed_dir = root.join("allowed");
    std::fs::create_dir_all(&allowed_dir).expect("create allowed dir");
    let canonical_allowed = allowed_dir
        .canonicalize()
        .expect("canonicalize allowed dir");

    let output = Command::new(tino_bin())
        .args([
            "--expand-env",
            "--write-restrict",
            "--write-allow",
            allowed_dir.to_str().expect("allowed dir utf-8"),
            "--explain",
            "--",
            "/bin/echo",
            "-port=${SERVICE_PORT:-8900}",
        ])
        .env_remove("SERVICE_PORT")
        .env("TINI_SUBREAPER", "1")
        .output()
        .expect("failed to run tino explain test");

    assert!(
        output.status.success(),
        "explain scenario failed: {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("mode: explain"),
        "missing explain header\n{stdout}"
    );
    assert!(
        stdout.contains("subreaper: true"),
        "missing effective subreaper\n{stdout}"
    );
    assert!(
        stdout.contains("subreaper.source: env:TINI_SUBREAPER"),
        "missing subreaper source\n{stdout}"
    );
    assert!(
        stdout.contains(r#"command.effective: ["/bin/echo", "-port=8900"]"#),
        "missing effective command\n{stdout}"
    );
    assert!(
        stdout.contains("write_restrict.enabled: true"),
        "missing write restriction status\n{stdout}"
    );
    assert!(
        stdout.contains("write_restrict.dev_writable: true"),
        "missing write restriction /dev behavior\n{stdout}"
    );
    assert!(
        stdout.contains(&canonical_allowed.display().to_string()),
        "missing canonical allowlist path\n{stdout}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn explain_does_not_execute_child() {
    let root = unique_temp_dir("tino-explain-noexec");
    std::fs::create_dir_all(&root).expect("create explain root");
    let marker = root.join("marker");

    let output = Command::new(tino_bin())
        .args(["--explain", "--", "sh", "-c", r#"touch "$MARKER""#])
        .env("MARKER", &marker)
        .output()
        .expect("failed to run tino explain noexec test");

    assert!(
        output.status.success(),
        "explain noexec scenario failed: {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !marker.exists(),
        "expected explain mode to avoid executing the child"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn exec_failure_reports_missing_binary_reason() {
    let output = Command::new(tino_bin())
        .args(["--", "/definitely/missing/tino-test-binary"])
        .output()
        .expect("failed to run tino missing-binary test");

    assert!(
        !output.status.success(),
        "expected missing binary execution to fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("file not found; check the path or PATH lookup"),
        "expected friendly ENOENT hint\n{stderr}"
    );
}

#[test]
fn exec_failure_reports_non_executable_reason() {
    use std::os::unix::fs::PermissionsExt;

    let root = unique_temp_dir("tino-exec-failure");
    std::fs::create_dir_all(&root).expect("create exec failure root");
    let script = root.join("non-executable.sh");
    std::fs::write(&script, "#!/bin/sh\necho should-not-run\n").expect("write non executable file");
    let mut perms = std::fs::metadata(&script)
        .expect("stat non executable file")
        .permissions();
    perms.set_mode(0o644);
    std::fs::set_permissions(&script, perms).expect("chmod non executable file");

    let output = Command::new(tino_bin())
        .arg("--")
        .arg(&script)
        .output()
        .expect("failed to run tino non-executable test");

    assert!(
        !output.status.success(),
        "expected non-executable child to fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("permission denied or file is not executable"),
        "expected friendly EACCES hint\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&root);
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
            "--write-restrict",
            "--write-allow",
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
            "--write-restrict",
            "--write-allow",
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
            "--write-restrict",
            "--write-allow-file",
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
