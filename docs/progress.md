# Filemux 阶段一进度

## 当前状态

- 阶段一需求与验收条件已整理到 `docs/phase-1.md`。
- 阶段一功能与三 crate 合并已完成，workspace 仅含 `filemux-core`、`filemux-cli`、`filemux-gui`。GUI 已按“服务 / 设置 / 日志”分开布局并支持双语，HTTP 已支持独立启用上传；0.2.0 发布时三个 crate 的版本统一为 `0.2.0`。
- 初版预览为 `docs/previews/phase-1.webp`。新版先生成并检查 `docs/previews/gui-layout-services.webp` 和 `docs/previews/gui-layout-nfs-settings.webp`，用户确认后才开始拆解与实现；图片示例不是实际服务结果。
- 后续继续开发前先核对项目实际状态，保留已有改动，不重复初始化。Linux/macOS GUI 窗口及硬件启动的未验证范围见文末。
- 下列原阶段一的 GUI 验收记录对应旧布局，不能代替新版交互验收。新版改动与实际验证范围见文末。
- GUI 双语与独立语言偏好已实现，Windows/Linux 完整测试及构建通过。README 已精简并完成英文全文、中文全文和顶部语言跳转，接入用户提供的服务页与设置页截图，不使用日志页截图。

## 原阶段一里程碑

| 工作 | 状态 | 完成证据 |
| --- | --- | --- |
| 需求、范围与基本验收说明 | 已整理 | `docs/phase-1.md`。 |
| Workspace、配置与公共文件访问 | Windows、Linux 验收通过 | 3 个 crate；受限目录访问、原生对象身份与 SQLite 状态；覆盖平台链接、特殊文件和替换对象。 |
| NFS UDP/TCP、只读语义与持久句柄 | Windows、Linux 验收通过 | 真实 RPC 测试、跨进程旧句柄恢复、U-Boot READ 参数与子目录 MOUNT。 |
| HTTP、TFTP 与 CLI | Windows、Linux 验收通过 | 真实 HTTP/UDP 请求、上传开关、发布前不发成功 ACK；实际 CLI 进程运行与重启。 |
| 基本单元测试与真实服务集成测试 | Windows 52 项、Linux 54 项通过 | 无 ignored 测试；进程级测试不重新 MOUNT/LOOKUP 即可复用旧文件与目录句柄。 |
| generate_image GUI 预览 | 已完成 | `docs/previews/phase-1.webp`，由 generate_image 生成，已检查关键中文与 egui 可实现性。 |
| egui GUI 与基本运行检查 | Windows 验收通过 | 实际目录选择、保存、配置状态区分、HTTP 启停与下载、只读拒绝、绑定失败及重试、关闭后端口释放；中文与普通/最大化布局已检查。 |
| 三平台构建、测试配置与使用说明 | 已完成 | 0.1.0 三平台 CI 及四平台 Release 已完成；三 crate 改造已通过本机 Windows/Linux 测试与构建，macOS 以改造提交的远端 CI 为准。 |

## 后续使用

按 `README.md` 构建和运行 CLI 或 GUI。原阶段一没有遗留实现项；新版 GUI 尚未验收的交互及各平台限制见文末，不把旧布局的验收或软件级结果扩大为新版交互、真机启动承诺。

## 原阶段一实际检查记录

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
- 当前采用 SQLite 保存持久身份，仅保留 core、cli、gui 三个 crate；文件访问、三种协议和服务管理是 core 内部模块。HTTP 使用 axum/hyper，TFTP 与 NFS 使用有界 Tokio 实现。未增加 tar/rootfs 镜像导入。
- 文件身份要求原生创建时间；Windows 使用 NTFS，并提供只读可执行的虚拟 POSIX mode；Linux 的锚定文件读取依赖 `/proc/self/fd`。限制已写入 README，不宣称与完整 POSIX rootfs 等价。
- 状态目录当前对应一个共享根；更换根时选择另一个状态目录。部分状态缺失、损坏和根对象替换会报错；人为删除全部状态无法与首次初始化区分，新导出 UUID 不会让旧句柄错误访问新对象。
- 没有额外的 GUI 预览人工审批要求；先实际生成并检查预览，再开始 GUI 拆解和开发。
- GUI 使用 eframe 官方 accesskit 提供原生辅助功能信息；Windows 基本验收通过真实辅助控件与目录选择框完成，不使用测试专用服务接口。

## 平台与验证范围

- 原阶段一没有遗留实现项。新版 GUI 受本次窗口操作授权限制，交互验收未执行，详见文末。
- 原阶段一已完成 Windows x64/MSVC、WSL Ubuntu 24.04 测试与构建，以及旧布局的 Windows GUI 验收；既有三平台 CI、四平台 Release 的结果不代替本次改动检查。Linux/macOS GUI 窗口尚未运行。
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
- 修复提交 `b44833b` 的远端 [Rust #40279460896](https://github.com/lona-cn/filemux/actions/runs/40279460896) 已完成，Windows、Linux、macOS 全部通过；macOS 结果来自远端 runner，不是本机模拟。

## v0.1.0 GitHub Release

- `v0.1.0` 指向 `b44833b`。[Release #40280255603](https://github.com/lona-cn/filemux/actions/runs/40280255603) 的版本检查、三平台完整 CI、四平台原生构建与发布共 9 个 job 全部成功。
- [公开 Release](https://github.com/lona-cn/filemux/releases/tag/v0.1.0) 包含 Windows x64、Linux x64、macOS Intel、macOS Apple Silicon 四个压缩包及 `SHA256SUMS`，不是 draft 或 prerelease。既有 tag 和 Release 不因后续 crates.io 发布准备而移动或覆盖。

## 0.1.0 旧拆分发布记录（已停止）

- 初次方案曾计划发布 core、fs、http、tftp、nfs、engine、cli 共 7 个 crate，GUI 不发布。用户随后明确只保留 core、cli、gui；旧发布计划已停止，不能继续按本节历史命令发布旧拆分。
- 源码采用用户确认的 `MIT OR Apache-2.0`，根 `LICENSE` 包含两份完整条款。Noto 字体仍单独使用 SIL OFL。后续 GitHub 二进制打包也加入源码 `LICENSE`，不改已发布资产。
- README 已按安装、快速开始、GUI、配置、协议与平台限制、开发及发布维护重新组织，并增加 `cargo install filemux-cli --version 0.1.0 --locked`、PATH、编译环境与许可证说明。

| 上传前检查 | 实际结果 |
| --- | --- |
| `cargo test --workspace --locked`，Windows | 52 项通过，0 failed、0 ignored。未修改 Rust 服务源码。 |
| `cargo fmt --all -- --check` | 通过。 |
| 7-crate workspace `cargo publish --dry-run --locked --allow-dirty` | 全部完成打包、解包构建验证；没有使用 `--no-verify`。首次受个人 rsproxy 源替换阻断，改用临时 `CARGO_HOME` 直连官方 registry 后通过，未修改全局配置。 |
| 源码包内容 | 7 包均附带 README、LICENSE、Cargo.lock；README 与 LICENSE 的 SHA-256 分别与原文件一致，规范化依赖清单使用 registry 版本而非本地路径。 |
| `cargo install --path crates/filemux-cli --locked` | 安装到本任务隔离目录，`filemux --version` 为 `0.1.0`，帮助、初始化与配置检查通过。 |
| 安装后真实服务检查 | HTTP 返回 200 且内容一致；TFTP 下载内容一致、默认拒绝上传；NFS 与 portmapper 的 TCP/UDP RPC NULL 均成功。进程结束后全部 6 个监听地址可重新绑定。 |
| Windows Release 打包资源 | 用已有真实 CLI/GUI 二进制运行打包脚本，ZIP 同时包含源码 LICENSE 与字体 OFL。只生成本地临时包，没有替换远端 Release。 |

`license` 保留 SPDX 表达式，`license-file` 用于将共享完整条款自动带入源码包；Cargo 对同时声明两项给出冗余提示，但构建验证成功。旧方案最终只发布了 core、fs、http、tftp、nfs、engine 的 0.1.0，CLI 未发布；遇到过 index 同步超时、服务端 500 及新 crate 频率限制，均按官方 API 的实际状态判断，没有把 dry-run 算作发布。

## 0.1.1 三 crate 改造

- 按用户要求，仅保留 `filemux-core`、`filemux-cli`、`filemux-gui`。原文件访问、HTTP、TFTP、NFS 和服务管理代码分别迁入 core 的 `fs`、`http`、`tftp`、`nfs`、`engine` 模块，旧源码与 Cargo 清单已移除，不保留转发 crate。
- CLI/GUI 直接使用 `filemux_core::engine`。已有行为测试全部迁移，未删除断言或跳过测试。
- 中文字体和 OFL 移入 GUI crate，`include_bytes!` 不再引用 crate 外文件；GUI 现在与 core、CLI 一起发布。版本统一为 0.1.1，不覆盖已发布的 core 0.1.0，也不移动 GitHub 的旧 tag。
- README、阶段说明、依赖清单、Cargo.lock、源码许可证中的字体路径以及 GitHub 打包资源路径均已同步。

| 合并后检查 | 实际结果 |
| --- | --- |
| Windows `cargo test --workspace --locked`、`cargo build --workspace --locked` | 52 项测试通过，0 failed、0 ignored；CLI/GUI 均构建成功。 |
| WSL Ubuntu `cargo test/build --workspace --locked --offline --target-dir target/linux` | 54 项测试通过，0 failed、0 ignored；CLI/GUI 均构建成功。复用现有环境，未安装系统组件。 |
| 格式 | 已执行 `cargo fmt --all`。 |
| 实际 CLI 0.1.1 | 初始化、检查配置及真实 HTTP/TFTP 下载成功；默认拒绝上传；NFS/portmapper 的 TCP/UDP RPC NULL 成功，进程与临时数据已清理。 |
| 三包 `cargo publish --workspace --registry crates-io --locked --dry-run --allow-dirty` | core、CLI、GUI 均完成实际解包构建，没有使用 `--no-verify`。 |
| 源码包与字体 | 三包 README/LICENSE 的 SHA-256 与原文一致；CLI/GUI 唯一内部依赖均为 core。GUI 压缩包约 6.9 MiB，包含原字体和 OFL，字体 SHA-256 与原文件一致。 |

上述记录为 0.1.1 正式上传前检查。core、CLI、GUI 的 0.1.1 均已正式发布，本次已通过 crates.io 版本索引确认。用户接管了 fs、http、tftp、nfs、engine 五个多余包的手动删除，本任务不再代为删除或 yank，也不将其删除标为已完成。

## GUI 分页重做（0.1.2）

### 预览与实现

- 用户确认的两张 generate_image 预览先于本次拆解和开发：`docs/previews/gui-layout-services.webp`、`docs/previews/gui-layout-nfs-settings.webp`。保留这些预览，不将其中示例状态当作运行证据。
- `main.rs` 改为固定的“服务 / 设置 / 日志”导航，窗口默认 1040×680、最小 900×600 逻辑尺寸。服务页只展示共享目录、真实状态、监听地址、权限和启停操作；不再混入编辑表单、CLI 勾选与日志控制台。
- 新建 GUI 内部 `settings.rs`，按“共享与状态 / HTTP / TFTP / NFSv3”分组。持久状态留在通用设置，RPC 端口发现留在 NFSv3，TFTP 上传只在 TFTP 设置中编辑；CLI 默认启用协议移入通用设置的折叠区。
- 保存和放弃修改按钮固定在设置页底部；切页保留草稿。首次未保存不再显示成“修改未保存”；已保存设置与运行中使用的设置分别记录。保存不重启服务，运行目录仍显示实际使用的旧目录；全部停止、再启动后才使用新设置。
- 日志独立显示，按协议及错误筛选，保留最近 500 条；服务错误提供短原因与对应协议日志入口。完整错误可选取复制，记录被滚出列表后仍可查看最近一次服务错误。
- `worker.rs` 分开跟踪启停、保存、目录选择，后两者独立运行，不用一个 busy 状态禁用停止操作。保存中关闭也会先请求停止服务，再等待原子保存完成；保存失败则保留窗口和草稿，不丢弃错误。关闭不等待用户关闭目录选择框。
- 保存错误与服务状态分别显示。复核曾实际复现“保存失败后点击全部停止，错误被‘所有服务已停止’覆盖”；对应回归修复前失败，修复后通过。服务成功状态不再写入设置错误提示，也不再创建未显示的成功提示字符串。
- workspace 仍为三个 crate，未增加 GUI 框架、服务实现、测试专用启动接口或系统依赖。`filemux-gui` 仅增加复用已有 `tempfile` 的开发依赖。

### 实际验证

| 检查 | 实际结果 |
| --- | --- |
| Windows 初轮完整 `cargo test --workspace --locked --offline` | 55 项通过，0 failed、0 ignored。此时 GUI 有 3 项测试；随后补充的端口冲突及保存错误回归另行验证如下。 |
| WSL Ubuntu 24.04 初轮完整 workspace 测试 | 57 项通过，0 failed、0 ignored；使用现有 Rust、缓存及 `target/linux`，未安装系统组件。 |
| 最终 `cargo test -p filemux-gui --locked --offline` | Windows、WSL Linux 各 5 项通过、无跳过：配置状态区分、日志筛选、保存新根不改变运行中的真实 HTTP、NFS RPC 冲突后重试且 HTTP 持续可用、保存失败后停止服务仍保留错误和原配置。后三项使用真实文件操作及本机网络，不用 mock。 |
| Windows GUI 构建 | `cargo build -p filemux-gui --locked --offline --target x86_64-pc-windows-msvc` 通过；使用独立 target 路径，不覆盖可能由用户运行的默认构建。 |
| Linux GUI 构建 | WSL 中 `cargo build -p filemux-gui --locked --offline --target-dir target/linux` 通过；不是 Linux 窗口验收。 |
| 格式与编译警告 | `cargo fmt --all -- --check` 通过；本次新增的 6 处 `f32` 推断警告已通过明确类型修正，最终构建不再出现。既有 SPDX 与 `license-file` 同时声明的清单提示未在 GUI 任务中改动。 |
| 独立 CLI 真实网络检查，Windows | 临时配置启动 HTTP/TFTP/NFS；HTTP GET 200 且内容一致、POST 405，TFTP 下载内容一致且默认上传返回 ERROR 2；NFSv3、MOUNTv3、portmapper v2 的 TCP/UDP RPC NULL 共 6 次均成功。 |
| 实际进程退出与监听释放 | 上述 CLI 接收 `CTRL_BREAK_EVENT` 后正常退出、退出码 0；6 个 TCP/UDP 监听地址均可重新绑定。GUI 后台回归也验证停止后 HTTP 不再接受连接。 |
| Windows GUI 只读启动 | 实际启动新原生可执行文件，辅助功能树包含“服务 / 设置 / 日志”、共享目录、三服务的已停止状态、实际监听与权限列及启停按钮；没有执行任何点击、输入、拖动或聚焦操作。截图被其他窗口遮挡，未将其算作视觉验收。 |

### 尚未验证及交付范围

- 上次交付未获窗口操作授权，未执行页面点击、输入与关闭交互检查。用户本轮明确反馈已亲自测试该版 GUI、没有问题；此处记录用户手动验证结果，不冒充自动执行。新增语言功能的本次验证另见下节。
- Linux/macOS GUI 窗口未运行；本次未进行 macOS 编译、远端 CI 或硬件启动验证，不沿用旧 CI 结论宣称本次已通过。
- 本次不改版本、不提交、不推送、不发布到 crates.io/GitHub Release。新版目前只在工作区源码中。
- 本任务测试窗口、CLI 进程、临时配置、测试目录及只读检查生成的截图均已清理；未操作已有用户实例、配置、防火墙或系统服务，未取得或使用 GUI 输入控制权限。


## README 精简与 GUI 双语（0.1.2）

### 已实现

- README 先精简中文稿，再完成英文全文在前、中文全文在后的排版；顶部提供语言跳转。保留安装、快速开始、GUI/NFS 用法、安全限制、三个 crate、开发命令及许可，不再放发布流水、历史测试过程与重复说明。
- 用户实际截图保存为 `docs/screenshots/services.png`、`docs/screenshots/settings.png`，与原图 SHA-256 一致，未修图或改变服务状态。服务页作为主图，设置页默认折叠；中文正文链接到共享截图区，不重复放图。按用户要求不接入日志页截图，也不使用生成预览或占位图。
- GUI 新增 English / 简体中文，启动时按系统界面语言选择：中文主语言使用简中，其他或不可获取的语言使用 English。系统检测使用 [sys-locale 0.3.2](https://docs.rs/sys-locale/0.3.2/sys_locale/)，不修改系统语言。
- “设置 → 通用 → 界面语言”可选择跟随系统、English、简体中文；选择立即作用于界面，不修改表单或服务配置，不产生未保存的服务参数，也不重启服务。
- 语言偏好通过 eframe 持久化保存到配置目录的 `.filemux-gui.ron`，与 CLI 共用的 TOML 分开；保留原窗口行为，不新增窗口位置或其他界面状态的保存。
- 导航、状态、按钮、表单、提示、关闭确认、已知错误说明与文件夹对话框自定义标题均提供双语。通知保留静态双语说明，切换后重新选择文本，不冻结旧语言。原始服务日志、完整底层诊断、路径和用户数据保留原文。
- 只增加 GUI 内部 `i18n.rs`、`sys-locale` 依赖及 eframe 的 persistence 功能；workspace 仍为三个 crate。core/CLI 与现有服务配置格式未改动，原有服务与文件访问行为继续由真实网络测试验证。

### 实际验证

| 检查 | 实际结果 |
| --- | --- |
| Windows 完整 workspace 测试 | `cargo test --workspace --locked`：60 项通过，0 failed、0 ignored；含 8 项 GUI/语言测试与真实本机网络回归。 |
| WSL Ubuntu 24.04 完整 workspace 测试 | `cargo test --workspace --locked --offline --target-dir target/linux`：62 项通过，0 failed、0 ignored。 |
| 两平台 GUI 构建及格式 | Windows MSVC 独立 target 与 WSL Linux GUI 构建成功；`cargo fmt --all -- --check` 通过。非测试构建所需的 SocketAddr 导入已修正；既有许可证清单提示保留。 |
| 真实系统语言检测 | Windows 返回 `en-US`，自动选择 English，保存选择 `zh-CN` 后解析为中文；Linux 独立进程分别使用中文、英文、日文语言环境，确认自动选择、非中文回退以及手动覆盖。未改系统设置或全局环境。 |
| Windows 原生窗口只读检查 | 标准启动方式、默认渲染配置下，自动模式窗口的导航/状态/动作文字为英文；加载已保存 `zh-CN` 偏好后，英文系统上的新窗口显示中文。读取实际辅助功能树，不以源码字符串代替窗口结果。 |
| 独立偏好持久化与配置兼容 | 实际 GUI 自动保存生成 `.filemux-gui.ron`，内容为 `filemux.language = auto` 对应的 RON 数据；加载中文偏好后仍保留 `zh-CN`。服务 TOML 内容保持不变；精简 README 的实际 CLI 初始化、配置检查成功。 |
| 双语 README 与截图 | 使用现有 Bun Markdown 渲染并在本机 Chromium 检查：仅两张图片，均完整加载；设置图默认折叠、可展开；两个语言入口实际跳转成功，英文全文在中文全文之前；无横向溢出，本地引用均存在，两种语言的 6 组命令示例一致。 |

### 范围与待办

- 上述 GUI 验证遵循用户不操作窗口的选择，未点击语言下拉框或操作其他界面控件；使用系统 API、存储、真实窗口文字及单元/网络测试验证，不宣称完成新控件的手动交互或视觉布局验收。
- Linux/macOS 图形窗口、本次 macOS 编译和远端 CI 未执行；既有平台结果不替代本次检查。
- 此前 GUI 验证创建的测试实例、诊断实例、临时配置、语言探测程序及测试目录已清理，未操作用户现有实例、配置、系统服务或防火墙，未自行采集 GUI 截图；README 图片由用户提供。
- README 图片及双语排版已完成，无待接入截图。上一轮补图仅修改文档与图片，未改动 Rust 源码或重复运行其测试，也未提交、推送或发布；本节保留该轮验证范围，后续发布前检查见下节。

## 0.1.2 发布前检查

- 用户已授权提交、推送到 `lona-cn/filemux` 并发布 crates.io。仅发布 core、CLI、GUI 三个 crate，不创建新 tag、不移动旧 tag，也不覆盖已有 GitHub Release。
- crates.io 已存在三个包的 0.1.1，不能覆盖；workspace 版本与 CLI/GUI 的 core 依赖要求统一提升为 0.1.2，并同步 Cargo.lock。
- 本次包含新版 GUI 分页、English / 简体中文、独立语言偏好、双语 README、用户提供的服务页与设置页截图及已确认的设计预览。
- 本节记录提交与上传前的检查；实际提交、推送和发布状态以 GitHub 与 crates.io 的公开记录为准。

| 发布前检查 | 实际结果 |
| --- | --- |
| 源码格式 | `cargo fmt --all -- --check` 通过。 |
| Windows workspace 测试 | `cargo test --workspace --offline`：60 项通过，0 failed、0 ignored；包含真实本机网络集成测试。 |
| 实际 CLI / GUI 程序 | 通过 `cargo run --locked --offline` 分别运行版本命令，输出 `filemux 0.1.2` 与 `filemux-gui 0.1.2`；GUI 版本命令不打开窗口。 |
| 发布顺序 | 批量 dry-run 在打包 CLI 时因 `rsproxy-sparse` 尚无 core 0.1.2 而停止，未上传任何包。改为 core → CLI → GUI，正式发布保留 Cargo 的解包构建校验，不使用 `--no-verify`。 |
| core 发布包预检 | `cargo publish -p filemux-core --registry crates-io --locked --dry-run --allow-dirty` 完成真实解包构建，成功后按 dry-run 中止上传；CLI/GUI 在 core 公开后分别执行正式发布校验，不把此前批量预检记为通过。 |

## HTTP 上传（0.2.0）

### 已实现

- HTTP 新增独立的 `allow_upload` 与 `max_upload_bytes` 配置，默认关闭上传、单文件上限 1 GiB；旧配置缺少新字段时仍可加载。TFTP 原有开关不改变 HTTP 权限，NFS 仍只读。
- `PUT /path/file` 流式接收原始文件内容，支持空文件和 chunked 请求，成功返回 `201` 与规范编码的 `Location`。父目录必须已经存在，不提供覆盖、分段或断点续传。
- HTTP 目录页在允许上传时显示单文件表单；multipart POST 只接受一个名为 `file`、具有合法文件名的 part，成功后以 `303` 返回当前目录。拒绝额外 part 后才发布文件；请求体总上限为单文件上限加 64 KiB。
- 复用公共文件层的临时文件和无覆盖发布；已有目标返回 `409`，超限返回 `413`。路径、保留名和跨站写入检查不会因开启上传而取消；断连和停服不会发布未完成的文件。
- CLI 增加 `--allow-http-upload`；GUI 增加双语 HTTP 上传开关、单文件上限说明，服务页按各协议的已保存或正在使用的配置显示权限。README 中英文、配置示例和阶段说明已同步。

### 实际验证

| 检查 | 实际结果 |
| --- | --- |
| Windows workspace 测试 | `cargo test --workspace`：66 项通过，0 failed、0 ignored；包含 6 项新增上传网络回归。此命令同时更新 multipart 所需的 Cargo.lock 依赖。 |
| 最终 HTTP 回归与格式 | 保留原测试中 `enabled = false` 也可显式启动服务的检查后，`cargo fmt --all -- --check` 和 `cargo test -p filemux-core --test http --locked` 通过；HTTP 共 10 项通过。 |
| 实际程序构建 | `cargo build --workspace --locked` 通过，生成 CLI 和 GUI；保留原有 `license` / `license-file` 清单提示。 |
| 独立 CLI 进程 | 通过 `--allow-http-upload` 启动，PUT 返回 `201`、下载内容完全一致、重复上传返回 `409` 且原文件不变；3 MiB multipart 上传返回 `303`，落盘内容一致，不受 axum 默认 2 MiB 限制。配置文件没有被运行参数改写。 |
| 真实浏览器表单 | Chromium 实际选择文件、提交中文子目录的表单，观察到 POST `303` → GET `200`；目录出现上传文件，落盘内容与源文件一致，并检查实际页面截图。 |
| CLI 参数与 curl | 实际 `serve --help` 包含两个独立开关；`curl.exe --upload-file` 成功，文件内容一致。只传 TFTP 的 `--allow-upload` 时，HTTP PUT 仍返回 `405`、`Allow: GET, HEAD`，目录无上传表单，未创建文件。 |
| 原生 GUI 只读检查 | 实际打开隔离配置的两个窗口，辅助功能树分别显示 HTTP 的 `Download only` 和 `Upload enabled`，并保持“已保存、尚未运行”状态。未操作用户已有窗口。 |
| 清理 | 本任务 CLI、GUI 实例及浏览器页已停止或关闭；两个 CLI 监听端口均可重新绑定。隔离 smoke 文件及目录已清理，未修改用户配置、系统服务或防火墙。 |

桌面输入控制首次调用即返回 `Computer session is closed`，因此未点击新复选框或验证设置页保存交互；只读窗口文字检查不算交互或视觉布局验收。本轮未运行 Linux/macOS、远端 CI 或硬件检查，未提交、推送或发布。

## 0.2.0 发布前检查

- workspace 版本及 CLI、GUI 的 core 依赖要求统一升级到 `0.2.0`，Cargo.lock 已同步；本版包含上述 HTTP 上传功能，不覆盖旧版本。
- Windows 执行 `cargo test --workspace --offline`：66 项通过，0 failed、0 ignored；`cargo fmt --all -- --check`、`cargo build --workspace --locked --offline` 通过。实际 CLI 与 GUI 版本命令分别输出 `filemux 0.2.0`、`filemux-gui 0.2.0`。
- `cargo publish -p filemux-core --registry crates-io --locked --dry-run --allow-dirty` 完成真实打包和解包构建校验，按 dry-run 中止上传；三包文件清单均包含 README、LICENSE、Cargo.lock，GUI 另含原中文字体与 OFL。
- 批量 dry-run 在 CLI 解析 `filemux-core ^0.2.0` 时停止：registry 中尚无该版本。正式发布按 core → CLI → GUI 进行，下游包在 core 可用后执行解包构建，不使用 `--no-verify`。
- 本节记录提交和上传前的本机检查；提交、推送、crates.io 发布及远端 CI 的最终结果以实际远程记录为准。不创建新 tag 或 GitHub Release。

## PXE 菜单、工具下载与可选 ProxyDHCP

### 已实现

- 新增默认关闭的 `pxe` 配置，旧 TOML 继续可用；支持外部 DHCP 和 ProxyDHCP。菜单包含 Linux、Windows PE、本地 iPXE 脚本，支持排序、默认项与人工等待；检查资源不创建 state。
- HTTP 使用现有监听器提供 `/.filemux-pxe/menu.ipxe`，支持 GET／HEAD、禁止写入、禁用缓存；共享资源继续经过原文件访问限制。标题和参数通过 iPXE hex setting 传入，资源 URL 按组件编码。
- 可主动下载固定官方 iPXE 2.0.0、wimboot 2.9.0，校验 HTTPS 和固定 SHA-256；限制下载与解压大小，只提取两个指定常规文件，复用无覆盖发布。保留已有文件，失败／取消清理临时文件；不附带二进制、不下载 Windows 镜像。
- ProxyDHCP 实现 Windows、Linux、macOS 接口收发路径，监听 UDP 67／4011，按接收接口筛选；只提供启动信息，不分配租约，不响应其他 DHCP 的地址请求、未知架构或 relay。HTTP／TFTP 是启动依赖，停止与健康检查会维护这些限制。
- GUI 新增双语 PXE 页、独立后台文件任务、ProxyDHCP 服务行与日志筛选；保存不会更改正在运行的服务。README 与配置示例已更新。

### 实际验证与限制

| 检查 | 实际结果 |
| --- | --- |
| Windows 测试与构建 | `cargo test --workspace --locked`：80 项通过。之后仅重跑受影响的 GUI 8 项、ProxyDHCP 2 项，均通过；最终 `cargo build --workspace --locked` 通过。保留现有 license/license-file 清单提示。 |
| Linux 测试 | WSL Ubuntu 24.04 执行计划中的 workspace 命令。首轮发现新增测试错误地假定共享根内目录链接一律禁止；删除该实现细节断言，补充根外链接检查，不修改现有文件访问规则。受影响测试与此前尚未运行的所有 suite 分别通过，最终 82 个测试项通过，doc-tests 为 0 项。未重复全量运行。 |
| 固定官方下载 | 真实 CLI 成功从官方 Release 下载两个 iPXE 文件和 wimboot；再次下载报告“已存在，未覆盖”，文件摘要未改变。Linux 复用这些已验证文件，未重复联网下载。 |
| Windows CLI 协议 | 首轮真实标准端口 smoke 通过：外部 DHCP 菜单 GET／HEAD、资源字节比对、TFTP 两种 iPXE 文件传输、67／4011 BIOS／UEFI／iPXE OFFER／ACK、无租约选项、其他 server identifier 请求不回复、后续合法请求仍回复、正常退出与绑定失败回滚。均为 loopback 软件客户端，不是物理广播或完整开机。最终独占绑定后的标准端口限制见下文。 |
| Linux CLI 协议 | 原生 WSL 临时目录上运行 Linux 自身 CLI，HTTP 18080、TFTP 69、ProxyDHCP 67／4011 的同等检查全部通过；SIGTERM 后正常退出，各监听可重绑，真实端口冲突时回滚。最初 `/mnt/e` 的 DrvFS 共享根缺少所需创建时间，程序明确拒绝；随后仅将 smoke share/state 改至 WSL 原生临时目录，没有放宽身份检查。 |
| Windows socket 修正 | 实测 Winsock 克隆句柄上的 SO_RCVTIMEO／SO_BROADCAST 没有同步到原句柄。Windows 改为借用原 socket 设置选项，高端口真实 UDP 测试验证收发、超长／畸形报文后继续服务、200ms 取消检查和停止后重绑。Linux／macOS 保留共享句柄方式。 |
| Windows 同机 DHCP 冲突 | 后续检查发现 `SharedAccess`（PID 4144）占用 `172.21.160.1:67`。最终 Windows 增加 SO_EXCLUSIVEADDRUSE，防止与同机 DHCP 共用端口；实际 CLI 启动明确报 10048，HTTP／TFTP 回滚，18080／69／4011 可重新绑定。没有停止系统服务；最终独占版本的 Windows 标准端口代理收发受此占用限制，不能把首轮结果写成最终版本再次通过。最终代理收发由 Windows 随机高端口测试验证。 |
| Windows GUI | 真实窗口 AX 操作验证模式切换、检查与旧结果清空、主动下载保留现有文件、菜单新增／删除／上下移／默认项保存、运行中保存仍保留原设置、先启动依赖再启动代理、代理运行时禁用依赖停止、实际两个 UDP 端点、ProxyDHCP 日志筛选及双语切换。通过正常关闭验证进程退出。服务列已按真实截图修正宽度，ProxyDHCP 名称与中文访问权限不再断行。 |
| GUI 视觉范围 | 实际查看服务页及 PXE 设置顶部的普通／最大化窗口。后台滚轮没有滚动页面；前台控制请求未获答复，键盘回退被 Windows 拒绝。因此菜单下部与检查结果的滚动布局没有完成视觉验收；相关控件已通过 AX 操作，不以此替代滚动布局截图。 |
| 超时编辑框可访问名称 | GUI 审查发现超时数值没有关联可见标签；已沿用端口编辑框的 `.labelled_by(label.id)` 修复，最终 GUI 构建通过。桌面会话关闭后，JS／Python 工具只返回 undefined／None，重建 Eval 上下文也未恢复，已报告工具问题；新标签的原生 AX 复验未执行。 |
| 文档与格式 | 配置示例通过 TOML 解析，PXE 默认关闭、空菜单；`cargo fmt --all -- --check` 通过。未触发远程 CI。 |
| 清理 | 本任务 CLI／GUI 进程均已退出，桌面控制已释放；一次性 smoke 脚本、下载工具、传输 fixture、独立 state 与 WSL 原生临时目录均已清理。保留源码、文档和正常构建产物，没有停止 SharedAccess 或其他用户服务。 |

本轮不宣称 macOS 实机、Linux/macOS GUI、真实路由器 DHCP 互操作、物理 PXE 固件或完整 Linux／WinPE 开机通过；不支持 IPv6、relay、Secure Boot、ARM64 客户端、任意 ISO 导入或 Windows 安装。未安装虚拟机／ADK，不修改防火墙、系统服务或用户凭据，未提交或推送。

## GUI 首次设置、PXE 整理与连接指引

### 改动

- 保留现有深色界面、字体与服务启停方式。通用设置先展示共享目录和持久状态目录，语言选择改为紧凑的一行；首次设置直接说明保存后到服务页启动。
- PXE 按地址与协议要求、启动菜单、文件检查组织，显示 HTTP／TFTP 监听条件及设置入口。启动程序路径、下载和来源链接放入展开区，未启用 PXE 时仍可准备配置。
- 保存后运行中的服务未改变时使用警示色，列出仍使用原设置的服务并提供“前往服务”入口，说明停止会中断连接；不自动停止或重启。设置页不再重复底部状态和固定重启说明，各设置标签分别保留滚动位置。
- 新增 GUI 内部 `connections.rs`：根据实际监听端点显示访问范围，提供 HTTP 复制／打开操作及 Linux NFS 挂载示例。通配监听地址使用同地址族的本机单播地址生成连接目标，不把 `0.0.0.0`、`::` 或服务器端 IPv6 link-local zone 当作客户端目标。网络接口读取失败时明确提示改用具体监听 IP。
- GUI 复用 core 已使用的 `network-interface 2.0.5`，仅增加 GUI 的直接依赖及 Cargo.lock 对应依赖项；接口枚举和连接文本在服务快照改变时生成，不在每次绘制中读取网络接口。
- PXE 地址未配置时不显示菜单 URL；地址与 HTTP 监听设置兼容时提供可复制的预览，并说明预览不表示菜单已运行。本机回环地址另有仅供本机检查的说明。
- TFTP 绑定错误补充具体地址；端口占用提示包含处理办法，ProxyDHCP 则提示外部 DHCP 或另一台服务器。服务错误可进入对应设置，普通端口及 NFS RPC 发现地址可定位并聚焦；完整原文放在技术详情和日志中。
- 主按钮背景与链接文字分别使用蓝色深浅值；源码颜色计算的白字／主按钮对比度为 5.37:1，链接色／面板为 6.46:1。状态点改为直接绘制，上传上限显示 GiB／MiB／KiB，悬停可看精确字节数。
- README 中英文同步新流程；保留用户提供的原截图，明确其为较早开发版，不替换为生成图或伪造状态。

### 实际验证

| 检查 | 实际结果 |
| --- | --- |
| GUI 回归 | `cargo test -p filemux-gui --locked --offline`：12 项通过。新增边界包括通配地址筛选、IPv6 URL／本机访问范围、NFS 端口与导出名引用，以及真实 TFTP 端口冲突地址。 |
| TFTP 网络回归 | `cargo test -p filemux-core --test tftp_udp --locked --offline`：12 项通过。 |
| 格式与真实构建 | `cargo fmt --all` 完成；`cargo run -p filemux-gui --locked --offline -- --config <临时配置>` 完成非测试构建并启动窗口。保留既有 license／license-file 清单警告。 |
| 首次设置与布局 | 实际查看 Windows 中英文、默认 1040×680 和最小 900×600 逻辑尺寸附近的窗口；确认共享目录和状态目录优先、保存动作固定可见、PXE 可滚动访问菜单／文件检查／可选工具。 |
| 复制与连接示例 | 实际启动 loopback HTTP／NFS，HTTP 获取临时文件返回 200 且内容一致。HTTP 地址、NFS 命令及有效 PXE 预览均通过系统剪贴板比对；包含空格和单引号的真实 NFS 示例经 Python `shlex` 解析仍为单个导出参数，使用实际 NFS 端口而非 RPC 发现端口。 |
| 浏览器入口 | 点击后 Chrome 窗口标题为 `Filemux directory`。浏览器 relay 超时，之后浏览器已切到其他页面，未继续操作其当前页；不把窗口标题检查写成完整浏览器 DOM 验证。 |
| 保存与使用新设置 | HTTP 运行时启用上传并保存，中英文均显示警示和仍使用原设置的服务，PUT 仍返回 405；通过服务页明确全部停止、重新启动后，PUT 返回 201，下载内容一致，其他协议没有被自动重新启动。 |
| 失败与恢复入口 | 占用随机 UDP 端口后，TFTP 显示实际冲突地址和处理提示，技术详情保留完整诊断；点击设置入口后，端口输入框可见聚焦边框和光标。 |
| PXE 预览 | 未配置 IPv4 时不生成 `0.0.0.0` 菜单地址；使用兼容的 loopback／HTTP 配置时可复制正确菜单 URL，并明确未启动服务。检查失败和可选下载展开区仍可访问；没有启动 DHCP／PXE 或联网下载程序。 |
| 清理 | 三个测试 GUI 均正常退出，退出码为 0；关闭运行窗口后 HTTP 不再接受连接。临时 UDP 占用 socket 已关闭，自建配置、共享文件、state、截图和脚本已清理，未修改用户配置、系统服务或防火墙。 |

本轮未执行 Linux／macOS GUI、完整键盘／读屏验收、实际客户端 NFS 挂载、物理 PXE 启动或远端 CI；不以命令示例检查代替这些验证。未修改版本、提交、推送或发布。

## 0.3.0 版本更新

- workspace、CLI 和 GUI 对 core 的依赖版本及 Cargo.lock 中三个项目包的版本更新为 `0.3.0`。
- Windows 上 `cargo fmt --all -- --check`、`cargo test --workspace --locked` 和 `cargo build --workspace --locked` 均通过；测试共 84 项通过。CLI 和 GUI 的 `--version` 分别输出 `filemux 0.3.0` 和 `filemux-gui 0.3.0`。
- 发布元数据检查 `python .github/scripts/release.py metadata --tag v0.3.0` 通过；Linux、macOS 构建结果以远端 CI 为准。
