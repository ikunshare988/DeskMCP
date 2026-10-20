# DeskMCP

把 Windows 桌面能力暴露成 [MCP](https://modelcontextprotocol.io) 工具，让 Claude Desktop、Cursor、Cline 这类客户端可以直接看屏幕、点鼠标、敲键盘。

单文件 exe，无 Python 运行时、无浏览器内核、不下载任何模型。

## 当前状态

这是 MVP。已实现窗口层、输入层、视觉层三件事，语义层（UI Automation）和审计面板还没做。

| 层 | 能力 | 状态 |
| --- | --- | --- |
| L0 窗口 | 枚举、聚焦、读矩形、抓屏 | 已实现 |
| L1 语义 | UI Automation 控件树、按控件名定位 | 未实现 |
| L2 视觉 | 截图 PNG、坐标点击 | 已实现 |
| L3 注入 | SendInput 鼠标键盘、Unicode 输入 | 已实现 |
| 宏模式 | 录制 → 编译成原生脚本 | 未实现 |

## 构建

需要 Windows + Rust stable。

```powershell
cargo build --release
```

产物是 `target\release\deskmcp.exe`，可以直接拷走。

自检（不接客户端也能跑）：

```powershell
.\deskmcp.exe --self-test
.\deskmcp.exe --self-test --shot out.png   # 顺便存一张全屏截图
```

## 接入客户端

```powershell
.\deskmcp.exe --print-config
```

会打印一段带绝对路径的配置。把它合并进客户端的 `mcpServers`：

```json
{
  "mcpServers": {
    "deskmcp": {
      "command": "D:\\DeskMCP\\target\\release\\deskmcp.exe",
      "args": []
    }
  }
}
```

配置文件位置：

- Claude Desktop：`%APPDATA%\Claude\claude_desktop_config.json`
- Cursor：`%USERPROFILE%\.cursor\mcp.json`
- Cline / Roo Code：扩展设置里的 MCP Servers

改完重启客户端。stdio 传输不需要端口，也不会弹窗。

## 工具

| 工具 | 作用 |
| --- | --- |
| `list_windows` | 列出顶层窗口：hwnd、标题、类名、进程名、屏幕矩形 |
| `get_window` | 单个窗口详情 + 该窗口支持的能力层级 |
| `focus_window` | 切到前台并解除最小化 |
| `screenshot` | 截图返回 PNG；传 hwnd 抓该窗口，不传抓全屏 |
| `click` | 移动并点击；传 hwnd 时坐标相对客户区 |
| `drag` | 按住左键拖动，16ms 步进插值 |
| `scroll` | 滚轮滚动，正数向上 |
| `type_text` | 输入 Unicode 文本，走 SendInput 的 Unicode 通道 |
| `press_keys` | 组合键，如 `ctrl+shift+s`、`alt+f4`、`win+r` |
| `clipboard_read` | 读剪贴板纯文本 |
| `clipboard_write` | 写剪贴板纯文本 |

`screenshot` 默认把图缩到 1280 宽，并在返回文本里给出缩放比例——模型换算点击坐标时要用这个值。

### 典型调用流程

```text
list_windows                  → 找到目标 hwnd
focus_window {hwnd}           → 切到前台
screenshot {hwnd}             → 看当前画面
click {x, y, hwnd}            → 点一下
screenshot {hwnd}             → 确认结果
```

## 实现要点

- **DPI**：进程启动时设成 Per-Monitor V2，截图像素和鼠标坐标都是物理像素，不会因为缩放比例错位。
- **窗口截图**：先 `PrintWindow` + `PW_RENDERFULLCONTENT` 拿 DWM 合成内容（窗口被遮挡也能拿到），失败再退回屏幕区域 `BitBlt`。
- **窗口矩形**：用 DWM 的 `DWMWA_EXTENDED_FRAME_BOUNDS`，避开 Win10 之后窗口四周那圈看不见的边框。
- **坐标注入**：`SendInput` 用 `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`，按虚拟桌面归一化，多显示器不用额外处理。
- **中文输入**：`type_text` 不模拟按键，直接发 UTF-16 code unit，不依赖键盘布局，也不动剪贴板。
- **协议**：stdio 上一行一条 JSON-RPC 2.0，日志全走 stderr。支持协议版本 2024-11-05 / 2025-03-26 / 2025-06-18。

## 已知限制

- 只支持 Windows。
- 对以管理员权限运行的窗口发送输入会被 UIPI 拒绝，除非 DeskMCP 本身也提权。
- 最小化的窗口无法截图，需要先 `focus_window`。
- 独占全屏的应用抓不到画面（这是 DWM 的机制限制，不是实现问题）。
- 不执行 JS、不做 OCR，视觉层目前只有截图，没有「按文字找位置」。
- 唯一实例模型：多个客户端同时连同一个 exe 会各自启动一个进程，不会共享状态。

## 路线

1. L1：UI Automation 控件树、按控件名/角色定位（让模型不必依赖坐标）
2. 审计面板：每次调用的入参、结果、耗时，可视化并支持回放
3. 宏模式：把一串操作编译成原生脚本，60Hz 确定性执行
4. 权限模型：默认拒绝点击/输入，按进程白名单授权

## 许可证

MIT
