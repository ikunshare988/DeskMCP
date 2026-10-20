use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use serde_json::{json, Value};

use crate::win::{capture, clipboard, input, window};

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
