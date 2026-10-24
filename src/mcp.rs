use std::io::{BufRead, Write};

use serde_json::{json, Value};

use crate::tools;

const SUPPORTED_VERSIONS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];
const LATEST_VERSION: &str = "2025-06-18";

const INSTRUCTIONS: &str = "\
DeskMCP 把 Windows 桌面能力暴露为工具。优先走语义路径，最后才用像素路径。

标准应用通常只需要 2 次调用：
1. list_windows 找到目标窗口的 hwnd；
2. observe {hwnd} 读取控件树，每个控件带一个 id，例如 `e3a1 Button \"保存\" aid=btnSave @120,340,80x26`；
3. act {hwnd, steps:[{do:\"click_element\",id:\"e3a1\"},{do:\"set_value\",id:\"e7\",text:\"...\"},{do:\"wait_for\",name:\"完成\"}]}
   一次调用把整串操作执行完，结束时自动附带截图。

observe 的 id 在同一窗口内是稳定的，可以跨多次调用复用；界面大改之后再重新 observe。
click_element / set_value 内部会自动选择合适的 UI Automation 动作，不需要坐标，也不需要知道控件类型。

只有 capabilities.uia=false 的窗口（游戏、canvas、自绘界面）才退回 screenshot + click 的坐标方式。
坐标是屏幕物理像素；click / scroll 传 hwnd 时相对该窗口客户区左上角。
screenshot 返回的图片可能被缩小，返回文本里给出了缩放比例，换算坐标时要用。
最小化的窗口先 focus_window，否则截图会失败。
输入事件对以管理员权限运行的窗口无效（UIPI 限制）。";

fn error_response(id: Value, code: i64, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
    .to_string()
}

fn initialize_result(params: &Value) -> Value {
    let requested = params
        .get("protocolVersion")
        .and_then(|v| v.as_str())
        .unwrap_or(LATEST_VERSION);
    let version = if SUPPORTED_VERSIONS.contains(&requested) {
        requested
    } else {
        LATEST_VERSION
    };
    json!({
        "protocolVersion": version,
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "serverInfo": {
            "name": "deskmcp",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": INSTRUCTIONS
    })
}

pub fn handle_line(line: &str) -> Option<String> {
    let message: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(error) => {
            return Some(error_response(Value::Null, -32700, &format!("JSON 解析失败：{error}")))
        }
    };

    let id = message.get("id").cloned();
    let method = message.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let params = message.get("params").cloned().unwrap_or(Value::Null);

    // 通知没有 id，按协议不回复
    let Some(id) = id else {
        return None;
    };

    match method {
        "initialize" => Some(
            json!({ "jsonrpc": "2.0", "id": id, "result": initialize_result(&params) }).to_string(),
        ),
        "ping" => Some(json!({ "jsonrpc": "2.0", "id": id, "result": {} }).to_string()),
        "tools/list" => Some(
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": tools::definitions() }
            })
            .to_string(),
        ),
        "tools/call" => match tools::call(&params) {
            Ok(result) => Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()),
            Err((code, message)) => Some(error_response(id, code, &message)),
        },
        "" => Some(error_response(id, -32600, "缺少 method 字段")),
        other => Some(error_response(id, -32601, &format!("不支持的方法：{other}"))),
    }
}

/// stdio 传输：一行一条 JSON-RPC 消息，日志全部走 stderr，绝不污染 stdout。
pub fn serve_stdio() {
    eprintln!(
        "deskmcp {} 已启动，等待 MCP 客户端（stdio）",
        env!("CARGO_PKG_VERSION")
    );
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(response) = handle_line(trimmed) {
            if writeln!(writer, "{response}").is_err() {
                break;
            }
            let _ = writer.flush();
        }
    }
    // 读循环结束（stdin 关闭）后先把缓存的 UIA 元素放掉：留到 thread_local
    // 析构时释放会卡进 OLE32 的线程分离路径，进程就再也不退出了。
    crate::win::uia::clear_cache();
}
