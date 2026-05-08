#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn tino_bin() -> &'static str {
    env!("CARGO_BIN_EXE_tino")
}

fn tino_command() -> Command {
    let mut command = Command::new(tino_bin());
    command.arg("--no-config");
    command
}

fn landlock_available() -> bool {
    let output = tino_command()
        .args([
            "--write-warn-only",
            "--write-preset",
            "tmp",
            "--",
            "sh",
            "-c",
            "exit 0",
        ])
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
    let available = !stderr.contains("access restriction unavailable; continuing");
    if require && !available {
        panic!("landlock required for CI but unavailable:\n{stderr}");
    }
    available
}

fn landlock_tcp_available() -> bool {
    let output = tino_command()
        .args([
            "--write-warn-only",
            "--bind-tcp-allow",
            "1",
            "--",
            "sh",
            "-c",
            "exit 0",
        ])
        .output()
        .expect("failed to probe landlock TCP availability");

    let require = std::env::var_os("TINO_TEST_REQUIRE_LANDLOCK").is_some();
    if !output.status.success() {
        if require {
            panic!(
                "landlock TCP probe failed with exit status {:?}\nstderr:\n{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return false;
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let available = !stderr.contains("access restriction unavailable; continuing");
    if require && !available {
        panic!("landlock TCP restrictions required for CI but unavailable:\n{stderr}");
    }
    available
}

fn landlock_scope_available() -> bool {
    let output = tino_command()
        .args([
            "--write-warn-only",
            "--scope-signals",
            "--",
            "sh",
            "-c",
            "exit 0",
        ])
        .output()
        .expect("failed to probe landlock scope availability");

    let require = std::env::var_os("TINO_TEST_REQUIRE_LANDLOCK").is_some();
    if !output.status.success() {
        if require {
            panic!(
                "landlock scope probe failed with exit status {:?}\nstderr:\n{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return false;
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let available = !stderr.contains("access restriction unavailable; continuing");
    if require && !available {
        panic!("landlock IPC scopes required for CI but unavailable:\n{stderr}");
    }
    available
}

fn python3_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn next_free_tcp_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("bind ephemeral TCP port")
        .local_addr()
        .expect("query listener address")
        .port()
}

fn distinct_free_tcp_ports() -> (u16, u16) {
    let first = next_free_tcp_port();
    loop {
        let second = next_free_tcp_port();
        if second != first {
            return (first, second);
        }
    }
}

fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()))
}

fn unique_abstract_socket_name(prefix: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{prefix}-{}-{nanos}", std::process::id())
}

fn executable_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(program);
        let Ok(metadata) = std::fs::metadata(&candidate) else {
            continue;
        };
        if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
            return candidate.canonicalize().ok();
        }
    }
    None
}

fn wait_child_with_timeout(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now().checked_add(timeout);
    loop {
        if let Some(status) = child.try_wait().expect("poll child status") {
            return Some(status);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return None;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn spawn_pty_holder() -> Option<(std::process::Child, PathBuf)> {
    if !python3_available() {
        return None;
    }

    let script = r#"import os, pty, time
master, slave = pty.openpty()
print(os.ttyname(slave), flush=True)
time.sleep(5)
"#;
    let mut child = Command::new("python3")
        .args(["-u", "-c", script])
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let mut stdout = BufReader::new(child.stdout.take()?);
    let mut line = String::new();
    stdout.read_line(&mut line).ok()?;
    drop(stdout);
    let path = PathBuf::from(line.trim());
    Some((child, path))
}

#[test]
fn license_flag_prints_license() {
    let output = tino_command()
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
fn help_flag_prints_usage_and_exits_successfully() {
    let output = tino_command()
        .arg("--help")
        .output()
        .expect("failed to run tino --help");

    assert!(output.status.success(), "help flag exited with failure");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("usage: tino [OPTIONS] [--] CMD [ARGS...]"),
        "unexpected help output\n{}",
        stdout
    );
}

#[test]
fn version_flag_prints_version_and_exits_successfully() {
    let output = tino_command()
        .arg("--version")
        .output()
        .expect("failed to run tino --version");

    assert!(output.status.success(), "version flag exited with failure");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("tino "),
        "unexpected version output\n{}",
        stdout
    );
}

#[test]
fn unknown_argument_exits_with_parse_error() {
    let output = tino_command()
        .arg("--nope")
        .output()
        .expect("failed to run tino with unknown argument");

    assert_eq!(
        output.status.code(),
        Some(2),
        "unexpected exit code for parse failure: {:?}",
        output.status.code()
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unexpected argument"),
        "missing parse error message\n{}",
        stderr
    );
    assert!(
        stderr.contains("usage: tino [OPTIONS] [--] CMD [ARGS...]"),
        "missing usage text in parse failure output\n{}",
        stderr
    );
}

#[test]
fn missing_command_exits_with_error() {
    let status = tino_command()
        .status()
        .expect("failed to run tino without args");

    assert_eq!(
        status.code(),
        Some(1),
        "expected exit code 1 when CMD is missing"
    );
}

#[test]
fn successful_command_is_quiet_by_default() {
    let output = tino_command()
        .args(["--", "/bin/true"])
        .output()
        .expect("failed to run tino quiet-default test");

    assert!(
        output.status.success(),
        "quiet-default command failed: {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "default successful execution should not emit logs\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn verbose_successful_command_reports_exit() {
    let output = tino_command()
        .args(["-v", "--", "/bin/true"])
        .output()
        .expect("failed to run tino verbose exit test");

    assert!(
        output.status.success(),
        "verbose command failed: {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("INFO tino: exiting with 0"),
        "verbose execution should report exit status\n{stderr}"
    );
}

#[test]
fn remap_exit_zeroes_expected_codes() {
    let status = tino_command()
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
    let output = tino_command()
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
    let output = tino_command()
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
    let output = tino_command()
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
fn expand_env_rejects_empty_program_name() {
    let output = tino_command()
        .args([
            "--expand-env",
            "--",
            "${__TINO_TEST_MISSING_PROGRAM_123456__}",
        ])
        .output()
        .expect("failed to run tino empty-program expand-env test");

    assert!(
        !output.status.success(),
        "expected empty expanded program to fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("command program cannot be empty"),
        "expected empty-program error\n{stderr}"
    );
}

#[test]
fn explain_rejects_empty_program_name() {
    let output = tino_command()
        .args([
            "--expand-env",
            "--explain",
            "--",
            "${__TINO_TEST_MISSING_PROGRAM_123456__}",
        ])
        .output()
        .expect("failed to run tino empty-program explain test");

    assert!(
        !output.status.success(),
        "expected explain to reject empty expanded program"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("command program cannot be empty"),
        "expected empty-program error\n{stderr}"
    );
}

#[test]
fn print_config_emits_line_based_config_without_running_child() {
    let output = tino_command()
        .args([
            "--no-config",
            "--print-config",
            "--expand-env",
            "--write-preset",
            "runtime",
            "--write-allow",
            "/data/logs",
            "--bind-tcp-allow",
            "8900",
            "--exec-allow",
            "/opt/app/service",
        ])
        .output()
        .expect("failed to run tino print-config test");

    assert!(
        output.status.success(),
        "print-config failed: {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        concat!(
            "write-allow /data/logs\n",
            "write-preset runtime\n",
            "bind-tcp-allow 8900\n",
            "exec-allow /opt/app/service\n",
            "expand-env\n",
        )
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

    let output = tino_command()
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
        stdout.contains("write_restrict.presets: []"),
        "missing preset list\n{stdout}"
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
fn explain_reports_native_env_defaults_before_tini_compatibility_defaults() {
    let output = tino_command()
        .args(["--explain", "--", "/bin/true"])
        .env("TINO_SUBREAPER", "1")
        .env("TINI_SUBREAPER", "0")
        .env("TINO_VERBOSITY", "2")
        .env("TINI_VERBOSITY", "3")
        .output()
        .expect("failed to run tino native env explain test");

    assert!(
        output.status.success(),
        "native env explain scenario failed: {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("subreaper.source: env:TINO_SUBREAPER"),
        "missing native subreaper source\n{stdout}"
    );
    assert!(
        stdout.contains("verbosity: 2"),
        "missing native verbosity value\n{stdout}"
    );
    assert!(
        stdout.contains("verbosity.source: env:TINO_VERBOSITY"),
        "missing native verbosity source\n{stdout}"
    );
}

#[test]
fn explain_reports_write_preset_expansion() {
    let output = tino_command()
        .args(["--write-preset", "tmp", "--explain", "--", "/bin/true"])
        .output()
        .expect("failed to run tino explain preset test");

    assert!(
        output.status.success(),
        "explain preset scenario failed: {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(r#"write_restrict.presets: ["tmp"]"#),
        "missing preset list\n{stdout}"
    );
    assert!(
        stdout.contains("/tmp"),
        "missing expanded tmp preset path\n{stdout}"
    );
}

#[test]
fn explain_reports_tcp_restrictions() {
    let output = tino_command()
        .args([
            "--bind-tcp-allow",
            "8900",
            "--connect-tcp-allow",
            "443",
            "--explain",
            "--",
            "/bin/true",
        ])
        .output()
        .expect("failed to run tino explain tcp test");

    assert!(
        output.status.success(),
        "explain TCP scenario failed: {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("tcp_restrict.enabled: true"),
        "missing TCP restriction status\n{stdout}"
    );
    assert!(
        stdout.contains("tcp_restrict.bind_allow_ports: [8900]"),
        "missing bind TCP allowlist\n{stdout}"
    );
    assert!(
        stdout.contains("tcp_restrict.connect_allow_ports: [443]"),
        "missing connect TCP allowlist\n{stdout}"
    );
}

#[test]
fn explain_reports_ipc_scopes() {
    let output = tino_command()
        .args([
            "--scope-signals",
            "--scope-abstract-unix",
            "--explain",
            "--",
            "/bin/true",
        ])
        .output()
        .expect("failed to run tino explain scope test");

    assert!(
        output.status.success(),
        "explain IPC scope scenario failed: {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("ipc_scope.enabled: true"),
        "missing IPC scope status\n{stdout}"
    );
    assert!(
        stdout.contains("ipc_scope.signals: true"),
        "missing signal scope status\n{stdout}"
    );
    assert!(
        stdout.contains("ipc_scope.abstract_unix: true"),
        "missing abstract UNIX scope status\n{stdout}"
    );
}

#[test]
fn explain_reports_exec_restrictions() {
    let sh = executable_path("sh").expect("resolve sh path");
    let output = tino_command()
        .args([
            "--exec-allow",
            sh.to_str().expect("sh path utf-8"),
            "--explain",
            "--",
            "/bin/true",
        ])
        .output()
        .expect("failed to run tino explain exec test");

    assert!(
        output.status.success(),
        "explain exec scenario failed: {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("exec_restrict.enabled: true"),
        "missing exec restriction status\n{stdout}"
    );
    assert!(
        stdout.contains(&sh.display().to_string()),
        "missing configured exec allow path\n{stdout}"
    );
    assert!(
        stdout.contains("/bin/true"),
        "missing auto-allowed main executable path\n{stdout}"
    );
}

#[test]
fn explain_reports_device_ioctl_restrictions() {
    let output = tino_command()
        .args([
            "--device-ioctl-allow",
            "/dev/null",
            "--explain",
            "--",
            "/bin/true",
        ])
        .output()
        .expect("failed to run tino explain device ioctl test");

    assert!(
        output.status.success(),
        "explain device ioctl scenario failed: {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("device_ioctl_restrict.enabled: true"),
        "missing device ioctl restriction status\n{stdout}"
    );
    assert!(
        stdout.contains("/dev/null"),
        "missing configured device ioctl allow path\n{stdout}"
    );
}

#[test]
fn explain_does_not_execute_child() {
    let root = unique_temp_dir("tino-explain-noexec");
    std::fs::create_dir_all(&root).expect("create explain root");
    let marker = root.join("marker");

    let output = tino_command()
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
    let output = tino_command()
        .args(["--", "/definitely/missing/tino-test-binary"])
        .output()
        .expect("failed to run tino missing-binary test");

    assert!(
        !output.status.success(),
        "expected missing binary execution to fail"
    );
    assert_eq!(output.status.code(), Some(127));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("file not found; check the path or PATH lookup"),
        "expected friendly ENOENT hint\n{stderr}"
    );
}

#[test]
fn exec_failure_with_exec_restriction_reports_missing_binary_reason() {
    let output = tino_command()
        .args([
            "--write-warn-only",
            "--exec-allow",
            "/bin/sh",
            "--",
            "/definitely/missing/tino-test-binary",
        ])
        .output()
        .expect("failed to run tino missing-binary exec-restrict test");

    assert!(
        !output.status.success(),
        "expected missing binary execution to fail"
    );
    assert_eq!(output.status.code(), Some(127));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("file not found; check the path or PATH lookup"),
        "expected friendly ENOENT hint under exec restriction\n{stderr}"
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

    let output = tino_command()
        .arg("--")
        .arg(&script)
        .output()
        .expect("failed to run tino non-executable test");

    assert!(
        !output.status.success(),
        "expected non-executable child to fail"
    );
    assert_eq!(output.status.code(), Some(126));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("permission denied or file is not executable"),
        "expected friendly EACCES hint\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn signal_forwarding_reaches_child() {
    let mut child = tino_command()
        .stdout(Stdio::piped())
        .args([
            "--",
            "sh",
            "-c",
            "trap 'exit 42' TERM; printf 'ready\\n'; while :; do :; done",
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
    // SAFETY: child.id() is the live child PID returned by std::process::Child.
    let rc = unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    assert_eq!(
        rc,
        0,
        "failed to send SIGTERM: {}",
        std::io::Error::last_os_error()
    );

    let status = child.wait().expect("failed to wait on tino signal test");
    assert_eq!(
        status.code(),
        Some(42),
        "expected child to receive forwarded SIGTERM"
    );
}

#[test]
fn signal_forwarding_escalates_after_grace_without_process_group() {
    let mut child = tino_command()
        .stdout(Stdio::piped())
        .args([
            "-t",
            "50",
            "--",
            "sh",
            "-c",
            "trap '' TERM; printf 'ready\\n'; while :; do :; done",
        ])
        .spawn()
        .expect("failed to spawn tino single-process grace test");

    let mut stdout = BufReader::new(child.stdout.take().expect("grace test stdout"));
    let mut ready = String::new();
    stdout
        .read_line(&mut ready)
        .expect("read readiness marker for grace test");

    assert_eq!(ready.trim_end(), "ready", "unexpected readiness marker");
    drop(stdout);
    // SAFETY: child.id() is the live child PID returned by std::process::Child.
    let rc = unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    assert_eq!(
        rc,
        0,
        "failed to send SIGTERM: {}",
        std::io::Error::last_os_error()
    );

    let status = child.wait().expect("failed to wait on tino grace test");
    assert_eq!(
        status.code(),
        Some(137),
        "expected non-pgroup child to be escalated to SIGKILL"
    );
}

#[test]
fn pdeath_signal_is_configured_for_execed_child() {
    if !python3_available() {
        return;
    }

    let script = r#"import ctypes, signal, sys
libc = ctypes.CDLL(None)
value = ctypes.c_int()
if libc.prctl(2, ctypes.byref(value), 0, 0, 0) != 0:
    sys.exit(100)
sys.exit(0 if value.value == signal.SIGUSR1 else 101)
"#;

    let status = tino_command()
        .args(["-p", "USR1", "--", "python3", "-c", script])
        .status()
        .expect("failed to run tino pdeath test");

    assert!(
        status.success(),
        "expected execed child to inherit configured PDEATHSIG, got {status:?}"
    );
}

#[test]
fn signal_forwarding_preserves_unlisted_linux_signals() {
    let signal = libc::SIGXCPU;
    let mut child = tino_command()
        .stdout(Stdio::piped())
        .args([
            "--",
            "sh",
            "-c",
            "trap 'exit 45' \"$SIGNAL\"; printf 'ready\\n'; while true; do sleep 1; done",
        ])
        .env("SIGNAL", signal.to_string())
        .spawn()
        .expect("failed to spawn tino unlisted signal test");

    let mut stdout = BufReader::new(child.stdout.take().expect("unlisted signal test stdout"));
    let mut ready = String::new();
    stdout
        .read_line(&mut ready)
        .expect("read readiness marker for unlisted signal test");

    assert_eq!(ready.trim_end(), "ready", "unexpected readiness marker");
    drop(stdout);
    // SAFETY: child.id() is the live child PID returned by std::process::Child.
    let rc = unsafe { libc::kill(child.id() as i32, signal) };
    assert_eq!(
        rc,
        0,
        "failed to send unlisted signal: {}",
        std::io::Error::last_os_error()
    );

    let status = wait_child_with_timeout(&mut child, Duration::from_secs(2)).unwrap_or_else(|| {
        let _ = child.kill();
        let _ = child.wait();
        panic!("timed out waiting for unlisted signal forwarding test");
    });
    assert_eq!(
        status.code(),
        Some(45),
        "expected child to receive forwarded unlisted signal"
    );
}

#[test]
fn warn_on_reap_emits_warning() {
    let output = tino_command()
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
    let mut child = tino_command()
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
    // SAFETY: child.id() is the live child PID returned by std::process::Child.
    let rc = unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    assert_eq!(
        rc,
        0,
        "failed to send SIGTERM: {}",
        std::io::Error::last_os_error()
    );

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

    let status = tino_command()
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
fn write_preset_tmp_allows_writes_in_tmp() {
    if !landlock_available() {
        return;
    }

    let root = unique_temp_dir("tino-write-preset-tmp");
    std::fs::create_dir_all(&root).expect("create tmp preset root");

    let status = tino_command()
        .args([
            "--write-preset",
            "tmp",
            "--",
            "sh",
            "-c",
            r#"set -e; echo ok > "$TARGET/ok""#,
        ])
        .env("TARGET", &root)
        .status()
        .expect("run tino write preset tmp test");

    assert!(
        status.success(),
        "expected tmp preset to allow writes under /tmp"
    );
    assert!(
        root.join("ok").exists(),
        "expected tmp preset file to be created"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn landlock_allows_bind_only_on_allowlisted_tcp_ports() {
    if !landlock_available() || !landlock_tcp_available() || !python3_available() {
        return;
    }

    let (allowed_port, denied_port) = distinct_free_tcp_ports();
    let bind_script = r#"import socket, sys
s = socket.socket()
s.bind(("127.0.0.1", int(sys.argv[1])))
s.close()
"#;
    let bind_denied_script = r#"import socket, sys
s = socket.socket()
try:
    s.bind(("127.0.0.1", int(sys.argv[1])))
except PermissionError:
    sys.exit(13)
else:
    s.close()
    sys.exit(0)
"#;

    let allowed = tino_command()
        .args([
            "--bind-tcp-allow",
            &allowed_port.to_string(),
            "--",
            "python3",
            "-c",
            bind_script,
            &allowed_port.to_string(),
        ])
        .status()
        .expect("run tino bind TCP allow test");
    assert!(
        allowed.success(),
        "expected allowlisted TCP bind to succeed, got {allowed:?}"
    );

    let denied = tino_command()
        .args([
            "--bind-tcp-allow",
            &allowed_port.to_string(),
            "--",
            "python3",
            "-c",
            bind_denied_script,
            &denied_port.to_string(),
        ])
        .status()
        .expect("run tino bind TCP deny test");
    assert!(
        !denied.success(),
        "expected non-allowlisted TCP bind to fail, got {denied:?}"
    );
}

#[test]
fn landlock_allows_connect_only_on_allowlisted_tcp_ports() {
    if !landlock_available() || !landlock_tcp_available() || !python3_available() {
        return;
    }

    let allowed_listener =
        TcpListener::bind(("127.0.0.1", 0)).expect("bind allowlisted TCP listener");
    let denied_listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind denied TCP listener");
    let allowed_port = allowed_listener
        .local_addr()
        .expect("query allowlisted TCP listener addr")
        .port();
    let denied_port = denied_listener
        .local_addr()
        .expect("query denied TCP listener addr")
        .port();
    let connect_script = r#"import socket, sys
s = socket.create_connection(("127.0.0.1", int(sys.argv[1])))
s.close()
"#;
    let connect_denied_script = r#"import socket, sys
try:
    s = socket.create_connection(("127.0.0.1", int(sys.argv[1])))
except PermissionError:
    sys.exit(13)
else:
    s.close()
    sys.exit(0)
"#;

    let allowed = tino_command()
        .args([
            "--connect-tcp-allow",
            &allowed_port.to_string(),
            "--",
            "python3",
            "-c",
            connect_script,
            &allowed_port.to_string(),
        ])
        .status()
        .expect("run tino connect TCP allow test");
    assert!(
        allowed.success(),
        "expected allowlisted TCP connect to succeed, got {allowed:?}"
    );

    let denied = tino_command()
        .args([
            "--connect-tcp-allow",
            &allowed_port.to_string(),
            "--",
            "python3",
            "-c",
            connect_denied_script,
            &denied_port.to_string(),
        ])
        .status()
        .expect("run tino connect TCP deny test");
    assert!(
        !denied.success(),
        "expected non-allowlisted TCP connect to fail, got {denied:?}"
    );

    drop(allowed_listener);
    drop(denied_listener);
}

#[test]
fn landlock_exec_restrict_auto_allows_main_command() {
    if !landlock_available() {
        return;
    }

    let status = tino_command()
        .args(["--exec-allow", "/bin/sh", "--", "/bin/true"])
        .status()
        .expect("run tino exec auto-allow test");

    assert!(
        status.success(),
        "expected exec restriction to auto-allow main command, got {status:?}"
    );
}

#[test]
fn landlock_exec_restrict_auto_allows_env_shebang_command() {
    if !landlock_available() {
        return;
    }

    let env = executable_path("env");
    let sh = executable_path("sh");
    let (Some(env), Some(_sh)) = (env, sh) else {
        return;
    };

    let root = unique_temp_dir("tino-env-shebang");
    std::fs::create_dir_all(&root).expect("create env shebang dir");
    let script = root.join("script");
    std::fs::write(
        &script,
        format!("#!{} sh\nexit 0\n", env.display()).as_bytes(),
    )
    .expect("write env shebang script");
    let mut perms = std::fs::metadata(&script)
        .expect("stat env shebang script")
        .permissions();
    perms.set_mode(perms.mode() | 0o755);
    std::fs::set_permissions(&script, perms).expect("chmod env shebang script");

    let status = tino_command()
        .args([
            "--exec-allow",
            script.to_str().expect("script path utf-8"),
            "--",
            script.to_str().expect("script path utf-8"),
        ])
        .status()
        .expect("run tino env shebang exec test");

    assert!(
        status.success(),
        "expected env shebang script to succeed under exec restriction, got {status:?}"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn landlock_exec_restrict_blocks_non_allowlisted_execs() {
    if !landlock_available() {
        return;
    }

    let sh = executable_path("sh");
    let uname = executable_path("uname");
    let (Some(sh), Some(uname)) = (sh, uname) else {
        return;
    };

    let status = tino_command()
        .args([
            "--exec-allow",
            sh.to_str().expect("sh path utf-8"),
            "--",
            sh.to_str().expect("sh path utf-8"),
            "-c",
            r#""$UNAME" >/dev/null 2>/dev/null"#,
        ])
        .env("UNAME", &uname)
        .status()
        .expect("run tino exec deny test");

    assert!(
        !status.success(),
        "expected non-allowlisted exec to fail, got {status:?}"
    );
}

#[test]
fn landlock_exec_restrict_allows_configured_execs() {
    if !landlock_available() {
        return;
    }

    let sh = executable_path("sh");
    let uname = executable_path("uname");
    let (Some(sh), Some(uname)) = (sh, uname) else {
        return;
    };

    let status = tino_command()
        .args([
            "--exec-allow",
            sh.to_str().expect("sh path utf-8"),
            "--exec-allow",
            uname.to_str().expect("uname path utf-8"),
            "--",
            sh.to_str().expect("sh path utf-8"),
            "-c",
            r#""$UNAME" >/dev/null"#,
        ])
        .env("UNAME", &uname)
        .status()
        .expect("run tino exec allow test");

    assert!(
        status.success(),
        "expected configured exec to succeed, got {status:?}"
    );
}

#[test]
fn landlock_device_ioctl_restrict_allows_configured_paths() {
    if !landlock_available() || !python3_available() {
        return;
    }

    let Some((mut holder, tty_path)) = spawn_pty_holder() else {
        return;
    };
    let script = r#"import fcntl, os, sys, termios
fd = os.open(sys.argv[1], os.O_RDONLY | os.O_NOCTTY)
fcntl.ioctl(fd, termios.TIOCGWINSZ, b"\0" * 8)
os.close(fd)
"#;

    let status = tino_command()
        .args([
            "--device-ioctl-allow",
            tty_path.to_str().expect("tty path utf-8"),
            "--",
            "python3",
            "-c",
            script,
            tty_path.to_str().expect("tty path utf-8"),
        ])
        .status()
        .expect("run tino device ioctl allow test");

    let _ = holder.kill();
    let _ = holder.wait();

    assert!(
        status.success(),
        "expected configured device ioctl path to succeed, got {status:?}"
    );
}

#[test]
fn landlock_device_ioctl_restrict_blocks_non_allowlisted_paths() {
    if !landlock_available() || !python3_available() {
        return;
    }

    let Some((mut holder, tty_path)) = spawn_pty_holder() else {
        return;
    };
    let script = r#"import fcntl, os, sys, termios
try:
    fd = os.open(sys.argv[1], os.O_RDONLY | os.O_NOCTTY)
    try:
        fcntl.ioctl(fd, termios.TIOCGWINSZ, b"\0" * 8)
    finally:
        os.close(fd)
except PermissionError:
    sys.exit(13)
else:
    sys.exit(0)
"#;

    let status = tino_command()
        .args([
            "--device-ioctl-allow",
            "/dev/null",
            "--",
            "python3",
            "-c",
            script,
            tty_path.to_str().expect("tty path utf-8"),
        ])
        .status()
        .expect("run tino device ioctl deny test");

    let _ = holder.kill();
    let _ = holder.wait();

    assert!(
        !status.success(),
        "expected non-allowlisted device ioctl to fail, got {status:?}"
    );
}

#[test]
fn landlock_signal_scope_allows_same_domain_signals() {
    if !landlock_available() || !landlock_scope_available() {
        return;
    }

    let status = tino_command()
        .args([
            "--scope-signals",
            "--",
            "sh",
            "-c",
            r#"sleep 10 & pid=$!; kill -TERM "$pid"; wait "$pid"; code=$?; test "$code" -eq 143"#,
        ])
        .status()
        .expect("run tino same-domain signal scope test");

    assert!(
        status.success(),
        "expected same-domain signal delivery to succeed, got {status:?}"
    );
}

#[test]
fn landlock_signal_scope_blocks_out_of_domain_signals() {
    if !landlock_available() || !landlock_scope_available() {
        return;
    }

    let mut target = Command::new("sh")
        .stdout(Stdio::piped())
        .args([
            "-c",
            "trap 'exit 0' TERM; printf 'ready\\n'; while true; do sleep 1; done",
        ])
        .spawn()
        .expect("spawn out-of-domain signal target");
    let mut stdout = BufReader::new(target.stdout.take().expect("signal scope target stdout"));
    let mut ready = String::new();
    stdout
        .read_line(&mut ready)
        .expect("read readiness marker for signal scope target");
    assert_eq!(ready.trim_end(), "ready", "unexpected readiness marker");
    drop(stdout);

    let status = tino_command()
        .args([
            "--scope-signals",
            "--",
            "sh",
            "-c",
            r#"kill -TERM "$TARGET_PID" 2>/dev/null || exit 13"#,
        ])
        .env("TARGET_PID", target.id().to_string())
        .status()
        .expect("run tino out-of-domain signal scope test");

    assert!(
        !status.success(),
        "expected out-of-domain signal delivery to fail, got {status:?}"
    );
    assert!(
        target
            .try_wait()
            .expect("poll signal scope target")
            .is_none(),
        "expected out-of-domain target to remain alive"
    );

    // SAFETY: target.id() is the live child PID returned by std::process::Child.
    let rc = unsafe { libc::kill(target.id() as i32, libc::SIGTERM) };
    assert_eq!(
        rc,
        0,
        "terminate signal scope target: {}",
        std::io::Error::last_os_error()
    );
    let cleanup = target.wait().expect("wait for signal scope target");
    assert!(
        cleanup.success(),
        "signal scope target cleanup failed: {cleanup:?}"
    );
}

#[test]
fn landlock_abstract_unix_scope_allows_same_domain_connects() {
    if !landlock_available() || !landlock_scope_available() || !python3_available() {
        return;
    }

    let script = r#"import socket, threading, uuid
name = "\0" + "tino-scope-" + uuid.uuid4().hex
server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
server.bind(name)
server.listen(1)
accepted = []

def accept_once():
    conn, _ = server.accept()
    conn.close()
    accepted.append(True)

thread = threading.Thread(target=accept_once)
thread.start()
client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
client.connect(name)
client.close()
thread.join(timeout=2)
assert accepted, "same-domain abstract UNIX socket connect should succeed"
server.close()
"#;

    let status = tino_command()
        .args(["--scope-abstract-unix", "--", "python3", "-c", script])
        .status()
        .expect("run tino same-domain abstract UNIX scope test");

    assert!(
        status.success(),
        "expected same-domain abstract UNIX connect to succeed, got {status:?}"
    );
}

#[test]
fn landlock_abstract_unix_scope_blocks_out_of_domain_connects() {
    if !landlock_available() || !landlock_scope_available() || !python3_available() {
        return;
    }

    let socket_name = unique_abstract_socket_name("tino-abstract-scope");
    let server_script = r#"import socket, sys, time
name = "\0" + sys.argv[1]
sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.bind(name)
sock.listen(1)
print("ready", flush=True)
time.sleep(5)
sock.close()
"#;
    let connect_script = r#"import socket, sys
sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
try:
    sock.connect("\0" + sys.argv[1])
except PermissionError:
    sys.exit(13)
else:
    sock.close()
    sys.exit(0)
"#;

    let mut server = Command::new("python3")
        .args(["-u", "-c", server_script, &socket_name])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn abstract UNIX scope server");
    let mut stdout = BufReader::new(
        server
            .stdout
            .take()
            .expect("abstract UNIX scope server stdout"),
    );
    let mut ready = String::new();
    stdout
        .read_line(&mut ready)
        .expect("read readiness marker for abstract UNIX scope server");
    assert_eq!(ready.trim_end(), "ready", "unexpected readiness marker");
    drop(stdout);

    let status = tino_command()
        .args([
            "--scope-abstract-unix",
            "--",
            "python3",
            "-c",
            connect_script,
            &socket_name,
        ])
        .status()
        .expect("run tino out-of-domain abstract UNIX scope test");

    assert!(
        !status.success(),
        "expected out-of-domain abstract UNIX connect to fail, got {status:?}"
    );

    let _ = server.kill();
    let _ = server.wait();
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

    let status = tino_command()
        .args([
            "--write-restrict",
            "--write-allow",
            allowed_dir.to_str().expect("allowed dir utf-8"),
            "--",
            "sh",
            "-c",
            r#"set -e; echo ok > "$ALLOWED/ok"; if (echo denied > "$OUTSIDE/deny") 2>/dev/null; then exit 0; else exit 13; fi"#,
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
