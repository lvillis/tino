#![cfg(target_os = "linux")]

use std::fs;
use std::process::Command;

fn tino_bin() -> &'static str {
    env!("CARGO_BIN_EXE_tino")
}

fn readme() -> String {
    fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md")).expect("read README.md")
}

#[test]
fn readme_contains_core_install_and_usage_snippets() {
    let readme = readme();

    for snippet in [
        "COPY --from=lvillis/tino:latest /sbin/tino /sbin/tino",
        "ENTRYPOINT [\"/sbin/tino\", \"-g\", \"-s\", \"--\"]",
        "ENTRYPOINT [\"/sbin/tino\", \"--expand-env\", \"--\"]",
        "--write-preset runtime",
        "--write-allow /data/logs",
        "--bind-tcp-allow 8900",
        "--exec-allow /opt/app/service",
        "--security-opt seccomp=./seccomp-landlock.json",
    ] {
        assert!(
            readme.contains(snippet),
            "README is missing documented snippet: {snippet}"
        );
    }
}

#[test]
fn readme_and_help_stay_aligned_on_key_flags() {
    let readme = readme();
    let output = Command::new(tino_bin())
        .arg("--help")
        .output()
        .expect("run tino --help");
    assert!(output.status.success(), "--help exited with failure");
    let help = String::from_utf8_lossy(&output.stdout);

    for flag in [
        "--write-restrict",
        "--write-allow",
        "--write-preset",
        "--bind-tcp-allow",
        "--connect-tcp-allow",
        "--scope-signals",
        "--scope-abstract-unix",
        "--exec-allow",
        "--device-ioctl-allow",
        "--expand-env",
        "--explain",
    ] {
        assert!(
            readme.contains(flag),
            "README is missing documented flag: {flag}"
        );
        assert!(help.contains(flag), "--help is missing flag: {flag}");
    }
}
