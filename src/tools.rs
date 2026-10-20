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
                    }
                },
                "required": ["hwnd"],
                "additionalProperties": false
            }
        },
        {
            "name": "click_element",
            "description": "按 observe 返回的 id 触发控件。内部会依次尝试 Invoke / Toggle / Select / Expand / 默认动作，都不支持时才点控件中心。调用方不需要知道控件类型，也不需要坐标。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "observe 返回的控件 id，如 e3a1。" }
                },
                "required": ["id"],
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
            "description": "一次调用按顺序执行多个步骤，任一步失败即停止（stop_on_error=false 可继续）。这是最省往返的用法：把点击、填值、快捷键、等待、截图合成一次调用。步骤对象用 do 字段区分：click_element{id} / set_value{id,text} / click{x,y,hwnd?} / type_text{text} / press_keys{keys} / scroll{x,y,amount} / wait{ms} / wait_for{name,timeout_ms} / focus_window{hwnd}。默认结束时附带一张截图，用于确认结果。",
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
            "description": "在指定位置滚动滚轮。amount 以格为单位，正数向上、负数向下。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "x": { "type": "integer" },
                    "y": { "type": "integer" },
                    "amount": { "type": "integer", "description": "默认 3。" }
                },
                "required": ["x", "y"],
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
        if let Ok(nodes) = uia::observe(hwnd, 12, 600) {
            let hit = nodes.iter().any(|node| {
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
            Ok(uia::activate(id)?.to_string())
        }
        "set_value" => {
            let id = str_arg(step, "id")?;
            let text = str_arg(step, "text")?;
            let (how, verified) = uia::set_value(id, text)?;
            Ok(if verified {
                format!("{how}，已回读确认")
            } else {
                format!("{how}，回读内容不一致")
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
            let (x, y) = step_point(step)?;
            let amount = opt_int(step, "amount").unwrap_or(3) as i32;
            input::scroll(x, y, amount)?;
            Ok(format!("滚动 {amount} 格"))
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
                let nodes = uia::observe(hwnd, depth, max_elements)?;
                let info = window::get(hwnd)?;
                text_result(format!(
                    "窗口 \"{}\" hwnd={} 控件 {} 个\n{}",
                    info.title,
                    info.hwnd,
                    nodes.len(),
                    uia::render(&nodes)
                ))
            }
            "click_element" => {
                let id = str_arg(&args, "id")?;
                let how = uia::activate(id)?;
                text_result(format!("{id} → {how}"))
            }
            "set_value" => {
                let id = str_arg(&args, "id")?;
                let text = str_arg(&args, "text")?;
                let (how, verified) = uia::set_value(id, text)?;
                let count = text.chars().count();
                text_result(if verified {
                    format!("{id} → {how}（{count} 个字符，已回读确认）")
                } else {
                    format!("{id} → {how}（{count} 个字符，但回读内容不一致，建议先 observe 确认）")
                })
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
                let (x, y) = resolve_point(&args)?;
                let amount = opt_int(&args, "amount").unwrap_or(3) as i32;
                input::scroll(x, y, amount)?;
                text_result(format!("已在 ({x}, {y}) 滚动 {amount} 格"))
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
