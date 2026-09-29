# Cross-SystemConnect

跨平台**键鼠共享**软件：一套键盘鼠标控制多台 Windows / Linux 电脑，并支持剪贴板与文件协同。

详细设计见 [Doc/设计方案.md](Doc/设计方案.md)；开发环境与构建步骤见 [Doc/开发环境.md](Doc/开发环境.md)。

---

## 功能介绍

| 功能 | 说明 |
|------|------|
| 键鼠共享 | 鼠标移到屏幕边缘即可切到相邻电脑，键盘输入跟随 |
| 屏幕布局 | 九宫格配置各机相对位置（左 / 右 / 上 / 下） |
| 剪贴板共享 | 文本、图片、文件清单跨机同步（大文件按需拉取） |
| 文件传输 | 复制粘贴与跨屏拖拽传输（设计目标，持续完善中） |
| 托盘驻留 | 启动后拉起内核并驻留托盘；右键或双击打开设置 |
| 断线回本地 | 连接断开时键鼠自动回到本机 |
| 开机自启 | 可配置（由内核 / 平台层配合） |

典型用法：两台电脑各自用 Wi-Fi 上网，中间用网线直连专门跑通信；直连网卡只配 IP/掩码，不配网关与 DNS。

**默认角色为客户端**；若配置中已保存为服务端（键鼠主机），则继续以服务端运行。

---

## 架构一览

双进程：

- **`Cross-SystemConnect-core`**：无界面内核（网络、切换、剪贴板、文件、本机 IPC）
- **`Cross-SystemConnect`**：托盘 + 内嵌 WebView 设置；经 Named Pipe / Unix socket 控制内核；启动时自动拉起内核

不使用本机 HTTP 配置端口。Windows 构建使用 **MSYS2 UCRT64 + GNU**（**不使用 MSVC**）。

---

## 快速开始

完整步骤见 **[Doc/开发环境.md](Doc/开发环境.md)**。

```bash
cargo build --release -p csc-core -p csc-ui

# 推荐入口：托盘 UI（自动启动内核）
./target/release/Cross-SystemConnect.exe

# 仅内核
./target/release/Cross-SystemConnect-core.exe
```

配置：`%APPDATA%\Cross-SystemConnect\config.json`（会自动迁移旧版 `%APPDATA%\CSC\`）。  
日志：`%APPDATA%\Cross-SystemConnect\Cross-SystemConnect.log`。

托盘：右键「打开设置」/ 双击打开设置 / 暂停共享 / 退出。

---

## 文档与目录

| 文档 / 目录 | 内容 |
|-------------|------|
| [Doc/设计方案.md](Doc/设计方案.md) | 需求、协议、架构 |
| [Doc/开发环境.md](Doc/开发环境.md) | 环境搭建、构建、运行、FAQ |
| `crates/` | Rust 源码（crate 名可含 csc-*，产物名为 Cross-SystemConnect*） |
| `resources/` | 图标等资源 |

生成产物（如 `target/`）不纳入版本库。
