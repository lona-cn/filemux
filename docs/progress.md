# Filemux 阶段一进度

## 当前状态

- 阶段一需求与验收条件已整理到 `docs/phase-1.md`。
- 阶段一实现及基本验收已完成：8-crate workspace、公共文件访问、持久身份、三种协议、engine、CLI 与 egui GUI 均已实现。Windows 52 项、Linux 54 项测试及两平台完整构建通过，Windows GUI 已完成实际窗口验收。
- GUI 预览已实际生成并检查，路径为 `docs/previews/phase-1.webp`；图片中的示例状态不代表实际服务运行结果。
- 后续继续开发前先核对项目实际状态，保留已有改动，不重复初始化。macOS、Linux GUI 窗口及 CI 的未验证范围见文末。

## 里程碑

| 工作 | 状态 | 完成证据 |
| --- | --- | --- |
| 需求、范围与基本验收说明 | 已整理 | `docs/phase-1.md`。 |
| Workspace、配置与公共文件访问 | Windows、Linux 验收通过 | 8 个 crate；受限目录访问、原生对象身份与 SQLite 状态；覆盖平台链接、特殊文件和替换对象。 |
| NFS UDP/TCP、只读语义与持久句柄 | Windows、Linux 验收通过 | 真实 RPC 测试、跨进程旧句柄恢复、U-Boot READ 参数与子目录 MOUNT。 |
| HTTP、TFTP 与 CLI | Windows、Linux 验收通过 | 真实 HTTP/UDP 请求、上传开关、发布前不发成功 ACK；实际 CLI 进程运行与重启。 |
| 基本单元测试与真实服务集成测试 | Windows 52 项、Linux 54 项通过 | 无 ignored 测试；进程级测试不重新 MOUNT/LOOKUP 即可复用旧文件与目录句柄。 |
| generate_image GUI 预览 | 已完成 | `docs/previews/phase-1.webp`，由 generate_image 生成，已检查关键中文与 egui 可实现性。 |
| egui GUI 与基本运行检查 | Windows 验收通过 | 实际目录选择、保存、配置状态区分、HTTP 启停与下载、只读拒绝、绑定失败及重试、关闭后端口释放；中文与普通/最大化布局已检查。 |
| 三平台构建、测试配置与使用说明 | 已完成 | Windows/Linux/macOS CI、README、配置示例及字体许可已写入；CI 尚未执行，不宣称 macOS 已通过。 |

## 后续使用

按 `README.md` 构建和运行 CLI 或 GUI。阶段一没有遗留的实现项；后续需要 macOS 或设备兼容证据时，再在对应环境执行检查，不把本轮软件级结果扩大为真机启动承诺。

## 实际检查记录

| 命令或检查 | 结果 | 范围 |
| --- | --- | --- |
| `cargo fetch` | 已完成，生成 Cargo.lock 并下载选定依赖。 | 使用现有本机代理，仅对该命令生效，未修改用户 Cargo 配置。 |
| `cargo test --workspace --locked`，Windows x64/MSVC | 52 项通过，0 failed，0 ignored。 | 最终 accesskit 依赖组合也已完整回归；覆盖配置、持久身份、真实 HTTP/TFTP/NFS/portmapper、CLI 进程重启与旧句柄复用。此前 `--nocapture` 也确认 NTFS 已分配的 2/2 短名称分支实际执行。 |
| `cargo build --workspace --locked`，Windows | 通过，CLI 和 GUI 可执行文件已生成。 | 不是 GUI 窗口验收。 |
| `cargo fmt --all -- --check` | 通过。 | 当前 Rust 源码格式。 |
| 独立 CLI 协议探测，Windows | 子目录 MOUNT、标准与 U-Boot READ 均成功；TFTP 不回复 ERROR，超长报文后仍能下载。 | 修复前已实际复现相反结果；修复后通过本机 TCP/UDP 确认，不使用 mock。 |
| 独立 CLI 正常退出检查，WSL Ubuntu 24.04 | 通过，退出码 0。 | 实际启动 HTTP/TFTP/NFS/portmapper，HTTP 下载内容一致；发送 SIGTERM 后输出 stopped 事件，全部 6 个独立 TCP/UDP 监听地址可重新绑定。临时目录及本任务进程均已清理。 |
| `cargo test --workspace --locked --offline --target-dir target/linux`，WSL Ubuntu 24.04 | 54 项通过，0 failed，0 ignored。 | 最终 accesskit 依赖组合完整回归。首次发现 openat2 不接受 O_PATH 与 O_NONBLOCK 组合，按平台修正后普通读取和旧句柄恢复均通过。 |
| `cargo build --workspace --locked --offline --target-dir target/linux`，WSL Ubuntu 24.04 | 通过，CLI 与 GUI 均构建成功。 | 包含最终 accesskit 配置；复用已有工具链和缓存，未安装系统软件。Linux GUI 窗口未运行。 |
| Windows GUI 实际运行 | 通过。 | 实际打开窗口，选择共享与状态目录并保存；从 GUI 启动真实 HTTP 后下载 32 字节文件完全一致，POST 返回 405 且文件未改变；编辑、保存及运行设置状态区分正确；真实端口冲突显示失败，释放后可重试成功。 |
| GUI 停止、关闭与清理 | 通过，已清理本任务临时数据。 | 点击停止及带服务关闭窗口后，原端口均可独占重新绑定；GUI 进程已退出。已删除本任务新建的临时配置和 smoke 目录，释放桌面控制；未覆盖用户已有配置、修改防火墙、提交或推送。 |

后续在此记录实际命令、平台、结果及必要的失败原因。未执行的检查保持未执行，不以静态阅读、mock 或预期结果代替。只保留有助于继续工作的记录，不追加完整聊天或逐条操作日志。

## 实现选择

- 用户已经确定：TFTP 上传默认关闭、面向 U-Boot/无盘启动、NFS 服务重启后不要求重新挂载、只做基本软件级验证。
- 当前采用 SQLite 保存持久身份，8-crate 划分；HTTP 使用 axum/hyper，TFTP 与 NFS 使用有界 Tokio 协议实现。未增加 tar/rootfs 镜像导入。
- 文件身份要求原生创建时间；Windows 使用 NTFS，并提供只读可执行的虚拟 POSIX mode；Linux 的锚定文件读取依赖 `/proc/self/fd`。限制已写入 README，不宣称与完整 POSIX rootfs 等价。
- 状态目录当前对应一个共享根；更换根时选择另一个状态目录。部分状态缺失、损坏和根对象替换会报错；人为删除全部状态无法与首次初始化区分，新导出 UUID 不会让旧句柄错误访问新对象。
- 没有额外的 GUI 预览人工审批要求；先实际生成并检查预览，再开始 GUI 拆解和开发。
- GUI 使用 eframe 官方 accesskit 提供原生辅助功能信息；Windows 基本验收通过真实辅助控件与目录选择框完成，不使用测试专用服务接口。

## 平台与验证范围

- 没有未完成的阶段一实现项或已确认的外部阻塞。
- Windows x64/MSVC、WSL Ubuntu 24.04 已实际完成测试与构建，Windows GUI 已实际验收。首次远端三平台 CI 已执行，结果及 macOS 测试修复见下方记录；Linux/macOS GUI 窗口尚未运行。
- 未进行 U-Boot 真机、完整 Linux 无盘启动、系统 NFS 挂载或硬件故障测试；这些不是本阶段交付门槛。无需重新挂载的基本证据是实际进程重启后，直接用旧文件与目录句柄通过 TCP/UDP 继续访问，未重新 MOUNT/LOOKUP 获取替代句柄。

## 后续改动：CI 缓存与版本 tag 发布

- `ci.yml` 已明确调试构建缓存的系统、架构与工作目录，PR 只恢复缓存；保留三平台完整检查，增加可复用入口，避免 tag 单独触发一套重复的普通 CI。
- `release.yml` 在 `v<workspace 版本>` tag 上复用完整 CI，再原生构建 Windows x64、Linux x64、macOS Intel 与 Apple Silicon；release 缓存按目标分开。
- `.github/scripts/release.py` 校验 tag、打包真实 CLI/GUI 和说明/字体许可，并要求四包齐全后生成 SHA256SUMS；上传 draft 成功后才公开，预发布 tag 标记 prerelease，只有发布 job 有写权限，不覆盖已有 Release。
- 用户已删除的 `rust-toolchain.toml` 未恢复；README 已改为说明现有 CI/发布使用的版本，并补充 tag 发布步骤、资产范围及未签名限制。

| 本次检查 | 实际结果 |
| --- | --- |
| Actionlint 1.7.12 | `ci.yml`、`release.yml` 均通过；校验工具来自官方 Release，执行前验证下载 SHA-256，未安装到系统。 |
| Windows / Linux release 构建 | 两个平台的真实 CLI 与 GUI 均构建成功，使用 `--release --locked --target`。Linux 为现有 WSL Ubuntu 环境，不代替 GitHub 的 Ubuntu 22.04 runner 验证。 |
| ZIP / TAR.GZ 打包与解包 | 均通过；二进制、README、配置示例、阶段文档、预览及字体许可齐全，Unix 二进制保留执行权限；解压后的两平台 CLI 均输出正确版本。 |
| 解压后运行 | Windows 真实 HTTP 下载返回 200 且内容一致；Linux 真实 HTTP 下载、SIGTERM 正常退出及全部 6 个监听地址释放通过。 |
| 发布脚本边界 | tag 不匹配、缺二进制、覆盖已有包、缺目标包均被拒绝；隔离清单数据验证了 SHA-256 内容、排序及额外资产拒绝；隔离版本文件验证了 prerelease 判断。未用这些数据冒充 macOS 构建。 |
| 清理与未执行范围 | 临时压缩包、解压目录、校验工具及本任务进程已清理。未创建/推送 tag、未提交、未发布 Release；GitHub 实际缓存命中、远端发布及 macOS release 构建尚未执行。Rust 服务源码未改动，本次未重复运行阶段一整套 Rust 测试。 |

## 首次 CI 失败修复：macOS 超长 UDP 测试

- 失败记录：[Rust #40276978551](https://github.com/lona-cn/filemux/actions/runs/40276978551)，提交 `00d03c0`。Windows/Linux job 通过，macOS 唯一失败项为 `oversized_request_is_rejected_without_stopping_the_listener`。
- 原测试构造约 60 KB 的 UDP 请求，macOS 客户端在 `send_to` 就返回 `Message too long`（OS error 40），报文没有到达服务器。不是服务端处理请求时出错，不能靠重跑、跳过测试或放宽服务端限制解决。
- 测试改为 4096 字节选项值，加协议字段后总长 4118 字节，仍超过服务端 4096 字节限制；保留 ERROR 4、响应来自监听端口、随后下载内容正确等断言。未改变服务端、CI 矩阵或测试启用条件。
- 修复后本地检查：`cargo fmt --all -- --check` 通过；Windows 与 WSL Linux 的 `cargo test -p filemux-tftp --test udp --locked` 各 12 项通过、0 failed、0 ignored（Linux 使用原有离线缓存和 `target/linux`）。
- 另以实际 release CLI 通过本机 UDP 发送同样的 4118 字节报文，收到 ERROR 4，随后成功下载原文件，内容完全一致；临时目录和进程均已清理。
- 本机没有 macOS 环境，修复后的 macOS 全套结果需以该修复提交的 GitHub CI 为准，不将 Windows/Linux 本地结果冒充 macOS 复验。
