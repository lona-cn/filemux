# Filemux

Rust 用户态文件共享服务，提供 `filemux` 命令行程序和 egui 原生桌面界面，支持 TFTP 上传/下载、只读 HTTP，以及只读 NFSv3 UDP/TCP。

适用于可信局域网中的启动文件分发和 NFS 文件访问。它不是完整 DHCP/PXE 服务，也不提供 NFSv4、Kerberos 或远程管理 API。U-Boot 真机与完整无盘启动尚未验证；协议测试和构建通过不等于设备启动验证。

## 安装

### 从 crates.io 安装 CLI

需要 Rust 工具链及对应平台的 C 编译器和链接环境；SQLite 随源码编译：

```sh
cargo install filemux-cli --version 0.1.1 --locked
filemux --help
```

Cargo 包名是 **`filemux-cli`**，安装后的命令名是 **`filemux`**。默认安装到当前用户的 `$CARGO_HOME/bin`（未设置时通常为 `~/.cargo/bin`，Windows 为 `%USERPROFILE%\.cargo\bin`），请确认该目录在 `PATH` 中。此方式只编译 CLI，不引入 GUI、窗口或 GPU 依赖。

### 从 crates.io 安装 GUI

```sh
cargo install filemux-gui --version 0.1.1 --locked
filemux-gui --help
```

GUI 的 Cargo 包名和命令名均为 **`filemux-gui`**，安装位置与 CLI 相同。`filemux-core` 是两者共用的库，不是可安装运行的程序。

GUI 从源码编译，需要 Rust 工具链及原生构建依赖：Windows 需要 MSVC C/C++ 编译和链接环境，macOS 需要 Xcode Command Line Tools，Linux 需要 C/C++ 构建工具、`pkg-config` 以及 eframe 所需的 X11/Wayland 开发库。运行还需要系统图形环境与图形驱动；`cargo install` 不会安装这些系统组件。SQLite 随源码编译，中文字体及其 OFL 许可已随 GUI 源码包提供。

### 下载 GitHub Release 二进制

[v0.1.0 Release](https://github.com/lona-cn/filemux/releases/tag/v0.1.0) 已提供以下四个平台的原生构建，也可在 [Releases](https://github.com/lona-cn/filemux/releases) 查看后续版本：

| 平台 | Target | 压缩格式 |
| --- | --- | --- |
| Windows x64 | `x86_64-pc-windows-msvc` | ZIP |
| Linux x64 | `x86_64-unknown-linux-gnu` | TAR.GZ |
| macOS Intel | `x86_64-apple-darwin` | TAR.GZ |
| macOS Apple Silicon | `aarch64-apple-darwin` | TAR.GZ |

下载对应压缩包，并按同一 Release 的 `SHA256SUMS` 校验。压缩包命名为 `filemux-<tag>-<target>`，包含 CLI、GUI、README、配置示例、阶段文档与中文字体许可；Unix 二进制保留执行权限。解压后运行 `filemux` / `filemux-gui`，Windows 对应 `.exe`；不需要安装 Rust。若未将解压目录加入 `PATH`，请使用可执行文件的完整或相对路径。

这些压缩包不是安装器，不包含系统图形运行环境，也没有 Windows 代码签名或 Apple Developer ID 签名、公证。Linux 包在 Ubuntu 22.04 构建，不是完全静态二进制。

上表及下载链接对应已有的 **v0.1.0**，不是 0.1.1 二进制发布。更新源码或发布 crates.io 的 0.1.1 包不会自动创建 GitHub Release；只有为对应提交创建并推送新的 `v0.1.1` tag、且 Release 工作流成功完成后，才会有该版本的二进制包。

### 从源码开发

```sh
git clone https://github.com/lona-cn/filemux.git
cd filemux
cargo build -p filemux-cli --locked
cargo build -p filemux-gui --locked
```

CLI 位于 `target/debug/filemux`，GUI 位于 `target/debug/filemux-gui`，Windows 对应 `.exe`。也可使用 `cargo run -p filemux-cli -- <参数>` 或 `cargo run -p filemux-gui -- <参数>` 开发运行。

清单声明 Rust 1.88、edition 2024；当前 CI 和发布构建使用 Rust 1.99.0，尚未单独验证声明的最低版本。项目不要求保留 `rust-toolchain.toml`。Windows 需要 MSVC 对应的链接环境；Linux GUI 需要 X11/Wayland 运行环境、图形驱动及 eframe 的系统构建依赖。

## 快速开始

以下使用已安装并加入 `PATH` 的 `filemux`。先创建共享目录，持久状态与共享目录分开存放：

```sh
mkdir share
filemux --config filemux.toml init --root ./share --state-dir ./.filemux-state
filemux --config filemux.toml check
filemux --config filemux.toml serve
```

- `init` 创建配置，不覆盖已有文件，也不启动服务。
- `check` 检查配置格式和目录等基本条件，不初始化状态、不绑定端口，因此不保证监听端口可用。
- `serve` 前台运行，按 Ctrl+C 停止。启动失败会退出并停止本次已经启动的其他服务，不留下未说明的部分运行状态。
- 初始配置仅启用 HTTP，监听 `127.0.0.1:8080`。TFTP、NFS 未启用，TFTP 上传也未开启。
- 指定的配置文件不存在时明确报错。不指定 `--config` 时，使用应用配置位置；`serve` 在没有该文件时可用命令行参数启动。

将文件放入 `share` 后，可在本机访问 <http://127.0.0.1:8080/>。下面的监听参数只覆盖本次运行，不修改配置文件。

本机高端口示例，不需要为低端口提权：

```sh
filemux --config filemux.toml serve --tftp-bind 127.0.0.1:1069 --nfs-bind 127.0.0.1:12049 --rpcbind 127.0.0.1:12111
```

显式允许 TFTP 上传：

```sh
filemux --config filemux.toml serve --tftp-bind 127.0.0.1:1069 --allow-upload
```

上传仍不覆盖已有文件；HTTP、NFS 不会因此获得写权限。可用 `--no-http` 禁用 HTTP，`--no-rpcbind` 关闭 NFS 端口发现。`--json` 将实际就绪端口输出为一行 JSON，日志写到 stderr；端口配置为 `0` 时以实际输出为准。

## GUI（egui）

使用 Release 中的 GUI，或运行自行构建的可执行文件：

```sh
filemux-gui --config filemux.toml
```

GUI 复用 CLI 的配置和 `filemux-core::engine` 服务管理，不会另写一套协议服务；可以使用上面 `init` 生成的配置。打开窗口不会自动启动服务：

1. 选择共享目录；首次使用时检查持久状态目录是否位于共享范围之外。
2. 设置监听地址、端口和 TFTP 上传开关，点击“保存设置”。
3. 点击相应协议的“启动”，查看状态、实际监听地址及日志。

协议名称旁的勾选控制 CLI `serve` 启动时是否启用该协议；本窗口仍通过对应按钮启停。

- 未保存的修改不会影响运行中的服务。
- 已保存但尚未使用的设置会明确显示。存在旧设置下运行的服务时，先“全部停止”，再启动以使用新设置。
- “持久状态与 NFS 端口映射”中可编辑状态位置、端口发现地址和 NFS 导出路径。
- 正常关闭窗口会停止其内部服务；它不管理另一个 CLI 进程，也没有隐含的后台守护进程。
- 日志最多保留 500 条；传输运行在后台，不在 UI 绘制函数中执行文件或网络操作。
- 界面内置中文字体，并启用 eframe 的原生辅助功能支持。

[界面设计预览](https://raw.githubusercontent.com/lona-cn/filemux/master/docs/previews/phase-1.webp) 是开发前生成的设计图，不是运行截图。Windows GUI 已实际运行；Linux/macOS 的 GUI 窗口尚未验证。

## 配置

示例配置见 [examples/filemux.toml](https://github.com/lona-cn/filemux/blob/master/examples/filemux.toml)。`root` 和 `state_dir` 的相对路径按配置文件所在目录解释，而不是启动命令的工作目录。

```toml
root = "share"
state_dir = ".filemux-state"

[http]
enabled = true
bind = "127.0.0.1:8080"
max_connections = 128

[tftp]
enabled = false
bind = "127.0.0.1:69"
allow_upload = false
max_upload_bytes = 1073741824
max_transfers = 32
timeout_ms = 1000
retries = 5
block_size_limit = 1468

[nfs]
enabled = false
bind = "127.0.0.1:2049"
rpcbind = "127.0.0.1:111"
export = "/"
max_connections = 64
max_request_bytes = 1048576
```

若要在配置中关闭端口发现，填写 `rpcbind = false`；省略这一项则采用默认地址。GUI 取消对应选项后也会明确保存为 false。CLI 的 `--no-rpcbind` 只覆盖本次运行，不修改配置文件。

共享状态目录不可放在共享目录内。当前一个状态目录对应一个共享根；切换根目录时选择另一个状态目录，不能覆盖旧身份。

## 协议与端口

| 服务 | 传输 | 说明 |
| --- | --- | --- |
| HTTP | TCP，默认 8080 | GET、HEAD、目录浏览、单段字节 Range；始终只读。 |
| TFTP | UDP，默认 69 | 初始 RRQ/WRQ 使用监听端口；实际传输使用独立、动态的 UDP TID 端口。 |
| NFSv3 / MOUNTv3 | UDP 与 TCP，默认 2049 | 两个 RPC 程序共用配置的 NFS 端口，提供只读文件访问。 |
| portmapper v2 | UDP 与 TCP，默认 111 | NULL、GETPORT、DUMP；不接受远程修改注册信息。 |

默认只监听本机。对局域网提供服务时，把地址改为服务器实际网卡 IP。只在明确接受风险时使用 `0.0.0.0`；程序不自动提权或修改防火墙。

Linux/macOS 的低端口可能需要额外权限。已有系统 rpcbind 或其他程序占用端口时，Filemux 报告绑定失败，不替换或自动注册到现有守护进程。可使用独立地址/主机，或用高端口和显式客户端参数；不要为测试停止未知系统服务。

TFTP 不能只考虑 UDP 69：后续数据走动态端口。防火墙应按应用、可信网段和实际传输需求设置，不要简单关闭防火墙。

### HTTP

文件从已经通过共享目录检查的打开句柄流式读取，不整文件载入内存。目录名称进行 HTML 转义，链接进行 URL 编码。

支持单段 `Range: bytes=0-1023`、开放结束范围和后缀范围，以及不可满足范围的 416。HEAD 不返回响应体。当前为 HTTP/1，没有条件请求验证器；多段/未知范围单位忽略为完整响应，带 `If-Range` 也返回完整响应，而不是冒充拥有强 ETag。

不提供 HTTPS、上传、删除或重命名。目录链接不作为目录浏览；共享范围内的普通文件链接可下载，外部链接及特殊文件拒绝。Windows 无法表示为 UTF-8 的名称和受路径策略限制的名称不会出现在 HTTP 目录链接中。

### TFTP

支持 octet 二进制模式，以及 `blksize`、`timeout`、`tsize`。未知选项（含 `windowsize`）不确认，按普通逐块 ACK 传输；不把 netascii/mail 悄悄当作二进制模式。

上传默认关闭，开启后有大小和并发限制。数据先写入保留的临时文件，完成同步与不覆盖发布后才发送最终成功 ACK。传输被中断时，未发布的临时文件被清理。已成功发布的文件不会仅因最终 ACK 丢失就撤销；客户端遇到网络错误时应检查目标是否已经存在。

`.filemux-` 开头的内部名称不允许远程访问，也不会在目录中列出。上传仅创建已有目录下的新文件，不自动创建任意目录树。

### NFS 与启动用途

服务端不调用宿主机内核 NFS 服务。支持 NFSv3/MOUNTv3，未实现 NFSv2/v4、NLM/statd 或 RPCSEC_GSS。客户端使用 `vers=3`，并禁用网络锁。

MOUNT 可以挂载配置的导出路径及其下的现存目录，适配按文件所在子目录发起挂载的 U-Boot 请求；仍然拒绝越出导出范围的路径。

Linux 标准端口示例：

```sh
sudo mount -t nfs -o ro,vers=3,proto=tcp,mountproto=tcp,nolock,noacl 192.168.1.10:/ /mnt/filemux
```

高端口、不依赖标准端口发现的示例：

```sh
sudo mount -t nfs -o ro,vers=3,proto=tcp,mountproto=tcp,port=12049,mountport=12049,nolock,noacl 192.168.1.10:/ /mnt/filemux
```

macOS 使用 `mount_nfs`，锁选项名为 `nolocks`，例如 `ro,vers=3,tcp,nolocks,port=12049,mountport=12049`。

U-Boot 的网络加载路径可能使用 NFS/UDP，程序因此同时提供 UDP 与 TCP。以下仅展示参数格式；加载地址、内核/DTB 文件及启动命令取决于使用者已有的启动环境：

```text
setenv serverip 192.168.1.10
tftpboot ${kernel_addr_r} Image
nfs ${kernel_addr_r} ${serverip}:/Image
```

Linux NFS root 常见内核参数形式：

```text
root=/dev/nfs ro nfsroot=192.168.1.10:/,vers=3,tcp,nolock ip=dhcp
```

这些命令仅说明参数格式，不是已经通过真机验证的启动脚本。内核需要相应 NFS root 支持。只读 rootfs 的运行时写入需要客户端通过 tmpfs、OverlayFS 或针对只读环境适配；普通发行版只加 `ro` 不保证能启动。

## 文件身份、恢复及平台限制

NFS 句柄包含持久化导出 UUID 和对象 ID，不使用启动时间。状态事务提交后才向客户端发出依赖该身份的句柄。

- 保留原共享根、对象和状态目录时，服务进程重启后旧文件与目录句柄可继续使用；客户端仍须采用合适的重试语义，例如 Linux `hard` 挂载。停机期间允许 I/O 等待，不承诺零中断。
- 在原对象上修改内容不等于创建新对象。删除后同名重建或替换根目录，不能让旧句柄错误读取新对象。
- 部分状态丢失、损坏、根对象替换会明确报错，不静默重建为原身份。人为删除全部状态无法与首次使用区分，会创建新的导出 UUID；它不会恢复旧句柄，也不会让旧句柄访问新对象。因此不要删除或重建使用中的状态目录。
- 当前依赖文件系统提供原生创建时间（Unix 的 birthtime）。缺少该信息时拒绝打开导出，而不是使用可变化的 ctime 冒充 generation。
- Windows 当前要求 NTFS；不支持 FAT 或 ReFS。对外 UID/GID 为 0，普通文件和目录采用虚拟只读可执行权限 `0555`，不是 NTFS ACL 的等价表达；真实文件打开仍受宿主进程权限约束。
- Unix 保留可用的原生权限、UID/GID、链接等元数据。Linux 的锚定普通文件读取依赖 `/proc/self/fd`；不适用于缺少相应 procfs 的隔离环境。
- Windows 普通目录不是完整 POSIX rootfs 的无损存储。路径、大小写、链接目标等仍受宿主平台限制；当前没有多种 rootfs 镜像解析或自动转换工具。
- 目录扫描上限为 10,000 项，超过时返回错误，不静默截断。NFS READ 在 TCP 上最多 1 MiB，UDP 回复受 60 KiB 总量限制；可使用 `rsize=32768`。
- NFS 属性的已用空间按逻辑大小估计，FSSTAT 不提供可用 inode 数量；只读导出的可写容量为零。COMMIT 对有效普通文件执行宿主同步，宿主拒绝刷新只读描述符时返回 I/O 错误。

TFTP 无用户认证，采用 AUTH_SYS 的 NFS 也不验证远程 UID/GID 的真实性。程序不会根据远程 UID 切换宿主身份。它们不提供加密，应仅在可信网络使用，不能当作公网安全文件服务。

## 开发与验证

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo build --workspace --locked
```

测试使用临时目录、本机 TCP/UDP 和空闲高端口，不要求目标板卡、系统 NFS 挂载或管理员权限。进程级测试运行实际 CLI，在结束并重新启动服务后直接复用旧文件和目录句柄；不通过重新挂载获取新句柄来替代恢复验证。

已有 v0.1.0 的 Windows、Linux、macOS 三平台 CI 已全部通过，四平台 release 二进制也已完成构建并发布；这不表示已经创建 0.1.1 二进制 Release。具体执行结果、失败修复和后续动态记录见 [docs/progress.md](https://github.com/lona-cn/filemux/blob/master/docs/progress.md)。这些结果不包含 Linux/macOS GUI 窗口、U-Boot 真机或完整无盘启动验证。

### Workspace 结构

| Crate | 内容 |
| --- | --- |
| `filemux-core` | CLI/GUI 共享库：配置、公共状态、事件、服务生命周期，以及受限文件系统访问、元数据、持久身份、上传发布、HTTP、TFTP、NFSv3/MOUNTv3/portmapper 和服务管理。 |
| `filemux-cli` | `filemux` 命令行程序。 |
| `filemux-gui` | `filemux-gui` egui 桌面程序，包含中文字体及 OFL 许可。 |

Workspace 仅包含这三个 crate。文件系统与协议实现分别位于 `filemux-core` 的 `fs`、`http`、`tftp`、`nfs` 模块；CLI/GUI 通过 `filemux_core::engine::Engine` 共享服务管理。

## 发布维护

### crates.io

crates.io 发布和维护范围仅为 `filemux-core`、`filemux-cli`、`filemux-gui` 三个 crate，版本统一为 **0.1.1**。CLI 使用包名 `filemux-cli` 安装、命令名 `filemux` 运行；GUI 的包名与命令名均为 `filemux-gui`。

`filemux-cli` 和 `filemux-gui` 均直接依赖 `filemux-core`；文件系统、协议和服务管理不再作为独立 crate 发布。发布顺序为 `filemux-core` → CLI / GUI，由 Cargo 的 workspace 发布处理依赖顺序。

发布前应确认版本与内部依赖版本一致、工作区变更已提交、README 和许可证已进入各源码包，并完成上面的开发验证。以下维护命令使用与 CI 一致的 Cargo 1.99.0，在仓库根目录执行：

```sh
cargo publish --workspace --registry crates-io --locked --dry-run
```

首次发布需要直连 crates.io 的 registry。若个人 Cargo 配置存在 `source.*.replace-with` 镜像替换，应使用独立的 `CARGO_HOME` 和配置完成发布；仅指定 `--registry crates-io` 不会取消源替换，可能导致 Cargo 去镜像查找尚未发布的内部依赖。

检查生成的 `.crate` 内容与 dry-run 结果后，已获得发布权限的维护者再执行实际发布：

```sh
cargo publish --workspace --registry crates-io --locked
```

crates.io 版本发布后不可覆盖；后续修改应使用新版本。crates.io 与 GitHub Release 是独立流程，GitHub Release 工作流不会上传 Cargo 包。

### GitHub CI 与 Release

[CI 工作流](https://github.com/lona-cn/filemux/blob/master/.github/workflows/ci.yml) 在分支 push、Pull Request 和手动运行时执行三平台格式检查、完整 workspace 测试及构建。Cargo registry、Git 依赖和编译依赖通过 `Swatinem/rust-cache` 缓存；调试检查与 release 构建分开，并区分系统、架构和发布 target。编译器、依赖清单或 Cargo.lock 变化会影响缓存键；PR 只恢复缓存，不写入。

推送新的 `v<版本号>` tag 会触发 [Release 工作流](https://github.com/lona-cn/filemux/blob/master/.github/workflows/release.yml)。tag 必须与 `Cargo.toml` 的 `workspace.package.version` 完全一致。`v0.1.0` 及其 Release 已存在，**不要重新创建、移动或覆盖既有 tag / Release**。后续版本应先更新并提交 `Cargo.toml`、内部依赖版本和 `Cargo.lock`，推送对应提交，再为该提交创建并推送新的版本 tag。

发布先验证版本并复用完整 CI，全部通过后进行安装章节所列的四套原生 release 构建。全部包齐全后生成 `SHA256SUMS`，先上传到 draft Release，上传成功才公开。`v0.2.0-rc.1` 等预发布版本会标为 prerelease。

已有 Release 不自动覆盖；上传失败留下的 draft 需检查并手动处理后再重跑，避免误改已发布内容。发布使用内置 `GITHUB_TOKEN`，只有最后的发布 job 有 `contents: write`，不需要配置 PAT。

## 许可证

项目源码采用 **MIT OR Apache-2.0** 双许可证，可选择其中任一许可证使用；完整条款见 [LICENSE](https://github.com/lona-cn/filemux/blob/master/LICENSE)。

内置中文字体为未修改的 Noto Sans SC Regular，来自 [Noto CJK 官方字体仓库](https://github.com/notofonts/noto-cjk/tree/main/Sans/SubsetOTF/SC)，单独遵循 **SIL Open Font License 1.1**，条款见 [crates/filemux-gui/assets/fonts/OFL.txt](https://github.com/lona-cn/filemux/blob/master/crates/filemux-gui/assets/fonts/OFL.txt)。源码的双许可证不替代字体许可。
