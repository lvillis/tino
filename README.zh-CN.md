<!-- ─── 语言切换 ─────────────────────────────────────────────── -->
<div align="right">

<a href="README.md">🇺🇸 English</a> ·
<span style="color:#999;">🇨🇳 中文</span>

</div>

<h1 align="center"><code>tino</code></h1>

<p align=center>💡 基于 Rust 的 tiny init 进程 —— <code>tini</code> 的现代替代品</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/tino.svg)](https://crates.io/crates/tino)&nbsp;
[![Repo Size](https://img.shields.io/github/repo-size/lvillis/tino?color=328657)](https://github.com/lvillis/tino)&nbsp;
[![CI](https://github.com/lvillis/tino/actions/workflows/ci.yaml/badge.svg)](https://github.com/lvillis/tino/actions)&nbsp;
[![Docker Pulls](https://img.shields.io/docker/pulls/lvillis/tino?style=flat-square)](https://hub.docker.com/r/lvillis/tino)&nbsp;
[![Image Size](https://img.shields.io/docker/image-size/lvillis/tino/latest?style=flat-square)](https://hub.docker.com/r/lvillis/tino)&nbsp;
[![Say Thanks](https://img.shields.io/badge/Say%20Thanks-!-1EAEDB.svg)](mailto:lvillis@outlook.com?subject=Thanks%20for%20tino!)

</div>

---

## ✨ 特性

| 特性 | 说明 |
|------|------|
| **纯 Rust，静态链接** | 无运行时依赖，musl 静态二进制 ≤ 60 kB |
| **Sub-reaper 模式** | `-s` 启用 `PR_SET_CHILD_SUBREAPER`，回收孤儿子进程 |
| **父进程退出信号** | `-p <SIG>` 对齐 `tini -p`（`PR_SET_PDEATHSIG`） |
| **信号转发** | 将大多数信号转发给子进程；`-g` 在无法设置 PGID 时会优雅降级 |
| **优雅退出** | `SIGTERM → 等待 → SIGKILL`；超时由 `-t/--grace-ms` 控制 |
| **退出码重映射** | `-e <code>` 将指定子进程退出码映射为 0（便于健康检查） |
| **日志等级** | `-v/-vv/-vvv` 或 `TINI_VERBOSITY=1..3`（基于 `tracing`） |
| **安全审计** | `#![deny(unsafe_op_in_unsafe_fn)]`，`unsafe` 面积最小化 |
| **跨平台构建** | Linux glibc / musl；可作为 Docker/LXC/Podman/K8s 的 PID 1 |
| **环境变量覆盖** | `TINI_SUBREAPER` / `TINI_KILL_PROCESS_GROUP` / `TINI_VERBOSITY` 作为默认值（命令行优先） |

## 🚀 快速开始

```bash
# 在 Dockerfile 中替换 tini
ENTRYPOINT ["/sbin/tino", "-g", "-s", "--"]

# 本地运行
tino -- echo "hello from child"
```

## 🧭 运行时说明

- `-g/--pgroup-kill` 会在无法设置进程组时输出告警，并回退为仅对单个 PID 发信号（例如受限的 PID namespace）。
- tino 内部使用 `signalfd` 且启用 `CLOEXEC`，确保子进程不会继承额外的文件描述符。
- 日志初始化是幂等的：重复初始化（测试、嵌入场景）不会 panic。
- `TINI_*` 环境变量仅在对应命令行 flag 未显式提供时生效（命令行优先）。

## 🧪 测试

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all --verbose
```

在 Unix 目标上，`tests/unix_behaviour.rs` 覆盖 `--license`、缺少 CMD 的错误路径，以及退出码重映射流程。

