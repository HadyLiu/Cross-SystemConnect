# Cross-SystemConnect

跨平台**键鼠共享**软件：一套键盘鼠标控制多台 Windows / Linux 电脑，并支持剪贴板与文件协同。

详细设计见 [Doc/设计方案.md](Doc/设计方案.md)；开发环境与构建步骤见 [Doc/开发环境.md](Doc/开发环境.md)。

---

## 功能介绍

| 功能 | 说明 |
|------|------|
| 键鼠共享 | 鼠标移到屏幕边缘即可切到相邻电脑，键盘输入跟随 |
| 屏幕布局 | 键鼠主机在中间，已连接的电脑拖到上下左右或四个角，按对应的边或角穿屏 |
| 剪贴板共享 | 文本、图片、文件清单跨机同步（大文件按需拉取） |
| 文件传输 | 复制粘贴与跨屏拖拽传输（设计目标，持续完善中） |
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

完整步骤见 **[Doc/开发环境.md](Doc/开发环境.md)**。

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

Linux 被控端在 X11 上需要 `libx11-6`、`libxtst6`。Wayland 上用 `/dev/uinput`，不弹出远程交互。见 [Doc/开发环境.md](Doc/开发环境.md)。

版本号在根目录 `Cargo.toml` 的 `version`。设置窗口左上角显示 `v` 加该版本；窗口标题和托盘提示同样带上版本。发布步骤见 [Doc/开发环境.md](Doc/开发环境.md) 第 6 节。

---

## 文档与目录

| 文档 / 目录 | 内容 |
|-------------|------|
| [Doc/设计方案.md](Doc/设计方案.md) | 需求、协议、架构 |
| [Doc/开发环境.md](Doc/开发环境.md) | 环境搭建、构建、运行、FAQ |
| `packaging/` | Docker 镜像定义，一次生成 Windows 与 Linux |
| `scripts/` | `package-docker.sh` |
| `crates/` | Rust 源码（crate 名可含 csc-*，产物名为 Cross-SystemConnect*） |
| `resources/` | 图标等资源 |

生成产物（如 `dist/`）不纳入版本库。
