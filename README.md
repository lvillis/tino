<!-- ─── Language Switch & ToC (top-right) ─────────────────────────── -->
<div align="right">

<span style="color:#999;">🇺🇸 English</span> ·
<a href="README.zh-CN.md">🇨🇳 中文</a> &nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;|&nbsp;&nbsp;&nbsp;&nbsp;&nbsp; Table of Contents ↗️

</div>

<h1 align="center"><code>tino</code></h1>

<p align=center>💡 A Rust-based tiny init process – a modern alternative to <code>tini</code></p>

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
| **Signal forwarding**   | Forwards 10+ common signals; `-g` mode falls back gracefully if PGID can't be assigned        |
| **Graceful shutdown**   | `SIGTERM → configurable wait → SIGKILL`; timeout set via `-t/--grace-ms`                       |
| **Exit-code remap**     | `-e <code>` maps specific child exit codes to zero for health-checks                           |
| **Verbosity control**   | `-v/-vv/-vvv` or `TINI_VERBOSITY=1..3` via `tracing`                                           |
| **Security-audited**    | `#![deny(unsafe_op_in_unsafe_fn)]`, minimal unsafe surface, no dynamic allocation in hot paths |
| **Cross-platform**      | Linux glibc / musl; works as PID 1 in Docker, LXC, Podman, Kubernetes, fire-cracker, etc.      |
| **Env overrides**       | `TINI_SUBREAPER`, `TINI_KILL_PROCESS_GROUP`, `TINI_VERBOSITY` toggle defaults without flags    |

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

## 🧪 Testing

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all --verbose
```

On Unix targets an integration suite in `tests/unix_behaviour.rs` covers the CLI licence output,
missing-command error path, and exit-code remapping flow.
