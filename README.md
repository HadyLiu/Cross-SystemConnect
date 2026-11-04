# Cross-SystemConnect

跨平台**键鼠共享**软件：一套键盘鼠标控制多台 Windows / Linux 电脑，并支持剪贴板与文件协同。

完整说明见 [Doc/README.md](Doc/README.md)；架构见 [Doc/架构设计.md](Doc/架构设计.md)；构建与发布见 [Doc/构建发布流程.md](Doc/构建发布流程.md)；使用见 [Doc/用户手册.md](Doc/用户手册.md)。

---

## 功能介绍

| 功能 | 说明 |
|------|------|
| 键鼠共享 | 鼠标移到屏幕边缘即可切到相邻电脑，键盘输入跟随 |
| 屏幕布局 | 键鼠主机在中间，已连接的电脑拖到上下左右或四个角，按对应的边或角穿屏 |
| 剪贴板共享 | 文本、图片、文件清单跨机同步（大文件按需拉取） |
| 文件传输 | 复制粘贴传输（`Ctrl+C`/`Ctrl+V` 与托盘粘贴；跨屏拖拽已取消） |
| 托盘驻留 | 启动后拉起内核并驻留托盘。Windows 双击或右键打开设置；Linux 左键打开设置 |
| 断线回本地 | 连接断开时键鼠自动回到本机 |
| 开机自启 | 可配置（由内核 / 平台层配合） |

典型用法：两台电脑各自用 Wi-Fi 上网，中间用网线直连专门跑通信；直连网卡只配 IP/掩码，不配网关与 DNS。

**默认角色为客户端**；若配置中已保存为服务端（键鼠主机），则继续以服务端运行。

---

## 架构一览

双进程：

- **`Cross-SystemConnect-core`**：无界面内核（网络、切换、剪贴板、文件、本机 IPC）
- **`Cross-SystemConnect`**：托盘 + 内嵌 WebView 设置；经 Named Pipe / Unix socket 控制内核；启动时自动拉起内核

不使用本机 HTTP 配置端口。Windows 与 Linux 的可执行文件由 Docker 镜像 `cross-systemconnect-packager` 一次生成，不在两端分别编译。Windows 目标仍是 GNU，不使用 MSVC。

---

## 快速开始

完整步骤见 **[Doc/构建发布流程.md](Doc/构建发布流程.md)**。

使用 Docker 生成的程序。重新生成前先托盘右键「退出」，否则新启动的界面会连上还在运行的旧内核。

```bash
bash scripts/package-docker.sh

# Linux（界面会拉起同目录的内核）
./dist/linux/Cross-SystemConnect

# Windows
.\dist\windows\Cross-SystemConnect.exe
```

压缩包：`dist/Cross-SystemConnect-linux-x86_64.tar.gz`、`dist/Cross-SystemConnect-windows-x86_64.zip`。

配置：Windows `%APPDATA%\Cross-SystemConnect\config.json`（会自动迁移旧版 `%APPDATA%\CSC\`），Linux `~/.config/cross-systemconnect/config.json`。  
日志：程序目录下的 `log.txt`（如 `dist/linux/log.txt`），超过 100 KB 自动清空。

托盘右键只有「设置」和「退出」。Windows 可双击托盘图标；Linux 用左键打开设置。被控端首次连上时只提示「信任」或「取消」。

电脑之间的键鼠通道是 TLS。每台机器在配置目录保存自己的 `identity.crt` 和 `identity.key`。发现广播只包含计算机名和端口。

Linux 被控端在 X11 上需要 `libx11-6`、`libxtst6`。Wayland 上用 `/dev/uinput`，不弹出远程交互。见 [Doc/用户手册.md](Doc/用户手册.md)。

### GitHub Actions

`.github/workflows/ci.yml` 按 **image → check → build** 运行，统一使用 Docker 工具链镜像 `ghcr.io/hadyliu/cross-systemconnect-toolchain`：`image` 只在 GHCR 上还没有该镜像时构建一次（即第一次推送时），之后直接复用；`check` 在每次 push/PR 时于镜像内执行测试、clippy、fmt、audit、deny；`build` 只在推送 `v*` 标签时运行，编译 Linux 与 Windows 包、生成 `SHA256SUMS` 并创建草稿 Release。需重建镜像时手动运行 `toolchain-image` 并勾选 `force`。仓库需在 Settings → Actions → General 把 Workflow permissions 设为读写；首次 check 可能因历史代码格式和 clippy 警告失败。详见 [Doc/构建发布流程.md](Doc/构建发布流程.md) §8。

版本号在根目录 `Cargo.toml` 的 `version`。设置窗口左上角显示 `v` 加该版本；窗口标题和托盘提示同样带上版本。发布步骤见 [Doc/构建发布流程.md](Doc/构建发布流程.md)。

---

## 目录结构

```
Cross-SystemConnect/
├── Cargo.toml          # Rust workspace（版本号、公共依赖）
├── crates/
│   ├── csc-proto/      # 协议帧、消息、配对、IPC 定义
│   ├── csc-config/     # 配置与日志文件
│   ├── csc-platform/   # 平台层：Windows 钩子/注入、Linux X11/uinput、剪贴板等
│   ├── csc-core/       # 内核 Cross-SystemConnect-core：网络、TLS、切换、剪贴板、文件传输、IPC
│   └── csc-ui/         # 界面 Cross-SystemConnect：托盘 + WebView 设置窗、传输窗口
├── .github/workflows/  # GitHub Actions：镜像 → 检查 → 构建发布
├── packaging/          # Docker 镜像定义与编译脚本（一次生成 Windows 与 Linux）
├── scripts/            # package-docker.sh、check-docker.sh
├── resources/          # 图标
├── tools/              # 辅助脚本
└── Doc/                # PRD、架构设计、构建发布、用户手册、测试、CHANGELOG
```

生成产物（`dist/`、`target/`）不纳入版本库。

## 文档

| 文档 | 内容 |
|------|------|
| [Doc/README.md](Doc/README.md) | 特性、支持平台、快速开始 |
| [Doc/PRD.md](Doc/PRD.md) | 需求 |
| [Doc/架构设计.md](Doc/架构设计.md) | 协议与架构 |
| [Doc/界面设计.md](Doc/界面设计.md) | 界面 |
| [Doc/构建发布流程.md](Doc/构建发布流程.md) | 工具链、构建、发布 |
| [Doc/用户手册.md](Doc/用户手册.md) | 使用说明 |
| [Doc/测试用例.md](Doc/测试用例.md) / [Doc/测试bug问题.md](Doc/测试bug问题.md) | 测试 |
| [Doc/CHANGELOG.md](Doc/CHANGELOG.md) | 变更记录 |

## 许可证

本项目基于 [Apache License 2.0](LICENSE) 开源。Copyright 2026 Hady Liu。
