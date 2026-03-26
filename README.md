<!-- ─── Language Switch & ToC (top-right) ─────────────────────────── -->
<div align="right">

<span style="color:#999;">🇺🇸 English</span> ·
<a href="README.zh-CN.md">🇨🇳 中文</a> &nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;|&nbsp;&nbsp;&nbsp;&nbsp;&nbsp; Table of Contents ↗️

</div>

<h1 align="center"><code>tino</code></h1>

<p align=center>
tino is a tiny init process (PID 1) for Docker/Kubernetes containers, written in Rust —
a modern alternative to <a href="https://github.com/krallin/tini">tini</a>.
</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/tino.svg)](https://crates.io/crates/tino)&nbsp;
[![Repo Size](https://img.shields.io/github/repo-size/lvillis/tino?color=328657)](https://github.com/lvillis/tino)&nbsp;
[![CI](https://github.com/lvillis/tino/actions/workflows/ci.yaml/badge.svg)](https://github.com/lvillis/tino/actions)&nbsp;
[![Docker Pulls](https://img.shields.io/docker/pulls/lvillis/tino?style=flat-square)](https://hub.docker.com/r/lvillis/tino)&nbsp;
[![Image Size](https://img.shields.io/docker/image-size/lvillis/tino/latest?style=flat-square)](https://hub.docker.com/r/lvillis/tino)&nbsp;
[![Say Thanks](https://img.shields.io/badge/Say%20Thanks-!-1EAEDB.svg)](mailto:lvillis@outlook.com?subject=Thanks%20for%20tino!)

</div>

---

## ✨ Features

| Feature                 | Description                                                                                    |
|-------------------------|------------------------------------------------------------------------------------------------|
| **Pure Rust, static**   | No runtime deps, musl-linked binary ≤ 60 kB                                                    |
| **Sub-reaper mode**     | `-s` flag enables `PR_SET_CHILD_SUBREAPER`, reaps orphaned children                            |
| **Parent-death signal** | `-p <SIG>` mirrors `tini -p` (`PR_SET_PDEATHSIG`)                                              |
| **Signal forwarding**   | Forwards most signals; `-g` mode falls back gracefully if PGID can't be assigned              |
| **Graceful shutdown**   | `SIGTERM → configurable wait → SIGKILL`; timeout set via `-t/--grace-ms`                       |
| **Exit-code remap**     | `-e <code>` maps specific child exit codes to zero for health-checks                           |
| **Verbosity control**   | `-v/-vv/-vvv` or `TINI_VERBOSITY=1..3` via `tracing`                                           |
| **Security-audited**    | `#![deny(unsafe_op_in_unsafe_fn)]`, minimal unsafe surface, no dynamic allocation in hot paths |
| **Cross-platform**      | Linux glibc / musl; works as PID 1 in Docker, LXC, Podman, Kubernetes, fire-cracker, etc.      |
| **Env overrides**       | `TINI_SUBREAPER`, `TINI_KILL_PROCESS_GROUP`, `TINI_VERBOSITY` act as defaults (CLI wins)       |
| **Command env expansion** | `--expand-env` expands `${VAR}` and `${VAR:-default}` without requiring `/bin/sh`            |
| **Landlock sandbox**    | `--landlock` restricts filesystem writes to allowlisted directories (Linux; may need seccomp)  |

## 📦 Installation

```bash
# Install locally with Cargo
cargo install tino

# Build a static binary (e.g. for PID 1 in Docker)
cargo build --release --target x86_64-unknown-linux-musl

# Docker image (includes /sbin/tino)
docker pull lvillis/tino
```

## 📦 Binary Releases

GitHub Releases publish stable, versioned archives with a single top-level directory:

```text
tino-<version>-<os>-<arch>-<abi>/
├── tino
├── LICENSE
└── README.md
```

Supported platform matrix and asset mapping:

| OCI platform | Rust target | Release asset |
| --- | --- | --- |
| `linux/amd64` | `x86_64-unknown-linux-gnu` | `tino-<version>-linux-x86_64-gnu.tar.gz` |
| `linux/amd64` | `x86_64-unknown-linux-musl` | `tino-<version>-linux-x86_64-musl.tar.gz` |
| `linux/arm64` | `aarch64-unknown-linux-musl` | `tino-<version>-linux-aarch64-musl.tar.gz` |
| `linux/arm/v6` | `arm-unknown-linux-gnueabihf` | `tino-<version>-linux-arm-gnueabihf.tar.gz` |
| `linux/arm/v7` | `armv7-unknown-linux-gnueabihf` | `tino-<version>-linux-armv7-gnueabihf.tar.gz` |

Every release also includes:

- `SHA256SUMS` covering all published assets
- per-asset `*.spdx.json` SBOM files in SPDX JSON format
- GitHub artifact attestations for the archives and attached SBOMs

## 🚀 Quick Start

```bash
# Replace tini in your Dockerfile
ENTRYPOINT ["/sbin/tino", "-g", "-s", "--"]

# Run locally
tino -- echo "hello from child"
```

## 🧭 Runtime Notes

- `-g/--pgroup-kill` logs a warning and falls back to single-process signalling when process-group
  creation fails (for example inside constrained PID namespaces).
- tino's internal signalfd is opened with `CLOEXEC`, ensuring child workloads do not inherit extra
  file descriptors.
- Logging setup is idempotent: repeated initialisation (tests, embedding) no longer panics.
- `TINI_*` env vars only apply when the corresponding CLI flag is not set (CLI wins).
- `--expand-env` expands child command arguments before `execvp`; supported forms are `${VAR}`,
  `${VAR:-default}`, and `$$` for a literal dollar sign. Unbraced `$VAR` is left unchanged.
  This is not a shell.
- Landlock (optional, Linux): `--landlock --landlock-writable /path` (repeatable) or
  `--landlock-profile file` (one path per line) denies filesystem writes outside allowlisted
  directories; default is strict (use `--landlock-warn-only` to continue).
- Landlock keeps `/dev` writable for TTY/stdout by default (disable with `--landlock-no-dev`).
- Docker: if Landlock syscalls are blocked, use `--security-opt seccomp=./seccomp-landlock.json`
  (or `seccomp=unconfined` for testing).

## 🛡️ Landlock + Docker (seccomp)

Docker's default seccomp profile often blocks `landlock_*` syscalls. This repo includes
`seccomp-landlock.json`, based on `moby/profiles` (see `seccomp-landlock.upstream.sha`).

```bash
docker run --rm -it \
  --security-opt seccomp=./seccomp-landlock.json \
  <image> \
  /sbin/tino --landlock --landlock-writable /data -- <cmd> ...
```

For scratch/distroless workloads that need simple parameter expansion without a shell:

```bash
/sbin/tino --expand-env -- /opt/app/collectord -port=${SERVICE_PORT:-8900}
```

To make this the default for all containers, set Docker's daemon config:

```json
{ "seccomp-profile": "/etc/docker/seccomp-landlock.json" }
```

Refresh the profile with `python scripts/update-seccomp-landlock.py`.

## 🧪 Testing

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all --verbose
```

On Unix targets an integration suite in `tests/unix_behaviour.rs` covers the CLI licence output,
missing-command error path, and exit-code remapping flow.
