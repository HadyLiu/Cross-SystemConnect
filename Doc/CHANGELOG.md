# Changelog

本文件记录 Cross-SystemConnect 的重要变更，格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)（规则见 [构建发布流程](构建发布流程.md) §7）。

> 1.1 和 0.3 两个条目是根据当时的设计文档整理的，日期是文档日期，不一定是软件发布日期。中间版本（如 0.4–1.0）没有记录，TBD。

## [Unreleased]

### 变更

- **被控提示改为托盘绿点**：不再显示屏幕右上角「Controlled · Ctrl+Alt+Shift+P」叠层（含 X11 override-redirect / 原计划的 Shell 顶栏指示）。已连接时主机与被控端托盘图标右下角常亮绿点；仅当本机正在被控时绿点闪烁；未连接不加绿点。状态字段 `status.beingControlled`；`ui.controlled_indicator` 与屏幕提示层不再使用。

### 删除

- **移除跨屏文件拖拽**：电脑之间传文件只用复制粘贴（`Ctrl+C` / `Ctrl+V`，以及托盘「粘贴其他电脑复制的文件到这里」）。不再实现边缘细窗、边缘投放窗、虚拟拖放源与承接窗。协议消息 `DRAG_*`、`CAP_VIRTUAL_DRAG`、IPC `drag_enter` / `drag_edge`、平台 `vdrag` 等保留编号/符号但不使用。相关测试与缺陷项标为功能已取消。

## [0.2.1] - 2026-11-04

### 变更

- 清理代码格式与 clippy 警告：全仓 `cargo fmt`，修复全部 clippy 警告，`packaging/check.sh` 的 fmt / clippy 步骤可通过。
- 工作流避免分支与标签重复触发：同一 commit 同时推送分支和 `v*` 标签时只由标签 run 完整执行 image → check → build（见 [构建发布流程](构建发布流程.md) §8.1.1）。
- 修复 CI 工具链镜像缺少 clippy/rustfmt：Dockerfile 固定 `rust:1.99.1-bookworm` 并显式安装组件；镜像改按 Dockerfile 内容哈希打标签，Dockerfile 变更后自动重建一次。

## [0.2.0] - 2026-11-04

协议版本 2，配置版本 5。设计依据：设计方案 v1.1 评审与优化方案 v1.0（2026-10-02）。

### ⚠ 破坏性变更

- **协议升级到 2，与 1.x 不互通**：帧头 `ver` 和 `HELLO.proto` 必须为 2，否则关闭连接（`bad_version`）。所有电脑要同时升级。
- **信任模型改变**：删除按计算机名保存的 `peer_fingerprints`，升级后每对电脑需要**重新配对一次**。主机不再自动信任第一次见到的证书。
- **配对流程改变**：两端都要先点「添加电脑…」打开 2 分钟的“允许新配对”窗口，未打开时配对请求直接拒绝（`pairing_closed`）；主机显示 6 位码、被控端输入（承诺-揭示，消息 `PAIR_COMMIT`/`PAIR_NONCE`）；新配对的电脑**不自动放入布局**，需拖进布局后才参与切换与剪贴板。
- **默认热键变更**：所有电脑一起锁屏 `Win+L` → `Ctrl+Alt+Shift+L`；锁定在当前屏幕为 `Ctrl+Alt+Shift+K`；新增被控端紧急暂停 `Ctrl+Alt+Shift+P`（避开 JetBrains 等 IDE 的 `Ctrl+Alt+L`/`Ctrl+Alt+K`）。配置中为 `Win+L`、`Ctrl+Alt+L`、`ScrollLock`、空值或 `Ctrl+Alt+K` 的值迁移时替换一次（之后仍可手动改回）。
- **新增数据端口 TCP 24901**：防火墙需放行 TCP 24900、TCP 24901、UDP 24900（默认值）。控制端口与数据端口必须不同，配置文件中二者相同或超出 1024–65535 时内核不监听网络（`config_invalid`）。
- **发现报文 `CSCD` 格式变更**：带发送方 `control_port`、`data_port`；`HELLO` 新增 `ports`，`DATA_ATTACH` 新增 `data_port`。
- **支持 Ubuntu 默认的 GNOME Wayland 会话（F32）**：主机经 xdg-desktop-portal InputCapture（指针屏障）抓取，被控端经 RemoteDesktop portal + libei 注入（首次需系统授权，保存 restore token），剪贴板经 portal Clipboard，焦点/全屏/文件管理器识别与本机态热键由随包的 GNOME Shell 扩展提供。KDE Plasma 尽力支持（TBD），wlroots 系（Sway、Hyprland 等）显示 `unsupported_session`。不使用 `/dev/uinput`。
- **`Ctrl+V` 文件粘贴存到文件管理器当前打开的文件夹**（原为固定存桌面），只在文件管理器/桌面触发：Windows 拦截 `Ctrl+V` 并经 `IShellWindows` 取当前目录；Linux（X11/Wayland）**不再用 `XGrabKey` 抓键**，改为文件管理器获得焦点时把剪贴板换成占位文件、用 inotify 定位粘贴位置，因此右键「粘贴」也有效。拿不到位置时才存到“兜底保存目录”（原“保存目录”）。
- **IPC 参数名统一为蛇形**：`cancel_transfer`/`resolve_conflict`/`dismiss_transfer` 的 `transferId` → `transfer_id`；`status`/`transfers`/`pairing` 推送结构重定义（架构设计 §8.2）。
- **`MOUSE_MOVE` 只带相对位移**：删除 `x`/`y`，`dx`/`dy` 为主机系统加速后的像素并按两端 DPI 换算到相同物理速度，再乘每台电脑的速度系数。
- **TLS 只允许 1.3**，证书算法固定为 Ed25519（v1.x 证书若不是 Ed25519，升级时重新生成；升级本来就需重新配对）。
- **接收文件名策略**：对 Windows 不合法但无害的名字（非法字符、保留名、末尾点或空格、过长）由“拒绝”改为“自动改名”并在进度窗列出；只有 `..`、绝对路径、盘符/UNC 等会逃出保存目录的路径仍被拒绝。`PathReject` 相应精简，新增 `adapt_name`。
- **自启目标改为 `Cross-SystemConnect --background`**（托盘后台运行并监护内核），Linux 自启文件改名为 `cross-systemconnect.desktop`。
- **IPC**：`receive_dir` 方法改为只读的 `get_save_dir`；UI 由轮询 `get_status` 改为 `subscribe` 推送。
- **平台接口**：删除 `capture_drain()`、`take_raw_motion()` 轮询接口，改为通道推送；`ScreenInfo::virtual_size` 改为 `monitors()`。

### 新增

- 双向配对：两端在“允许新配对”窗口内连接，6 位配对码由 TLS exporter 与双方随机数（承诺-揭示）导出，主机显示、被控端输入（3 次机会），主机再确认；身份为 `device_id + 证书指纹`，指纹变化时警告；消息 `PAIR_REQUEST`/`PAIR_COMMIT`(0x08)/`PAIR_NONCE`(0x09)/`PAIR_CONFIRM`/`PAIR_RESULT`。
- 设置页「设备」：「添加电脑…」倒计时、已配对设备列表、每台电脑指针速度、移除设备（IPC `list_peers`、`forget_peer`、`pair_confirm{device_id, accepted, code?}`、`set_pairing_open`、`set_peer_pointer_speed`）。
- 端口可配置（F16）：设置页「网络」可修改控制端口（TCP，默认 24900）、数据端口（TCP，默认 24901）、发现端口（UDP，默认 24900，`network.discovery_port`），范围 1024–65535。
  - 校验：控制端口 ≠ 数据端口（`port_conflict`）、范围（`port_out_of_range`）、绑定测试（`port_in_use`）；不通过时 UI 禁止保存。
  - 发现端口所有电脑必须相同，可与控制端口数值相同（UDP 与 TCP 不冲突）；控制/数据端口各电脑可不同，主机按对端通告的端口连接。
  - 修改端口后内核先绑定新端口再关闭旧监听，并自动重连，无需重启；任一新端口绑定失败则回滚。
  - IPC `validate_network_ports`、`set_network_ports`；`get_status` 新增 `network` 状态（`listening`/`rebinding`/`config_invalid`）。
  - 错误码 `config_invalid`、`port_conflict`、`port_out_of_range`、`port_in_use`、`bad_data_port`。
  - `trusted_peers[].last_endpoint` 记录对端 IP 与端口，用于断线重拨。
- 主机休眠/重启/断网处理（F17）：休眠、关机前释放按键并发送 `PEER_STATE`；唤醒后立即重建网络重连；被控端显示主机状态（`host_state`）；被控端本地键鼠始终可用。
- 安全桌面状态（F18）：Windows 被控端进入锁屏/登录/UAC 时上报 `secure_desktop`，主机收回键鼠并暂停切到该机；服务助手列为 M7（TBD）。
- 键盘布局与输入法（F19）：按 HID Usage 转发，Windows 用扫描码注入、X11 用 evdev+8；键位映射表；丢弃 AltGr 合成的 LCtrl；可选 `keyboard.remap`。
- 锁定键同步（F20）：`SWITCH_ENTER.locks`，主机为准，回主机时同步远端改动；`keyboard.sync_locks`。
- 版本协商与手动更新检查（F21）：`HELLO.app_version/proto_min/proto_max`，`version_mismatch` 提示；`check_update`，默认不检查、不自动安装。
- 切换辅助（F22）：按住修饰键阻止切换、锁定在当前屏幕（`Ctrl+Alt+Shift+K`，`set_lock_to_screen`）、全屏不切换（`FOCUS_INFO.fullscreen`）、角落死区 10 像素。
- 富文本剪贴板（F23）：多格式 `CLIP_OFFER.formats`（纯文本、HTML、RTF、PNG），`CLIP_REQUEST.fmt`；`clipboard.rich`。
- 文件传输细节（F24）：断点续传（`.csc-part`、`FILE_REQUEST.resume`、30 分钟窗口）、小文件打包 `FILE_BATCH`、修改时间/权限/属性保留策略、剩余空间与 FAT32 检查、跨系统文件名自动调整。
- 系统通知（F25）：`notify.*` 开关，同类 5 秒去重。
- 多语言（F26）：简体中文、English，`ui.language`，资源 `resources/i18n/*.json`。
- 网络环境（F27）：手动对端 `network.manual_peers`（IPv4/IPv6/主机名，跨网段、VPN）、IPv6 双栈监听、网卡自动模式（有线断开改用 Wi-Fi 重连）；IPC `add_manual_peer`、`remove_manual_peer`。
- 健壮性与诊断（F28）：UI 监护内核自动重启、Linux `systemd --user` 单元、崩溃信息与 Windows minidump、内存自检、诊断包导出 `export_diagnostics`。
- 卸载与数据清理（F30）：`--uninstall`、`--uninstall --purge`、应用内 `reset_data`；安装包卸载时可选择保留配置与配对记录。
- 性能测量模式（F31）：`BENCH_SYNC`/`BENCH_ECHO`、时钟同步、`bench_run` 与 `--bench`，CSV 输出；标准测量环境与验收流程。
- 界面设计文档（[界面设计](界面设计.md)）：设置窗 9 个页面（首页/设备/剪贴板与文件/快捷键与切换/网络/通知与语言/隐私与诊断/高级/关于与更新）、首次运行向导、配对窗、接收进度窗、托盘图标状态；热键录制器与冲突检测（IPC `validate_hotkey`、`set_hotkey_recording`）；状态字段 `first_run`（`drag_edge` 保留未用），明确 `host_state` 取值；浅色/深色、WCAG AA 对比度、键盘与读屏支持；被控提示层、未摆放区、Wayland 授权横幅；界面测试 T-UI-01～40（跨屏拖拽相关项后在 Unreleased 取消）。
- Wayland（F32）：会话判定 `status.platform`（`session`/`desktop`/`supported`/`portals`/`shell_extension`）；IPC `wayland_setup{step}`；GNOME Shell 扩展 `csc-shell@<域名 TBD>`（D-Bus `org.csc.Shell1`）；portal restore token 保存在 `portal-tokens.json`（0600）。
- 共享控制与被控提示（F33）：托盘/首页「暂停共享」（IPC `set_sharing_enabled`，不持久化，`status.sharing_paused`）；被控端紧急暂停热键与 `CONTROL_PAUSE`(0x16)；被控提示层（`ui.controlled_indicator`，`status.controlled_by`）。
- 指针与输入一致性（F34）：按物理速度换算（`pointer.match_physical_speed`）与每台速度系数（`trusted_peers[].pointer_speed`）；远端态隐藏主机光标（`pointer.hide_host_cursor`）、被控端非活动时隐藏光标（`pointer.hide_inactive_cursor`）；`SWITCH_ENTER`/`EDGE_HIT` 带越界位移 `carry`；多段外露边的 `position_pct` 规则；滚轮 1/120 单位并在 X11 端累积；连发只由被控端产生（转发的 repeat 丢弃）；AltGr 不触发 `Ctrl+Alt` 热键。
- 防闲置：使用任一电脑时其他电脑不熄屏、不闲置锁屏（`ACTIVITY`(0x15)、`power.keep_peers_awake`）。
- 粘贴定位：`PASTE_LOCATED`(0x34)，`FILE_OFFER.dest_hint`/`paste_id`/`purpose:"tray_paste"`；占位目录 `$XDG_RUNTIME_DIR/csc-paste/`；Ubuntu 桌面（DING）与 Dolphin 加入文件管理器白名单（DING 标识 TBD）。
- （已在 Unreleased 移除）拖拽：落点 = 目标电脑上松开左键时指针所在位置；Esc 在所有平台取消；拖到第三台电脑（`DRAG_RETARGET`(0x42)）。
- IPC 新增 `list_interfaces`、`refresh_network`、`set_sharing_enabled`、`set_pairing_open`、`set_peer_pointer_speed`、`wayland_setup`；`export_diagnostics` 增加 `include_crash_dumps`；`validate_hotkey` 冲突类型增加 `ide`；`status`/`transfers`/`pairing` 推送给出完整 JSON 结构。
- 新消息 `PEER_STATE`(0x07)、`PAIR_COMMIT`(0x08)、`PAIR_NONCE`(0x09)、`ACTIVITY`(0x15)、`CONTROL_PAUSE`(0x16)、`PASTE_LOCATED`(0x34)、`FILE_BATCH`(0x59)、`BENCH_SYNC`(0x70)、`BENCH_ECHO`(0x71)；`DRAG_*`(0x40–0x42) 编号保留但未用（跨屏拖拽已取消）；`MOUSE_*` 类型固定为 0x20–0x23；`LOCK_SCREEN{reason}`。
- 新错误码 `secure_desktop`、`no_space`、`file_too_large_for_fs`、`resume_mismatch`、`bench_disabled`、`bad_direction`、`pairing_closed`、`pair_code_mismatch`、`commit_mismatch`、`too_many_connections`、`handshake_timeout`、`rate_limited`、`sharing_paused`、`placeholder_mismatch`、`resume_origin_mismatch`、`portal_denied`、`extension_missing`、`path_not_allowed`；UI 本地 `forbidden`、`update_signature_invalid`。
- 配置 v5 新增字段：`keyboard.*`、`switch.block_modifier/block_when_fullscreen/corner_dead_px`、`hotkeys.lock_to_screen`、`clipboard.rich`、`file_transfer.preserve_mtime/preserve_mode/resume_window_min/max_parallel`、`notify.*`、`ui.language`、`network.manual_peers/interface_mode/ipv6/listen_private_only/trusted_wifi`、`update.*`、`limits.*`、`diagnostics.bench_enabled`、`trusted_peers[].pointer_speed`、`hotkeys.emergency_pause`、`clipboard.push_mode`、`pointer.*`、`power.keep_peers_awake`、`pairing.open_window_s`、`identity.key_store`、`file_transfer.mark_of_web/strip_exec_on_desktop`、`ui.controlled_indicator`（缺省自动补齐，不再升配置版本）。
- 首次运行向导：选择角色、网卡、开机自启（默认勾选），Wayland 下增加系统授权与扩展步骤，最后打开“允许新配对”；两端都为主机时提示角色冲突（`role_conflict`）。
- 独立数据连接（TCP `data_port`，默认 24901，与控制端口分开；`DATA_ATTACH` + 会话令牌绑定），文件、大段文字、图片都走数据连接。
- 剪贴板延迟拉取：大于 64 KiB 的文字和所有图片只发元数据，粘贴时才用 `CLIP_REQUEST`/`CLIP_DATA`/`CLIP_END` 分块拉取（Windows 延迟渲染、X11 选择所有者 + INCR）。
- `INPUT_RESET` 消息、主机 `PressedSet` 与被控端 `InjectedSet`：切换、断线、锁屏时释放所有按下的键和鼠标按钮；被控端 1.5 s 看门狗。
- Win（Super）键缓冲：远端态下 Win 键先缓冲，根据后续按键决定转发或执行热键。
- 锁屏联动会话事件：Windows `WTS_SESSION_LOCK`、Linux logind `Lock` / ScreenSaver `ActiveChanged`。
- `FOCUS_INFO` 焦点上报；托盘菜单「粘贴其他电脑复制的文件到这里」（IPC `paste_remote_files`）。
- `SCREEN_UPDATE` 与显示器矩形模型（支持负坐标、外露边判断）；Windows PerMonitorV2 DPI 感知。
- （已在 Unreleased 移除）Windows 跨屏拖拽承接窗；Linux 非 XDS 拖拽用 inotify 定位副本。
- 能力位 `CAP_DATA_CHANNEL`、`CAP_LAZY_CLIP`、`CAP_FOCUS_INFO`、`CAP_MULTI_MONITOR`。
- 协议错误码表（`frame_too_large`、`not_paired`、`unsolicited`、`path_rejected` 等）。
- Windows 低级钩子看门狗（自动重装）；“以管理员权限运行”自启选项（计划任务）。
- 发布包 Authenticode 签名与 `SHA256SUMS`。

### 变更

- 键鼠事件由钩子线程经通道直接交给网络写任务，不再经 8 ms 主循环；连续移动合并为一帧。主机本机态的到边检测也由输入事件驱动，取消 8 ms 定时器。
- 控制连接 `SO_SNDBUF` 64 KiB、Linux `TCP_NOTSENT_LOWAT` 16 KiB、DSCP EF；数据连接限速到链路的 92%。
- 键鼠延迟指标改为 P50 < 1 ms、P99 < 2 ms（传文件时 P99 < 3 ms）。
- 心跳分级：被控中 250 ms / 1.5 s 超时；被控中且有键按下 100 ms / 500 ms（看门狗同步缩短）；空闲 2 s / 10 s。
- 剪贴板只发给已配对且在布局中的对端（`clipboard.share_scope`）；事件驱动监听（`AddClipboardFormatListener` / XFixes / portal），切换路径只比较序号、异步读取（100 ms 超时），粘贴键最多暂扣 100 ms 等待新内容；被控端在 `EDGE_HIT` 前刷新剪贴板；可选“只在切到该电脑时发送”（`clipboard.push_mode=on_switch`）；带密码管理器标记的内容不同步。
- 锁屏联动只针对用户主动锁屏和主机闲置锁屏；被控端闲置锁屏不联动。
- 文件记录在任何电脑有新复制时失效，并有 30 分钟有效期（`clipboard.files_ttl_min`）。
- Windows 注入改用 `SendInput` 绝对坐标；前台隐藏/限制光标时改相对移动并暂停边缘离开。
- 中转流控改为端到端：目标端 `FILE_ACK` 由主机改写 id 后转给源端。
- 单实例：Windows 命名互斥体，Linux `flock` 锁文件；命名管道仅当前用户可访问并拒绝远程客户端；Unix socket 0600。
- 自启一律为“用户登录后”启动。
- 日志单文件 1 MB 时转存为 `log.1.txt`（原为 100 KB 清空重写）。
- “保存目录”改名为“兜底保存目录”（默认桌面），只在拿不到文件管理器当前文件夹或托盘粘贴时使用；`file_transfer.receive_dir` 只能在配置文件中设置。
- 首次运行无界面时 core 不写开机自启；向导默认勾选“开机自动启动”。

### 删除

- `/dev/uinput` 注入方案（Wayland 改用 portal + libei）。
- Linux `XGrabKey` 抓取 `Ctrl+V`（改为剪贴板占位方案）。
- 新配对设备自动放入布局。
- 配置字段 `peer_fingerprints`。
- 平台函数 `capture_drain()`、`take_raw_motion()`。

### 安全

- 未配对连接只允许 `HELLO`/`HEARTBEAT`/`PAIR_*`，不进入布局、不收剪贴板、不能成为切换目标。
- 帧长上限：控制连接 64 KiB+256、数据块帧 64 KiB+64、JSON 帧 4 MiB，超限断开。
- `FILE_OFFER` 必须带一次性 `offer_token`（120 s）。
- 接收路径逐分量净化：拒绝 `..`、绝对路径、盘符/UNC；Windows 非法字符（含 `:`，因而不会形成备用数据流）、保留名、末尾点或空格、控制字符自动改名；不跟随、不创建符号链接。
- 隐私（F29）：任何日志级别都不记录按键内容、剪贴板内容、文件内容、配对码；info 及以上不记录文件名；无遥测；诊断包不含私钥；杀毒软件/EDR 误报与白名单说明、隐私声明写入用户手册。
- 测量消息 `BENCH_*` 仅在两端都开启 `diagnostics.bench_enabled` 时接受。
- 配对承诺-揭示（防中间人在看到对方随机数后挑选碰撞）；限时“允许新配对”窗口；被控端输入配对码；新设备不自动进布局；指纹变化警告。
- WebView 加固：只用 `textContent` 渲染、严格 CSP、禁止导航与新窗口、每个窗口的桥方法白名单（越权 `forbidden`）；计算机名清洗（去控制/双向字符、≤ 63 字符）。
- 管理员模式：命名管道加高完整性标签，普通进程无法连接；IPC 路径参数白名单（`path_not_allowed`）。
- 私钥 Ed25519，0600 / 仅本用户 ACL；可选 Windows DPAPI / Linux Secret Service（`identity.key_store=os`）；配对 nonce 与密钥用后清零。
- 暴露面：只在所选网卡或私有/链路本地地址监听；网卡自动模式不使用公共网络和未信任的 Wi-Fi；Windows 防火墙规则只作用于专用/域网络。
- 抗滥用：未认证连接全局 8、每 IP 2；TLS/HELLO 各 5 s 超时；失败过多按 IP 拒绝 5 分钟；配对窗队列上限 3。
- 消息方向检查：方向不符的消息断开（`bad_direction`）。
- Linux 粘贴占位定位校验令牌、inode、属主，不跟随符号链接（`placeholder_mismatch`）。
- 收到的文件：Windows 加 Mark-of-the-Web（ZoneId=3）；Linux 存到桌面的文件去掉可执行位，`.desktop` 不标记可信；续传校验发送方身份（`resume_origin_mismatch`）。
- 威胁模型增加“主机被攻陷”：被控端可随时紧急暂停并看到被控提示。
- 更新检查：清单 minisign 签名、下载域名白名单、说明纯文本显示（`update_signature_invalid`）。
- 诊断包默认不含崩溃转储（`.dmp`），需用户确认才包含。

## [1.1] - 2026-09-30

（设计方案 v1.1，对应《问题描述》第 42–52 条）

### 新增

- 改用 Rust 2021 重写；双进程：`Cross-SystemConnect-core` 无界面内核 + `Cross-SystemConnect` 托盘与 Clash 风格 WebView 设置窗（tao + wry + tray-icon），经本机 IPC（Named Pipe / Unix socket，`u32 LE length` + JSON）通信。
- 发布程序只由 Docker 镜像 `cross-systemconnect-packager` 生成（Linux + Windows `x86_64-pc-windows-gnu`）。
- 边缘切换需要到边后继续推过 `switch.edge_push_px`（默认 8 像素原始位移）；离开由被控端判定（`EDGE_HIT`，`CAP_EDGE_REPORT`）。
- 共享剪贴板（文件）：复制时只告诉主机条目清单，在另一台电脑按 `Ctrl+V` 才传输。
- 跨屏虚拟文件拖拽（`CAP_VIRTUAL_DRAG`，Windows OLE、Linux XDND/XDS）。
- 被控端之间经主机中转；接收端每 1 MiB `FILE_ACK`，未确认超过 8 MiB 暂停（`CAP_FILE_ACK`）。
- 接收端进度窗与重名处理（跳过 / 保留两者 / 替换）。
- 图片剪贴板（PNG，`CLIP_DATA`）。
- 快捷键：回到主机 `Ctrl+Alt+/`，所有电脑一起锁屏 `Win+L`。
- 日志写入程序目录 `log.txt`，级别 info，超过 100 KB 清空重写。
- `config_version` 4。

### 变更

- 文字剪贴板上限可调，最大 15 MB（`clipboard.max_inline_mb`）。
- 回到主机热键由 `Ctrl+Alt+Home` 改为 `Ctrl+Alt+/`；锁屏由 `ScrollLock` 改为 `Win+L`。
- 默认角色为 client。
- 粘贴的文件存到桌面，设置页不再提供接收目录。

### 删除

- 「切换共享」快捷键（`Ctrl+Alt+Pause`，从未实现）及 `hotkeys.toggle_enable`。
- `clipboard.max_total_mb`。
- 本机 HTTP 配置端口与打开系统浏览器的设置方式。

### 安全

- 控制通道强制 TLS 1.3（rustls），自签名证书，首次连接校验指纹；旧明文程序无法再连上。

## [0.3] - 2026-09-28

（设计稿）

### 新增

- C++17 实现方案；Windows 使用 MSYS2（UCRT64 + GCC），Linux 使用系统 GCC。
- 界面用 EEZ Studio 设计，生成 LVGL 代码，桌面端通过 SDL2 显示。
- 后台驻留。
- 控制通道 TCP 24900，TLS 1.3（OpenSSL）。

[Unreleased]: TBD
[0.2.1]: https://github.com/HadyLiu/Cross-SystemConnect/releases/tag/v0.2.1
[0.2.0]: https://github.com/HadyLiu/Cross-SystemConnect/releases/tag/v0.2.0
[1.1]: TBD
[0.3]: TBD
