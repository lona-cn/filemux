# Filemux 阶段一进度

## 当前状态

- 阶段一需求与验收条件已整理到 `docs/phase-1.md`。
- 阶段一功能与三 crate 合并已完成，workspace 仅含 `filemux-core`、`filemux-cli`、`filemux-gui`。GUI 已按“服务 / 设置 / 日志”分开布局并支持双语；本轮三个 crate 的版本统一为 `0.1.2`。
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

