# Cross-SystemConnect

一套键盘鼠标控制多台 Windows / Linux 电脑，并在电脑之间同步剪贴板、用复制粘贴互传文件。所有操作在后台自动完成，不需要在软件里点「发送」或「接收」。

> 当前文档对应 **v2.0.0（未发布）**，协议版本 2，与 v1.x 不互通。变更见 [CHANGELOG](CHANGELOG.md)。

## 特性

- **键鼠共享**：鼠标推过屏幕边缘即切到相邻电脑，键盘跟随；支持滚轮、侧键、多显示器、混合 DPI。
- **剪贴板共享**：文字（最大 15 MB）和图片（PNG）双向同步，大内容在粘贴时才传输；密码管理器内容不同步。
- **文件粘贴**：在一台电脑复制文件，到另一台电脑的文件管理器或桌面按 `Ctrl+V`（Linux 也可右键「粘贴」），或用托盘「粘贴其他电脑复制的文件到这里」；文件存到那个文件管理器当前打开的文件夹，拿不到位置或托盘粘贴时存到兜底保存目录（默认桌面）。跨屏拖拽已取消。
- **被控端互传**：被控端之间的粘贴经主机中转。
- **安全**：TLS 1.3；配对需两端打开限时的「添加电脑…」窗口，被控端输入主机显示的 6 位配对码；新电脑要拖进布局才会被控制；随时可「暂停共享」，被控端可用 `Ctrl+Alt+Shift+P` 紧急暂停并能看到“正在被控制”提示。
- **低延迟**：千兆直连下输入到注入 P50 < 1 ms、P99 < 2 ms（目标）；文件传输 ≥ 100 MB/s。
- **键盘友好**：按物理键位转发，打出的字符由被控端自己的布局和输入法决定；大写锁定等状态自动同步。
- **防误切**：按住键不切换、锁定在当前屏幕（`Ctrl+Alt+Shift+K`）、全屏程序不切换、角落死区。
- **指针一致**：不同缩放的屏幕上指针物理速度一致，可按电脑微调；控制其他电脑时隐藏本机光标；高精度滚轮；使用任一电脑时其他电脑不熄屏。
- **文件传输**：断点续传、大量小文件打包、空间检查、跨系统文件名自动调整。
- **网络灵活**：自动发现同网段电脑，也可手动添加 IP/IPv6/主机名（跨网段、VPN）；网线断开可自动改走 Wi-Fi。
- **隐私**：数据只在配对的电脑之间传输，无遥测；日志不记录按键和剪贴板内容。
- **中英文界面**。
- **省心**：登录后自启内核，托盘常驻；断线自动重连，切换和断线不卡键；端口可配置，修改后自动重连无需重启。

## 支持平台

| 平台 | 状态 |
|---|---|
| Windows 10/11 x86_64 | 支持（设置窗需 WebView2 Runtime） |
| Linux GNOME Wayland 会话 x86_64（Ubuntu 24.04 LTS / 26.04 LTS 默认会话） | 支持（GNOME 46+；首次需允许系统“远程交互/输入捕获”授权，并启用随包的 GNOME Shell 扩展） |
| Linux X11 会话 x86_64（Deepin/UOS、Ubuntu 24.04 的“Ubuntu on Xorg”等） | 支持（需 `libx11-6`、`libxtst6`、`libxi6`、`libxfixes3`） |
| KDE Plasma 6 Wayland | 尽力支持，未验证（TBD，计划 M6） |
| wlroots 系 Wayland（Sway、Hyprland 等） | 不支持（缺少 InputCapture 等 portal） |
| macOS | 不支持（架构预留） |
| Windows 安全桌面（锁屏、登录、UAC） | 不支持控制，显示“安全桌面”状态并把键鼠交还主机（服务助手 M7，TBD） |

## 快速开始

1. **网络**：两台电脑用网线直连，直连网卡设静态 IP（如 `192.168.100.1/24` 与 `192.168.100.2/24`），**不设网关和 DNS**。
2. **防火墙**：放行 `Cross-SystemConnect-core` 的控制端口 TCP 24900、数据端口 TCP 24901、发现端口 UDP 24900（默认值，仅直连网卡所在网络；Windows 只放行“专用/域”网络，直连网卡请设为专用网络）。三个端口都可在设置「网络」中修改，控制端口与数据端口不能相同，改了要同步放行新端口。
3. **运行**：两台电脑都运行界面程序
   ```bash
   ./Cross-SystemConnect                   # Linux
   .\Cross-SystemConnect.exe               # Windows
   ./Cross-SystemConnect --open-settings   # 启动并打开设置
   ```
4. **选角色**：首次运行向导里，接着键盘鼠标的电脑选「是」（主机），其他选「否」（被控端）。Ubuntu（Wayland）上向导会多一步“系统授权”：按提示允许授权、启用 CSC 的 Shell 扩展，必要时注销重新登录。
5. **配对**：向导最后会自动打开「添加电脑…」（2 分钟内有效；之后可在托盘或设置「设备」页再次打开）。主机显示 6 位配对码，在被控端输入，然后在主机点「配对」。
6. **摆放**：新配对的电脑出现在主机首页的“未摆放”区，把它拖到它实际所在的方向。把鼠标推过那条边即可切换；`Ctrl+Alt+/` 随时回到主机，`Ctrl+Alt+Shift+L` 所有电脑一起锁屏。

详细说明见 [用户手册](用户手册.md)。

## 构建

发布程序只由 Docker 镜像 `cross-systemconnect-packager` 生成（Linux 本机目标 + Windows `x86_64-pc-windows-gnu`，不使用 MSVC，不在两端分别编译）：

```bash
bash scripts/package-docker.sh
```

产物在 `dist/linux/` 与 `dist/windows/`（界面与内核各一个，另有两个压缩包）。单元测试与静态检查同样在该镜像内：

```bash
bash scripts/check-docker.sh
```

性能测量（延迟 P50/P99、吞吐）：两端开启测量模式后运行 `Cross-SystemConnect-core --bench latency --peer <名字>`，方法见 [架构设计](架构设计.md) §12.15。

签名、打包、CI 与发布检查见 [构建发布流程](构建发布流程.md)。

## 目录结构

```
Cross-SystemConnect/
├── Cargo.toml                 # workspace
├── README.md
├── CHANGELOG.md
├── Doc/
│   ├── PRD.md
│   ├── 架构设计.md            # 含通信协议
│   ├── 界面设计.md
│   ├── 测试用例.md
│   ├── 构建发布流程.md
│   └── 用户手册.md
├── packaging/                 # Docker 镜像、gnome-shell-extension/（csc-shell 扩展）
├── scripts/                   # package-docker.sh
├── crates/
│   ├── csc-proto/             # 跨机帧 + IPC 类型
│   ├── csc-config/            # 配置读写与迁移
│   ├── csc-platform/          # 平台 trait 与 Windows / X11 / Wayland(GNOME) 实现
│   ├── csc-core/              # 二进制：内核 Cross-SystemConnect-core
│   └── csc-ui/                # 二进制：托盘 + WebView Cross-SystemConnect
│       └── ui/                # Clash 风格 HTML/CSS/JS
└── resources/                 # 图标、i18n/zh-CN.json、i18n/en-US.json 等
```

> 文档目录的最终布局（`Doc/` 内文件名）为建议，TBD。

## 文档索引

| 文档 | 内容 |
|---|---|
| [PRD.md](PRD.md) | 背景、目标用户、功能需求 F1–F34（含 F32 Wayland、F33 共享控制、F34 指针一致性）、非功能指标、平台范围、里程碑、固有限制 |
| [界面设计.md](界面设计.md) | 信息架构、各窗口线框图与控件映射（含配对窗、被控提示层、Wayland 授权）、状态设计、视觉规范、交互、多语言与无障碍、前端架构与 WebView 加固、界面测试 T-UI-01～40（T-UI-24/25 已取消） |
| [架构设计.md](架构设计.md) | 进程/模块、数据流、平台实现（Windows / X11 / GNOME Wayland）、配置与 IPC（含推送 JSON 结构）、威胁模型、完整通信协议、Wayland 设计（§13）、安全加固（§14） |
| [测试用例.md](测试用例.md) | 按模块编号的单元/集成/真机用例，有效约 205 条（含 T-SEC、T-UX、T-WL；跨屏拖拽相关已取消） |
| [构建发布流程.md](构建发布流程.md) | 工具链、交叉编译、签名、打包、CI、版本号、发布清单 |
| [CHANGELOG.md](CHANGELOG.md) | 版本变更记录 |
| [用户手册.md](用户手册.md) | 安装、Wayland 授权、配对、布局、热键、剪贴板、粘贴、暂停共享、设置、更新、诊断、隐私声明、卸载、固有限制、排错 |

## 参考

Deskflow、Input Leap、LAN Mouse（同类项目）；Tokio、rustls、serde；tao / wry / tray-icon；Windows `SetWindowsHookEx`、`SendInput`；X11 XInput2、XTest、XFixes、XDND；xdg-desktop-portal（InputCapture、RemoteDesktop、Clipboard）、libei。

## 许可证

本项目基于 [Apache License 2.0](../LICENSE) 开源。Copyright 2026 Hady Liu。
