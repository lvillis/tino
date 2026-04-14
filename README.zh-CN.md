<!-- ─── 语言切换 ─────────────────────────────────────────────── -->
<div align="right">

<a href="README.md">🇺🇸 English</a> ·
<span style="color:#999;">🇨🇳 中文</span>

</div>

<h1 align="center"><code>tino</code></h1>

<p align=center>
tino：基于 Rust 的 tiny init（PID 1）——
<a href="https://github.com/krallin/tini">tini</a> 的现代替代品
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
| **命令环境变量展开** | `--expand-env` 可展开 `${VAR}`、`${VAR:-default}`，无需 `/bin/sh` |
| **解释模式** | `--explain` 打印最终生效配置和子命令 argv，但不启动子进程 |
| **写入限制** | `--write-restrict` 限制子进程文件系统写入，仅允许写入白名单目录（Linux；可能需要 seccomp 放行） |
| **TCP 端口限制** | `--bind-tcp-allow` / `--connect-tcp-allow` 通过 Landlock 限制子进程可绑定/连接的 TCP 端口 |
| **IPC 域限制** | `--scope-signals` / `--scope-abstract-unix` 将 IPC 约束在同一个 Landlock 域内 |
| **执行限制** | `--exec-allow` 限制子进程启动后还能执行哪些可执行文件 |
| **设备 ioctl 限制** | `--device-ioctl-allow` 限制哪些设备节点可执行 ioctl 操作 |

## 📦 安装

```bash
# 本地安装（Cargo）
cargo install tino

# 构建静态二进制（例如在 Docker 中作为 PID 1）
cargo build --release --target x86_64-unknown-linux-musl

# Docker 镜像（包含 /sbin/tino）
docker pull lvillis/tino
```

## 📦 二进制发布

GitHub Release 提供稳定、可预测的版本归档，统一采用单顶层目录布局：

```text
tino-<version>-<os>-<arch>-<abi>/
├── tino
├── LICENSE
└── README.md
```

支持的平台矩阵与资产映射如下：

| OCI 平台 | Rust target | Release 资产 |
| --- | --- | --- |
| `linux/amd64` | `x86_64-unknown-linux-gnu` | `tino-<version>-linux-x86_64-gnu.tar.gz` |
| `linux/amd64` | `x86_64-unknown-linux-musl` | `tino-<version>-linux-x86_64-musl.tar.gz` |
| `linux/arm64` | `aarch64-unknown-linux-musl` | `tino-<version>-linux-aarch64-musl.tar.gz` |
| `linux/arm/v6` | `arm-unknown-linux-gnueabihf` | `tino-<version>-linux-arm-gnueabihf.tar.gz` |
| `linux/arm/v7` | `armv7-unknown-linux-gnueabihf` | `tino-<version>-linux-armv7-gnueabihf.tar.gz` |

每个版本还会额外提供：

- 覆盖全部正式资产的 `SHA256SUMS`
- 与每个归档对应的 SPDX JSON 格式 `*.spdx.json` SBOM
- 针对归档和 SBOM 的 GitHub artifact attestation

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
- `--expand-env` 会在 `execvp` 前展开子命令参数；支持 `${VAR}`、`${VAR:-default}`，
  以及用 `$$` 表示字面量 `$`。未加花括号的 `$VAR` 会保持原样。这不是 shell。
- `--explain` 会打印最终生效配置、展开后的子命令 argv，以及写入白名单，
  然后直接退出；它是解释模式，不是模拟执行。
- 写入限制（可选，Linux）：`--write-restrict --write-allow /path`（可重复）可阻止对白名单外目录的写入；
  默认严格（用 `--write-warn-only` 保持继续运行）。
- `--write-preset tmp` 会展开为 `/tmp` 和 `/var/tmp`；`--write-preset runtime` 会在此基础上再加 `/run`。
  缺失的标准目录会被自动跳过，preset 也可以和 `--write-allow` 叠加使用。
- 写入限制默认保持 `/dev` 可写以保证 TTY/stdout（用 `--write-no-dev` 禁用）。
- TCP 限制（可选，Linux）：`--bind-tcp-allow 8900` 可限制子进程只能监听指定本地 TCP 端口；
  `--connect-tcp-allow 443` 可限制对外 TCP 连接的目标端口。这两项依赖 Landlock ABI v4+。
- IPC 域限制（可选，Linux）：`--scope-signals` 会把发信号限制在相同或嵌套的 Landlock 域内；
  `--scope-abstract-unix` 会把 abstract UNIX socket 的连接限制在相同域内。这两项依赖
  Landlock ABI v6+。
- 执行限制（可选，Linux）：`--exec-allow /path` 可限制子进程启动后还能执行哪些可执行文件。
  针对指向文件的 allow 项，会自动补上直接 shebang 解释器和 ELF 动态加载器；目录级
  allow 项不会自动展开整棵依赖链。
- 设备 ioctl 限制（可选，Linux）：`--device-ioctl-allow /dev/pts/0` 可把 ioctl(2)
  限制到明确允许的设备节点或目录。这项能力依赖 Landlock ABI v5+。
- 只要请求了任意 Landlock 能力，`--write-warn-only` 就会把启动失败降级为告警，并继续运行，
  但不会施加对应限制。
- Docker：如果 Landlock syscall 被拦截，使用 `--security-opt seccomp=./seccomp-landlock.json`
  （或测试时使用 `seccomp=unconfined`）。

## 🛡️ Landlock + Docker（seccomp）

Docker 默认的 seccomp profile 往往会拦截 `landlock_*` syscall。本仓库提供了
`seccomp-landlock.json`（基于 `moby/profiles`，见 `seccomp-landlock.upstream.sha`）。

```bash
docker run --rm -it \
  --security-opt seccomp=./seccomp-landlock.json \
  <image> \
  /sbin/tino --write-restrict --write-allow /data -- <cmd> ...
```

对于 scratch / distroless 镜像，如果只需要简单参数展开而不想依赖 shell，可直接使用：

```bash
/sbin/tino --expand-env -- /opt/app/collectord -port=${SERVICE_PORT:-8900}
```

对于常见运行时布局，可用 preset 降低样板配置：

```bash
/sbin/tino --write-preset runtime --write-allow /data/logs -- /opt/app/collectord
```

如果只想把服务监听端口收紧到固定值，而不额外引入防火墙层：

```bash
/sbin/tino --bind-tcp-allow 8900 -- /opt/app/collectord --port=8900
```

如果要防止插件类子进程向域外进程发信号，或连接域外 abstract UNIX socket：

```bash
/sbin/tino --scope-signals --scope-abstract-unix -- /opt/app/untrusted-worker
```

如果要阻止被管理服务继续拉起额外 helper 命令，只保留显式白名单：

```bash
/sbin/tino --exec-allow /opt/app/collectord -- /opt/app/collectord
```

如果只想允许已知 PTY 或设备目录执行 ioctl：

```bash
/sbin/tino --device-ioctl-allow /dev/pts -- /opt/app/interactive-worker
```

如果只想查看最终 argv 和生效的安全配置，而不真正执行子进程：

```bash
/sbin/tino --expand-env --write-preset runtime --write-allow /data/logs --bind-tcp-allow 8900 --scope-signals --exec-allow /opt/app/collectord --explain -- \
  /opt/app/collectord -port=${SERVICE_PORT:-8900}
```

若希望对所有容器默认生效，可配置 Docker daemon：

```json
{ "seccomp-profile": "/etc/docker/seccomp-landlock.json" }
```

可用 `python scripts/update-seccomp-landlock.py` 刷新该 profile。

## 🧪 测试

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all --verbose
```

在 Unix 目标上，`tests/unix_behaviour.rs` 覆盖 `--license`、缺少 CMD 的错误路径，以及退出码重映射流程。
