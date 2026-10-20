# DeskMCP

把 Windows 桌面能力暴露成 [MCP](https://modelcontextprotocol.io) 工具，让 Claude Desktop、Cursor、Cline 这类客户端可以直接看屏幕、点鼠标、敲键盘。

单文件 exe，无 Python 运行时、无浏览器内核、不下载任何模型。

和同类工具最大的区别：**不靠截图也能操作**。先读窗口的控件树（文字，`observe`），再按控件 id 点击或填值（`click_element` / `set_value`），最后用一个 `act` 调用把整串步骤一次跑完。同一个任务通常只要 2–3 次调用，而不是「截图 → 点 → 截图 → 点」。

## 快速开始

**1. 下载**

从 [Releases](https://github.com/ikunshare988/DeskMCP/releases) 下载 `deskmcp.exe`，放到任意目录。exe 未做代码签名，Windows 首次运行可能提示「已保护你的电脑」，点「更多信息 → 仍要运行」即可。每个 Release 都附 `SHA256.txt`，可以自行核对。

**2. 生成配置**

```powershell
.\deskmcp.exe --print-config
```

**3. 贴进客户端**

把输出合并进客户端的 `mcpServers`：

```json
{
  "mcpServers": {
    "deskmcp": {
      "command": "D:\\DeskMCP\\deskmcp.exe",
      "args": []
    }
  }
}
```

配置文件位置：

- Claude Desktop：`%APPDATA%\Claude\claude_desktop_config.json`
- Cursor：`%USERPROFILE%\.cursor\mcp.json`
- Cline / Roo Code：扩展设置里的 MCP Servers
- Trae：设置 → MCP → 添加

改完重启客户端。stdio 传输不需要端口，也不会弹窗。

**4. 试一句**

> 打开记事本，写一段中文，保存，然后截图给我看。

客户端会按 `list_windows → observe → act` 走完，全程一张图。

**5. 出问题先自检**

```powershell
.\deskmcp.exe --self-test
```

会打印窗口数量、截图尺寸、输入通道是否可用、协议是否能正常应答。截图失败时用 `--self-test --shot out.png` 看一眼实际画面。

## 当前状态

这是 MVP。窗口层、语义层、输入层、视觉层都已可用，审计面板和宏模式还没做。

| 层 | 能力 | 状态 |
| --- | --- | --- |
| L0 窗口 | 枚举、聚焦、读矩形、抓屏 | 已实现 |
| L1 语义 | UI Automation 控件树、按控件 id 定位与触发 | 已实现 |
| L2 视觉 | 截图 PNG、坐标点击 | 已实现 |
| L3 注入 | SendInput 鼠标键盘、Unicode 输入 | 已实现 |
| 宏模式 | 录制 → 编译成原生脚本 | 未实现 |

## 构建

需要 Windows + Rust stable。

```powershell
cargo build --release
```

产物是 `target\release\deskmcp.exe`，可以直接拷走。

## 工具

| 工具 | 作用 |
| --- | --- |
| `list_windows` | 列出顶层窗口：hwnd、标题、类名、进程名、屏幕矩形 |
| `get_window` | 单个窗口详情 + 该窗口支持的能力层级 |
| `observe` | 读控件树，每个控件带稳定 id：`e3a1 Button "保存" aid=btnSave @120,340,80x26` |
| `click_element` | 按 id 触发控件，内部自动选 Invoke / Toggle / Select / Expand / 默认动作 / 点中心 |
| `set_value` | 按 id 写文本，写完回读校验，不一致自动改走键盘输入 |
| `act` | 一次调用顺序执行多步（点击/填值/快捷键/等待/等元素/滚动），默认附带结尾截图 |
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

## 为什么调用更少

同一个任务（在记事本里写一句话并保存），两种做法：

| 做法 | 调用序列 | 次数 | 图片 |
| --- | --- | --- | --- |
| 纯视觉 | list_windows → focus_window → screenshot → click → type_text → press_keys → screenshot | 7 | 2 张 |
| 语义 | list_windows → observe → act（聚焦、填值、Ctrl+S 全在里面） | 3 | 1 张 |

已知 hwnd 时只要 2 次。省下来的不只是往返延迟，还有两次整屏图片的 token。

几个具体的省法：

- **`observe` 一次拿到全部控件**，id 在同一窗口内稳定，可以跨调用复用，不必每步重新观察。
- **`set_value` 分两条通道**：普通输入框直接写值并回读；文档类控件（记事本正文、Word）一律走「聚焦 + 全选 + 输入」，因为它们的 `ValuePattern` 会接受写入却不生效，回读也读不到真相。回读不可靠时如实标注「无法校验」，不假装成功。
- **`click_element` 内部有动作降级链**：按钮走 Invoke，复选框走 Toggle，页签走 Select，菜单项走真实点击，都不支持才点坐标。
- **`wait_for` 把轮询放在服务端**：`act` 里等元素出现，不需要模型反复截图确认。

## 典型调用流程

```text
list_windows                     → 找到目标 hwnd
observe {hwnd}                   → 拿到控件 id
act {hwnd, steps: [
  {do:"focus_window", hwnd},
  {do:"click_element", id:"e3a1"},
  {do:"set_value", id:"e7", text:"..."},
  {do:"press_keys", keys:"ctrl+s"},
  {do:"wait_for", name:"已保存"}
]}                               → 一次执行完，结尾自动截图
```

窗口没有 UI Automation 树时（`get_window` 返回 `capabilities.uia=false`），退回 `screenshot` + `click` 的坐标方式。

## 实现要点

- **DPI**：进程启动时设成 Per-Monitor V2，截图像素和鼠标坐标都是物理像素，不会因为缩放比例错位。
- **窗口截图**：先 `PrintWindow` + `PW_RENDERFULLCONTENT` 拿 DWM 合成内容（窗口被遮挡也能拿到），失败再退回屏幕区域 `BitBlt`。
- **窗口矩形**：用 DWM 的 `DWMWA_EXTENDED_FRAME_BOUNDS`，避开 Win10 之后窗口四周那圈看不见的边框。
- **坐标注入**：`SendInput` 用 `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`，按虚拟桌面归一化，多显示器不用额外处理。
- **中文输入**：`type_text` 不模拟按键，直接发 UTF-16 code unit，不依赖键盘布局，也不动剪贴板。
- **协议**：stdio 上一行一条 JSON-RPC 2.0，日志全走 stderr。支持协议版本 2024-11-05 / 2025-03-26 / 2025-06-18。
- **控件树**：用 `IUIAutomationCacheRequest` 一次把 Name / ControlType / AutomationId / ClassName / Rect / IsEnabled / IsOffscreen 全部取回，避免每个控件 7 次跨进程调用。
- **防止被卡死**：连接超时 800ms、事务超时 2s；单次遍历最多访问 1200 个节点、墙钟 2.5 秒，超了先返回已拿到的部分，不让整个服务器陪着目标应用一起卡住。
- **控件 id**：由 `hwnd + automationId + name + controlType + className` 哈希得到，窗口移动或列表滚动后 id 不变，模型可以跨调用复用；重名时加 `-2` 后缀。
- **不可见节点**：`IsOffscreen` 为真的子树整棵跳过，既省 token，也避免模型点到看不见的控件。
- **弹窗跟随**：`observe` 会额外找出目标窗口拥有的顶层窗口、最近激活的弹出窗口，以及同进程的 Popup 类浮层（如 `Microsoft.UI.Content.PopupWindowSiteBridge`），用原始视图读一遍再合并进来，标成 `Popup` 缩进一层。
- **菜单项走真实点击**：WinUI 的菜单栏项对 `Invoke` 和 `Expand` 都返回成功，但界面毫无变化；对 `MenuItem` / `SplitButton` 直接点控件中心，实测能正常展开。
- **抢前台**：AttachThreadInput → 临时置顶再取消 → `SwitchToThisWindow` 三层叠加；仍然失败时直接报错，不把按键静默丢给别的窗口。

## 已知限制

- 只支持 Windows。
- 对以管理员权限运行的窗口发送输入会被 UIPI 拒绝，UIA 也读不到它们的控件树（返回空），除非 DeskMCP 本身也提权。
- 最小化的窗口无法截图，需要先 `focus_window`。
- 独占全屏的应用抓不到画面（这是 DWM 的机制限制，不是实现问题）。
- 弹出菜单、下拉框、对话框如果是被主窗口拥有的窗口，会自动合并进 `observe`；但有少数现代应用（例如 WinUI 3 的记事本）完全不把浮层内容暴露给 UI Automation，这时只能改用 `press_keys`（如 `alt+f`）或截图后按坐标点。
- 目标应用弹着模态对话框时，对它的 UIA 调用可能变慢甚至超时。
- 键盘输入只对前台窗口有效：窗口在别的虚拟桌面、或被系统前台锁挡住时，`set_value` 会明确报错而不是假装成功。
- 不执行 JS、不做 OCR，视觉层目前只有截图，没有「按文字找位置」。
- 唯一实例模型：多个客户端同时连同一个 exe 会各自启动一个进程，不会共享状态。

## 路线

1. 更细的语义动作：表格/树的选中与定位、滑块设值、容器内滚动
2. 审计面板：每次调用的入参、结果、耗时，可视化并支持回放
3. 宏模式：把一串操作编译成原生脚本，60Hz 确定性执行
4. 权限模型：默认拒绝点击/输入，按进程白名单授权

## 许可证

MIT
