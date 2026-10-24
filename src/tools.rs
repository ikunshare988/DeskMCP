use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use serde_json::{json, Value};

use crate::win::{capture, clipboard, input, uia, window};

fn text_result(text: impl Into<String>) -> Value {
    json!({ "content": [ { "type": "text", "text": text.into() } ] })
}

fn error_result(message: impl Into<String>) -> Value {
    json!({
        "content": [ { "type": "text", "text": message.into() } ],
        "isError": true
    })
}

pub fn definitions() -> Value {
    json!([
        {
            "name": "list_windows",
            "description": "列出桌面上的顶层窗口。返回每个窗口的 hwnd、标题、类名、进程名和屏幕矩形。后续所有针对窗口的操作都用返回的 hwnd。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "include_invisible": {
                        "type": "boolean",
                        "description": "是否包含不可见窗口，默认 false。"
                    },
                    "title_contains": {
                        "type": "string",
                        "description": "只返回标题或进程名包含该字符串的窗口（不区分大小写）。"
                    }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "get_window",
            "description": "读取单个窗口的详细信息，并返回该窗口当前支持的能力层级（window / input / vision / uia）。模型应先看 capabilities 再决定用坐标操作还是快捷键。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "hwnd": { "type": "integer", "description": "list_windows 返回的 hwnd。" }
                },
                "required": ["hwnd"],
                "additionalProperties": false
            }
        },
        {
            "name": "focus_window",
            "description": "把窗口切到前台并解除最小化。会等 60ms 让窗口进入可交互状态。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "hwnd": { "type": "integer" }
                },
                "required": ["hwnd"],
                "additionalProperties": false
            }
        },
        {
            "name": "screenshot",
            "description": "截图并返回 PNG 图片。不传 hwnd 时抓整个虚拟桌面；传 hwnd 时抓该窗口（被遮挡也能抓到）。返回值附带像素尺寸，模型据此换算点击坐标。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "hwnd": {
                        "type": "integer",
                        "description": "可选。目标窗口；省略则截全屏。"
                    },
                    "max_width": {
                        "type": "integer",
                        "description": "可选。图片最大宽度，默认 1280，用于控制上下文体积。传 0 表示不缩放。"
                    }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "observe",
            "description": "读取窗口的控件树（UI Automation），每个控件带一个稳定 id，例如 `e3a1 Button \"保存\" aid=btnSave @120,340,80x26`。拿到 id 后直接用 click_element / set_value 操作，不需要截图也不需要坐标。一次 observe 的结果可以支撑后续多步操作，不要每步都重新观察。不可见的控件不会出现。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "hwnd": { "type": "integer", "description": "目标窗口。" },
                    "max_depth": {
                        "type": "integer",
                        "description": "最大层数，默认 12。窗口特别复杂时可以调到 6 只看上层。"
                    },
                    "max_elements": {
                        "type": "integer",
                        "description": "最多返回多少个控件，默认 400。"
                    },
                    "include_popups": {
                        "type": "boolean",
                        "description": "是否把该窗口弹出的菜单/下拉框/对话框一并合并进来，默认 true。合并的浮层会标成 Popup 并缩进一层。"
                    }
                },
                "required": ["hwnd"],
                "additionalProperties": false
            }
        },
        {
            "name": "find",
            "description": "按条件查找控件，只返回命中的那些——不必先把整棵树读回来。适合「保存按钮在哪」「这个对话框里有哪些输入框」这类问题，也绕开了 observe 一次最多 400 个元素的限制。命中的 id 可直接交给 click_element / read / set_value，并且不会冲掉该窗口已有的 id。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "hwnd": { "type": "integer", "description": "目标窗口。" },
                    "name": { "type": "string", "description": "控件名称包含该字符串（忽略大小写）。" },
                    "exact_name": { "type": "boolean", "description": "true 时名称必须完全相等，默认 false。" },
                    "automation_id": { "type": "string", "description": "AutomationId 完全相等，例如 ViewMode_LargeIcons。" },
                    "control_type": { "type": "string", "description": "控件类型，如 Button / Edit / ListItem / MenuItem，也可用数字（50000 = Button）。" },
                    "class_name": { "type": "string", "description": "控件类名包含该字符串（忽略大小写）。" },
                    "enabled": { "type": "boolean", "description": "只看可用（true）或只看禁用（false）的控件。" },
                    "include_offscreen": { "type": "boolean", "description": "是否包含不可见控件，默认 false。" },
                    "within": { "type": "string", "description": "限定在某个控件的子树里查找，传 observe / find 返回的 id。" },
                    "max_depth": { "type": "integer", "description": "最大层数，默认 12。" },
                    "limit": { "type": "integer", "description": "最多返回几个匹配，默认 20。" }
                },
                "required": ["hwnd"],
                "additionalProperties": false
            }
        },
        {
            "name": "read",
            "description": "读控件当前状态：值、开关、选中、展开、范围、是否可用/可见/有焦点、矩形。用来确认上一步操作是否真的生效——比再截一张图便宜三个数量级，也不需要换算坐标。传 ids 可以一次读多个。出于安全考虑：密码框内容一律不回显；文档类控件的正文只报字符数、不回显内容；其余值最长回显 120 字符。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "单个控件 id。" },
                    "ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "多个控件 id；与 id 二选一。"
                    }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "click_element",
            "description": "按 id 触发控件。内部会依次尝试 Invoke / Toggle / Select / Expand / 默认动作，都不支持时才点控件中心。如果控件不在视野里（滚出去了、或虚拟化列表里没实体化），会先自动滚进来/实体化再操作。调用方不需要知道控件类型，也不需要坐标。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "observe / find 返回的控件 id，如 e3a1。" }
                },
                "required": ["id"],
                "additionalProperties": false
            }
        },
        {
            "name": "invoke",
            "description": "对控件执行一个明确指定的动作，不做猜测。什么时候用它而不是 click_element：滑块/进度条设值（set_range）、明确展开或收起（expand/collapse）、把某项滚进视野（scroll_into_view）、窗口最大化/最小化/关闭、只想聚焦不点击（focus）。动作不支持时会明确报错，而不是悄悄换成点击。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "控件 id。" },
                    "action": {
                        "type": "string",
                        "enum": [
                            "invoke", "toggle", "select", "expand", "collapse",
                            "scroll_into_view", "realize", "default_action",
                            "set_value", "set_range", "focus", "click_center",
                            "close", "maximize", "minimize", "restore"
                        ],
                        "description": "要执行的动作。"
                    },
                    "value": {
                        "type": "string",
                        "description": "set_value / set_range 需要的值；set_range 要求是数字。"
                    }
                },
                "required": ["id", "action"],
                "additionalProperties": false
            }
        },
        {
            "name": "set_value",
            "description": "按 id 把文本写进输入框/下拉框。控件支持 ValuePattern 时直接赋值；否则聚焦后全选替换。比「点击 + 全选 + 输入」省两次调用。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "text": { "type": "string" }
                },
                "required": ["id", "text"],
                "additionalProperties": false
            }
        },
        {
            "name": "act",
            "description": "一次调用按顺序执行多个步骤，任一步失败即停止（stop_on_error=false 可继续）。这是最省往返的用法：把点击、填值、快捷键、等待、截图合成一次调用。步骤对象用 do 字段区分：click_element{id} / invoke{id,action,value?} / set_value{id,text} / click{x,y,hwnd?} / type_text{text} / press_keys{keys} / scroll{x,y,amount} 或 scroll{item} / wait{ms} / wait_for{name,timeout_ms} / focus_window{hwnd}。默认结束时附带一张截图，用于确认结果。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "hwnd": {
                        "type": "integer",
                        "description": "可选。作为步骤里未指定 hwnd 时的默认值，也用于最后的截图。"
                    },
                    "steps": {
                        "type": "array",
                        "description": "按顺序执行的步骤。",
                        "items": { "type": "object" }
                    },
                    "stop_on_error": {
                        "type": "boolean",
                        "description": "失败是否立即停止，默认 true。"
                    },
                    "screenshot_at_end": {
                        "type": "boolean",
                        "description": "结束时是否截图，默认 true。"
                    },
                    "max_width": {
                        "type": "integer",
                        "description": "结尾截图的最大宽度，默认 1280。"
                    }
                },
                "required": ["steps"],
                "additionalProperties": false
            }
        },
        {
            "name": "click",
            "description": "移动鼠标并点击。坐标默认是屏幕物理像素；传 hwnd 时坐标相对该窗口客户区左上角。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "x": { "type": "integer" },
                    "y": { "type": "integer" },
                    "button": {
                        "type": "string",
                        "enum": ["left", "right", "middle"],
                        "description": "默认 left。"
                    },
                    "count": {
                        "type": "integer",
                        "description": "点击次数，2 表示双击，默认 1。"
                    },
                    "hwnd": {
                        "type": "integer",
                        "description": "可选。提供时 x/y 相对于该窗口客户区。"
                    }
                },
                "required": ["x", "y"],
                "additionalProperties": false
            }
        },
        {
            "name": "drag",
            "description": "按住左键从起点拖到终点，中途按 16ms 步进插值，模拟真人拖动。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from_x": { "type": "integer" },
                    "from_y": { "type": "integer" },
                    "to_x": { "type": "integer" },
                    "to_y": { "type": "integer" },
                    "duration_ms": {
                        "type": "integer",
                        "description": "拖动总时长，默认 400。"
                    }
                },
                "required": ["from_x", "from_y", "to_x", "to_y"],
                "additionalProperties": false
            }
        },
        {
            "name": "scroll",
            "description": "滚动。三种用法：1) 给 item（控件 id）→ 把它滚进视野（虚拟化列表里也能用，不依赖坐标）；2) 给 container_id + direction → 让该容器自己滚动；3) 给 x/y → 在该位置滚鼠标滚轮。amount 以格为单位，正数向上/向前，负数向下/向后。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "x": { "type": "integer", "description": "滚轮位置（与 item / container_id 三选一）。" },
                    "y": { "type": "integer" },
                    "amount": { "type": "integer", "description": "默认 3。" },
                    "item": { "type": "string", "description": "要滚进视野的控件 id。" },
                    "container_id": { "type": "string", "description": "要滚动的容器控件 id。" },
                    "direction": {
                        "type": "string",
                        "enum": ["up", "down", "left", "right"],
                        "description": "配合 container_id 使用，默认 down。"
                    }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "type_text",
            "description": "输入一段 Unicode 文本（中文、emoji 都可以）。走 SendInput 的 Unicode 通道，不经过剪贴板，不改动用户剪贴板内容。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                },
                "required": ["text"],
                "additionalProperties": false
            }
        },
        {
            "name": "press_keys",
            "description": "发送组合键，例如 \"ctrl+shift+s\"、\"alt+f4\"、\"win+r\"、\"enter\"。支持的键名见 tool 说明，修饰键为 ctrl/shift/alt/win。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "keys": {
                        "type": "string",
                        "description": "用 + 连接的组合键，例如 ctrl+alt+delete 之外都可以：ctrl、shift、alt、win、a-z、0-9、f1-f24、enter、tab、esc、space、backspace、delete、insert、home、end、pageup、pagedown、up、down、left、right、plus、minus、comma、period、slash 等。"
                    }
                },
                "required": ["keys"],
                "additionalProperties": false
            }
        },
        {
            "name": "clipboard_read",
            "description": "读取剪贴板纯文本。",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "clipboard_write",
            "description": "把纯文本写入剪贴板，会覆盖用户原有内容。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                },
                "required": ["text"],
                "additionalProperties": false
            }
        }
    ])
}

fn int_arg(args: &Value, key: &str) -> Result<i64> {
    args.get(key)
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow!("缺少整数参数 {key}"))
}

fn opt_int(args: &Value, key: &str) -> Option<i64> {
    args.get(key).and_then(|v| v.as_i64())
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("缺少字符串参数 {key}"))
}

fn resolve_point(args: &Value) -> Result<(i32, i32)> {
    let x = int_arg(args, "x")? as i32;
    let y = int_arg(args, "y")? as i32;
    match opt_int(args, "hwnd") {
        Some(hwnd) => {
            let (ox, oy) = window::client_origin(hwnd)?;
            Ok((ox + x, oy + y))
        }
        None => Ok((x, y)),
    }
}

fn screenshot(args: &Value) -> Result<Value> {
    let max_width = opt_int(args, "max_width").unwrap_or(1280).max(0) as u32;
    let (image, source) = match opt_int(args, "hwnd") {
        Some(hwnd) => {
            let info = window::get(hwnd)?;
            (
                capture::capture_window(hwnd)?,
                format!(
                    "窗口 hwnd={} \"{}\"，屏幕矩形 x={} y={} w={} h={}",
                    info.hwnd, info.title, info.rect.x, info.rect.y, info.rect.width, info.rect.height
                ),
            )
        }
        None => {
            let (x, y, w, h) = crate::win::virtual_screen();
            (
                capture::capture_screen()?,
                format!("全屏，虚拟桌面 x={x} y={y} w={w} h={h}"),
            )
        }
    };
    let original = (image.width(), image.height());
    let image = capture::downscale(image, max_width);
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image.clone())
        .write_to(&mut png, image::ImageFormat::Png)
        .context("PNG 编码失败")?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
    let scale = image.width() as f64 / original.0 as f64;
    let mut note = format!(
        "{}；原图 {}×{}，输出 {}×{}。",
        source,
        original.0,
        original.1,
        image.width(),
        image.height()
    );
    if scale < 0.999 {
        note.push_str(&format!(
            "图片已缩小，比例为 {:.3}：在图片上量到的坐标除以 {:.3} 再叠加原图偏移，才是屏幕坐标。",
            scale, scale
        ));
    }
    Ok(json!({
        "content": [
            { "type": "image", "data": encoded, "mimeType": "image/png" },
            { "type": "text", "text": note }
        ]
    }))
}

fn step_point(step: &Value) -> Result<(i32, i32)> {
    let x = int_arg(step, "x")? as i32;
    let y = int_arg(step, "y")? as i32;
    match opt_int(step, "hwnd") {
        Some(hwnd) => {
            let (ox, oy) = window::client_origin(hwnd)?;
            Ok((ox + x, oy + y))
        }
        None => Ok((x, y)),
    }
}

fn wait_for(hwnd: i64, name: &str, timeout_ms: u64) -> Result<String> {
    let needle = name.to_lowercase();
    let started = std::time::Instant::now();
    loop {
        if let Ok(found) = uia::observe(hwnd, 12, 600, true) {
            let hit = found.nodes.iter().any(|node| {
                node.name.to_lowercase().contains(&needle)
                    || node.automation_id.to_lowercase().contains(&needle)
            });
            if hit {
                return Ok(format!("命中 \"{name}\"（{}ms）", started.elapsed().as_millis()));
            }
        }
        if started.elapsed().as_millis() as u64 >= timeout_ms {
            return Err(anyhow!("等待 \"{name}\" 超时（{timeout_ms}ms）"));
        }
        crate::win::sleep_ms(200);
    }
}

fn run_step(step: &Value, default_hwnd: Option<i64>) -> Result<String> {
    let action = step
        .get("do")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("步骤缺少 do 字段"))?;
    match action {
        "click_element" => {
            let id = str_arg(step, "id")?;
            Ok(uia::activate(id)?)
        }
        "invoke" => {
            let id = str_arg(step, "id")?;
            let action = str_arg(step, "action")?;
            let value = step.get("value").and_then(|v| v.as_str());
            Ok(format!("{id} → {}", uia::invoke(id, action, value)?))
        }
        "set_value" => {
            let id = str_arg(step, "id")?;
            let text = str_arg(step, "text")?;
            let (how, verified) = uia::set_value(id, text)?;
            Ok(match verified {
                Some(true) => format!("{how}，已回读确认"),
                Some(false) => format!("{how}，回读内容不一致"),
                None => format!("{how}，回读无法校验"),
            })
        }
        "click" => {
            let (x, y) = step_point(step)?;
            let button = step.get("button").and_then(|v| v.as_str()).unwrap_or("left");
            let count = opt_int(step, "count").unwrap_or(1).max(1) as u32;
            input::click(x, y, button, count)?;
            Ok(format!("({x}, {y}) {button} ×{count}"))
        }
        "type_text" => {
            let text = str_arg(step, "text")?;
            input::type_text(text)?;
            Ok(format!("输入 {} 个字符", text.chars().count()))
        }
        "press_keys" => {
            let keys = str_arg(step, "keys")?;
            input::press_keys(keys)?;
            Ok(keys.to_string())
        }
        "scroll" => {
            if let Some(item) = step.get("item").and_then(|v| v.as_str()) {
                Ok(format!("{item} → {}", uia::scroll_into_view(item)?))
            } else {
                let (x, y) = step_point(step)?;
                let amount = opt_int(step, "amount").unwrap_or(3) as i32;
                input::scroll(x, y, amount)?;
                Ok(format!("滚动 {amount} 格"))
            }
        }
        "wait" => {
            let ms = opt_int(step, "ms").unwrap_or(300).clamp(0, 30_000) as u64;
            crate::win::sleep_ms(ms);
            Ok(format!("等待 {ms}ms"))
        }
        "wait_for" => {
            let name = str_arg(step, "name")?;
            let hwnd = opt_int(step, "hwnd")
                .or(default_hwnd)
                .ok_or_else(|| anyhow!("wait_for 需要 hwnd"))?;
            let timeout = opt_int(step, "timeout_ms").unwrap_or(5000).clamp(100, 60_000) as u64;
            wait_for(hwnd, name, timeout)
        }
        "focus_window" => {
            let hwnd = int_arg(step, "hwnd")?;
            let info = window::focus(hwnd)?;
            Ok(format!("\"{}\"", info.title))
        }
        other => Err(anyhow!("不支持的步骤 {other}")),
    }
}

fn act(args: &Value) -> Result<Value> {
    let steps = args
        .get("steps")
        .and_then(|value| value.as_array())
        .cloned()
        .ok_or_else(|| anyhow!("steps 必须是数组"))?;
    let default_hwnd = opt_int(args, "hwnd");
    let stop_on_error = args
        .get("stop_on_error")
        .and_then(|value| value.as_bool())
        .unwrap_or(true);

    let mut log = String::new();
    let mut failed = false;
    for (index, step) in steps.iter().enumerate() {
        let label = step.get("do").and_then(|value| value.as_str()).unwrap_or("?");
        match run_step(step, default_hwnd) {
            Ok(note) => log.push_str(&format!("{}. {label} → {note}\n", index + 1)),
            Err(error) => {
                log.push_str(&format!("{}. {label} → 失败：{error:#}\n", index + 1));
                failed = true;
                if stop_on_error {
                    break;
                }
            }
        }
    }

    let mut value = json!({
        "content": [ { "type": "text", "text": log.trim_end().to_string() } ]
    });
    let want_shot = args
        .get("screenshot_at_end")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    if want_shot {
        let mut shot = json!({ "max_width": opt_int(args, "max_width").unwrap_or(1280) });
        if let Some(hwnd) = default_hwnd {
            shot["hwnd"] = json!(hwnd);
        }
        if let Ok(result) = screenshot(&shot) {
            if let (Some(items), Some(target)) = (
                result.get("content").and_then(|v| v.as_array()),
                value.get_mut("content").and_then(|v| v.as_array_mut()),
            ) {
                target.extend(items.iter().cloned());
            }
        }
    }
    if failed {
        value["isError"] = json!(true);
    }
    Ok(value)
}

pub fn call(params: &Value) -> std::result::Result<Value, (i64, String)> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or((-32602i64, "缺少 name 参数".to_string()))?;
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let outcome: Result<Value> = (|| {
        Ok(match name {
            "list_windows" => {
                let include_invisible = args
                    .get("include_invisible")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let title_contains = args.get("title_contains").and_then(|v| v.as_str());
                let list = window::list_windows(include_invisible, title_contains);
                text_result(serde_json::to_string_pretty(&list).unwrap_or_default())
            }
            "get_window" => {
                let hwnd = int_arg(&args, "hwnd")?;
                let info = window::get(hwnd)?;
                let caps = window::capabilities(hwnd)?;
                text_result(
                    serde_json::to_string_pretty(&json!({
                        "window": info,
                        "capabilities": caps
                    }))
                    .unwrap_or_default(),
                )
            }
            "observe" => {
                let hwnd = int_arg(&args, "hwnd")?;
                let depth = opt_int(&args, "max_depth").unwrap_or(12).clamp(1, 32) as u32;
                let max_elements = opt_int(&args, "max_elements").unwrap_or(400).clamp(1, 3000) as usize;
                let include_popups = args
                    .get("include_popups")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let found = uia::observe(hwnd, depth, max_elements, include_popups)?;
                let info = window::get(hwnd)?;
                let mut text = format!(
                    "窗口 \"{}\" hwnd={} 控件 {} 个\n{}",
                    info.title,
                    info.hwnd,
                    found.nodes.len(),
                    uia::render(&found.nodes)
                );
                if found.truncated {
                    // 如实说明「上面不是全部」，否则模型会以为没有别的控件了
                    text.push_str(&format!(
                        "注意：已达上限（{}），上面列出的不是全部控件。用 find 按条件精确查找，或收窄 max_depth、提高 max_elements。\n",
                        found.stop_reason.unwrap_or("未知")
                    ));
                }
                text_result(text)
            }
            "find" => {
                let hwnd = int_arg(&args, "hwnd")?;
                let depth = opt_int(&args, "max_depth").unwrap_or(12).clamp(1, 32) as u32;
                let limit = opt_int(&args, "limit").unwrap_or(20).clamp(1, 500) as usize;
                let control_type = match args.get("control_type") {
                    None => None,
                    Some(Value::String(name)) => Some(
                        uia::control_type_from_name(name)
                            .ok_or_else(|| anyhow!("不认识的控件类型：{name}"))?,
                    ),
                    Some(other) => Some(
                        other
                            .as_i64()
                            .ok_or_else(|| anyhow!("control_type 需要字符串或数字"))? as i32,
                    ),
                };
                let filter = uia::Filter {
                    name: args.get("name").and_then(|v| v.as_str()).map(str::to_string),
                    exact_name: args
                        .get("exact_name")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    automation_id: args
                        .get("automation_id")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    control_type,
                    class_name: args
                        .get("class_name")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    enabled: args.get("enabled").and_then(|v| v.as_bool()),
                    include_offscreen: args
                        .get("include_offscreen")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                };
                let within = args.get("within").and_then(|v| v.as_str());
                let found = uia::find(hwnd, &filter, within, depth, limit)?;
                let info = window::get(hwnd)?;
                let mut text = format!(
                    "窗口 \"{}\" hwnd={} 命中 {} 个控件（访问 {} 个节点）\n",
                    info.title,
                    info.hwnd,
                    found.nodes.len(),
                    found.visited
                );
                if found.nodes.is_empty() {
                    text.push_str("没有命中。条件可能太严，或该控件不在这棵 UI Automation 树里。\n");
                } else {
                    text.push_str(&uia::render(&found.nodes));
                }
                if found.truncated {
                    text.push_str(&format!(
                        "注意：已达上限（{}），命中结果可能不完整。用 within 收窄范围或调大 limit。\n",
                        found.stop_reason.unwrap_or("未知")
                    ));
                }
                text_result(text)
            }
            "read" => {
                let mut ids: Vec<String> = Vec::new();
                if let Some(id) = args.get("id").and_then(|v| v.as_str()) {
                    ids.push(id.to_string());
                }
                if let Some(list) = args.get("ids").and_then(|v| v.as_array()) {
                    for item in list {
                        if let Some(id) = item.as_str() {
                            ids.push(id.to_string());
                        }
                    }
                }
                if ids.is_empty() {
                    bail!("需要 id 或 ids");
                }
                let mut lines = Vec::new();
                let mut failed = 0usize;
                for id in &ids {
                    match uia::read_state(id) {
                        Ok(line) => lines.push(line),
                        Err(error) => {
                            failed += 1;
                            lines.push(format!("{id} 读取失败：{error:#}"));
                        }
                    }
                }
                if failed == ids.len() {
                    // 全都没读到：当成错误返回，别让调用方以为读到了
                    bail!("{}", lines.join("\n"));
                }
                text_result(lines.join("\n"))
            }
            "click_element" => {
                let id = str_arg(&args, "id")?;
                let how = uia::activate(id)?;
                text_result(format!("{id} → {how}"))
            }
            "invoke" => {
                let id = str_arg(&args, "id")?;
                let action = str_arg(&args, "action")?;
                let value = args.get("value").and_then(|v| v.as_str());
                let how = uia::invoke(id, action, value)?;
                text_result(format!("{id} → {how}"))
            }
            "set_value" => {
                let id = str_arg(&args, "id")?;
                let text = str_arg(&args, "text")?;
                let (how, verified) = uia::set_value(id, text)?;
                let count = text.chars().count();
                let note = match verified {
                    Some(true) => "已回读确认",
                    Some(false) => "回读内容不一致，建议先 observe 确认",
                    None => "已输入，文档类控件无法回读校验，请用截图确认",
                };
                text_result(format!("{id} → {how}（{count} 个字符，{note}）"))
            }
            "act" => act(&args)?,
            "focus_window" => {
                let hwnd = int_arg(&args, "hwnd")?;
                let info = window::focus(hwnd)?;
                text_result(format!(
                    "已切到前台：hwnd={} \"{}\" 矩形 x={} y={} w={} h={}",
                    info.hwnd, info.title, info.rect.x, info.rect.y, info.rect.width, info.rect.height
                ))
            }
            "screenshot" => screenshot(&args)?,
            "click" => {
                let (x, y) = resolve_point(&args)?;
                let button = args.get("button").and_then(|v| v.as_str()).unwrap_or("left");
                let count = opt_int(&args, "count").unwrap_or(1).max(1) as u32;
                input::click(x, y, button, count)?;
                text_result(format!("已在 ({x}, {y}) 点击 {button} ×{count}"))
            }
            "drag" => {
                let x1 = int_arg(&args, "from_x")? as i32;
                let y1 = int_arg(&args, "from_y")? as i32;
                let x2 = int_arg(&args, "to_x")? as i32;
                let y2 = int_arg(&args, "to_y")? as i32;
                let duration = opt_int(&args, "duration_ms").unwrap_or(400).clamp(50, 10_000) as u32;
                input::drag(x1, y1, x2, y2, duration)?;
                text_result(format!("已从 ({x1}, {y1}) 拖到 ({x2}, {y2})，耗时 {duration}ms"))
            }
            "scroll" => {
                if let Some(item) = args.get("item").and_then(|v| v.as_str()) {
                    let how = uia::scroll_into_view(item)?;
                    text_result(format!("{item} → {how}"))
                } else if let Some(container) = args.get("container_id").and_then(|v| v.as_str()) {
                    let direction = args
                        .get("direction")
                        .and_then(|v| v.as_str())
                        .unwrap_or("down");
                    let amount = opt_int(&args, "amount").unwrap_or(3);
                    let how = uia::scroll_container(container, direction, amount)?;
                    text_result(format!("{container} → {how}"))
                } else {
                    let (x, y) = resolve_point(&args)?;
                    let amount = opt_int(&args, "amount").unwrap_or(3) as i32;
                    input::scroll(x, y, amount)?;
                    text_result(format!("已在 ({x}, {y}) 滚动 {amount} 格"))
                }
            }
            "type_text" => {
                let text = str_arg(&args, "text")?;
                input::type_text(text)?;
                text_result(format!("已输入 {} 个字符", text.chars().count()))
            }
            "press_keys" => {
                let keys = str_arg(&args, "keys")?;
                input::press_keys(keys)?;
                text_result(format!("已发送组合键 {keys}"))
            }
            "clipboard_read" => text_result(clipboard::read_text()?),
            "clipboard_write" => {
                let text = str_arg(&args, "text")?;
                clipboard::write_text(text)?;
                text_result(format!("已写入剪贴板（{} 个字符）", text.chars().count()))
            }
            other => bail!("未知工具：{other}"),
        })
    })();

    match outcome {
        Ok(value) => Ok(value),
        Err(error) => Ok(error_result(format!("{error:#}"))),
    }
}
