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
| L1 语义 | UI Automation 控件树、按控件 id 定位与触发、**id 使用前身份校验**、**按条件查找（find）**、**状态读取（read）**、**动作矩阵（invoke，16 个动作）** | 已实现 |
| L2 视觉 | 截图 PNG、坐标点击 | 已实现 |
| L3 注入 | SendInput 鼠标键盘、Unicode 输入 | 已实现 |
| 宏模式 | 录制 → 编译成原生脚本 | 未实现 |

当前版本 **v0.21.0**。

## 更新日志

### v0.21.0

工具从 15 个增加到 18 个。这一版的目标是「**少截屏**」：能用语义读到的就不截图，能用一次调用做完的就不来回。

新增：

- **`find`**：按名称 / AutomationId / 控件类型 / 类名 / 可用状态查找控件，**只返回命中的那些**，支持 `within` 限定子树。不必再把整棵树读回来自己找，也不受 `observe`「一次最多 400 个元素」的限制。
- **`read`**：一次读多个控件的值 / 开关 / 选中 / 展开 / 范围 / 可用 / 可见 / 焦点 / 矩形，用来确认上一步到底生效没有——比再截一张整屏图便宜三个数量级，也不需要换算坐标。
- **`invoke`**：明确指定动作，不做猜测。16 个动作：`invoke` / `toggle` / `select` / `expand` / `collapse` / `scroll_into_view` / `realize` / `default_action` / `set_value` / `set_range` / `focus` / `click_center` / `close` / `maximize` / `minimize` / `restore`。滑块设值、明确展开/收起、只聚焦不点击这些只能靠它。
- `scroll` 扩成三种用法：`{item}` 把某项滚进视野（不依赖坐标）、`{container_id, direction}` 让容器自己滚、`{x, y}` 滚鼠标滚轮。
- `act` 新增 `invoke` 与 `scroll {item}` 两种步骤。

改进：

- `click_element` 操作前会自动让控件变得**可达**：虚拟化列表里还没实体化的项先 `Realize`，滚出视野的行先 `ScrollIntoView`。以前遇到这类控件只会报「没有可见区域」。
- `observe` 触到上限时明确提示「上面列出的不是全部控件」，而不是让模型以为没有别的控件了。
- 动作失败不再留裸 HRESULT：现在是「`e86d4` 触发失败：…」。模式不支持也不会悄悄降级成点击，而是直说「控件不支持设值（RangeValue，通常是滑块或进度条）」。

安全：

- `read` 不再原样回显控件内容：密码框一律隐藏，文档类控件的正文只报字符数，其余值最长回显 120 字符。桌面自动化很容易顺手把密码、令牌、密钥送进对话上下文。

实测发现（已写进下面「实现要点」）：**WinUI / Electron 应用在后台不实体化 UI Automation 树**——同一个记事本窗口，聚焦前 1 个节点、聚焦后 38 个。不先聚焦就观察，会把「这个软件没有控件树」当成事实。

### v0.20.1

- **修复：`observe` 之后进程无法退出，会攒游离进程。** 缓存的 `IUIAutomationElement` 保存在 `thread_local` 里，走到进程退出时由 TLS 析构去 `Release()`，会撞进 OLE32 的线程分离路径互相等待——进程永远不退出，只剩一个卡在 `Wait/UserRequest` 的线程。客户端反复重启就会在后台留下一堆 `deskmcp.exe`。现在读循环收到 EOF 后主动清空缓存。
- **修复：控件 id 在界面变化后会作用到错的控件。** id 只保证「observe 那一刻」指向某个控件；界面一变，UI Automation 会把元素句柄回收给同一位置上的**另一个**控件（典型场景：列表换了内容之后照着旧 id 点，点到另一行）。现在每次使用 id 前都会用 `Current*` 重新读一遍类型与名称做比对，对不上就明确报错并要求重新 `observe`——不猜、不降级、不静默点到别的东西。
- 文档：把路线改写为「少截屏、可操作大部分软件」的 P1–P6 方案，并写明游戏能力边界。

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
| `find` | 按名称 / AutomationId / 类型 / 类名 / 可用状态查找控件，**只回命中的那些**——不必先把整棵树读回来 |
| `read` | 读控件当前状态（值 / 开关 / 选中 / 展开 / 范围 / 可用 / 可见 / 焦点 / 矩形），用来确认操作是否生效 |
| `click_element` | 按 id 触发控件，内部自动选 Invoke / Toggle / Select / Expand / 默认动作 / 点中心；控件不在视野里会先自动滚进来 |
| `invoke` | 执行明确指定的动作：`invoke`/`toggle`/`select`/`expand`/`collapse`/`scroll_into_view`/`realize`/`default_action`/`set_value`/`set_range`/`focus`/`click_center`/`close`/`maximize`/`minimize`/`restore`。不支持时明确报错，不偷偷换动作 |
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
- **id 身份校验**：id 由 `hwnd + automationId + name + controlType + className` 哈希得到，缓存里同时记下 observe 时的类型与名称。每次使用前用 `Current*`（不是 `Cached*`）重读一遍比对——`Cached*` 是 observe 那一刻的本地副本，跟它自己比永远相等，读不出任何变化。`Current*` 失败=句柄已被移除或重建，对不上=界面变了，两种情况都明确报错。
- **退出清理**：stdin 关闭后主动释放缓存的 UIA 元素。留到 `thread_local` 析构时释放会与 OLE32 的线程分离互相等待，进程不再退出。
- **控件值按需脱敏**：`read` 不会把控件内容原样回给模型——密码框一律隐藏；文档类控件的正文只报字符数；其余值最长回显 120 字符。桌面自动化很容易顺手把密码、令牌、密钥送进对话上下文，这条是刻意的。
- **动作失败不留裸 HRESULT**：控件上的每个动作都包了一层，失败时是「`e86d4` 触发失败：…」而不是一个 `0x80131509`。模式不支持时也不会悄悄降级成点击，而是明确说「控件不支持设值（RangeValue，通常是滑块或进度条）」。
- **先聚焦，再观察**：WinUI / Electron 这类应用在**后台不实体化 UI Automation 树**，此时 `observe` / `find` 只能看到窗口本身（`find` 会报「访问 1 个节点」）。所以调用前要先 `focus_window`，否则会误判成「这个软件没有控件树」。实测同一个记事本窗口：聚焦前 1 个节点，聚焦后 38 个。

## 已知限制

- 只支持 Windows。
- 对以管理员权限运行的窗口发送输入会被 UIPI 拒绝，UIA 也读不到它们的控件树（返回空），除非 DeskMCP 本身也提权。
- 最小化的窗口无法截图，需要先 `focus_window`。
- 独占全屏的应用抓不到画面（这是 DWM 的机制限制，不是实现问题）。
- 弹出菜单、下拉框、对话框如果是被主窗口拥有的窗口，会自动合并进 `observe`；但有少数现代应用（例如 WinUI 3 的记事本）完全不把浮层内容暴露给 UI Automation，这时只能改用 `press_keys`（如 `alt+f`）或截图后按坐标点。
- 目标应用弹着模态对话框时，对它的 UIA 调用可能变慢甚至超时。
- 控件 id 不是永久有效的：界面改过（列表换了内容、切了页签、对话框关闭）之后必须重新 `observe`。用旧 id 会明确报「已失效」或「界面已变化」，而不是照着旧位置点到别的控件上。
- 键盘输入只对前台窗口有效：窗口在别的虚拟桌面、或被系统前台锁挡住时，`set_value` 会明确报错而不是假装成功。
- 不执行 JS。视觉层目前只有截图：没有「按文字找位置」，也没有模板匹配（OCR 与模板匹配在 P5 计划内）。
- 唯一实例模型：多个客户端同时连同一个 exe 会各自启动一个进程，不会共享状态。

## 路线

目标：**通过 UI 树在尽量少截屏的前提下，流畅操作大部分软件**；对没有 UIA 树的软件（自绘界面、窗口化游戏）用视觉通道兜底。

| 阶段 | 内容 | 状态 |
| --- | --- | --- |
| P1 语义寻址 | `find` 按名称/类型/ID 查找（只回匹配项）、`read` 读控件当前状态、`observe` 截断提示 | 已实现 |
| P2 动作矩阵 | 补 ScrollIntoView / VirtualizedItem.Realize / RangeValue / Window 等模式（`invoke` 工具，16 个动作）；虚拟化列表可滚到第 N 项 | 已实现 |
| P3 服务端闭环 | `act` 支持控制流与断言、`$1` 引用前一步结果、`wait_for` 多条件、id 失效自动重试 | 计划中 |
| P4 输入加固 | scancode 输入、相对移动（RawInput 游戏必需）、前台校验、通道自动降级 | 计划中 |
| P5 视觉句柄化 | 区域截图、模板匹配（自实现金字塔 NCC）、OCR（系统自带 `Windows.Media.Ocr`）、像素探针、差异截图 | 计划中 |
| P6 宏与审计 | 录制 → 60Hz 确定性回放、调用审计（入参/结果/耗时/通道）、权限白名单 | 计划中 |

### 少截屏靠什么

一张 1280×696 的整屏图约 1000+ token；一次 `read` 或像素探针只有几十字节。所以：

- **寻址一次、复用多次**：id 在同一窗口内稳定，不必每步重新观察。
- **截图一次变句柄**：看到目标后 `register` 成命名模板/区域，之后用名字引用，不再整屏截图。
- **状态验证不靠图**：`read` 读值/开关/选中，`pixels` 读颜色，`wait_for` 在服务端轮询。
- **多步一次跑完**：把「展开 → 选中 → 断言」压成一个 `act` 调用，减少模型往返。
- **尺寸按需**：区域裁剪优先于整窗，整窗优先于整屏。

### 游戏能做到什么程度

| 类型 | UI 树 | 截图 | 输入 | 支持程度 |
| --- | --- | --- | --- | --- |
| Win32 / WPF / WinForms / WinUI | 完整 | 可以 | 可以 | 完整支持，基本零截屏 |
| Electron（VS Code、Slack、Discord…） | 默认稀疏 | 可以 | 可以 | 支持；检测到没有辅助树时提示用 `--force-renderer-accessibility` 重开应用 |
| Qt 自绘 / canvas 界面 | 稀疏或空 | 可以 | 可以 | P5 视觉通道 |
| Unity / Unreal **窗口化** | 通常为空 | 可以 | 多数可用 | P4 + P5 |
| **独占全屏** DirectX 11/12 | 空 | **不可以** | 多数可用 | 只能改用「无边框窗口化」 |
| 带反外挂的在线游戏 | — | — | **被拦截** | 不支持 |

独占全屏抓不到画面是 DWM 的机制限制，不是实现问题。明确不做：DirectX 钩子、驱动级输入注入、内存读写、注入 DLL——这些是外挂技术，不属于桌面自动化。

## 许可证

MIT
