mod mcp;
mod tools;
mod win;

fn self_test(shot: Option<&str>) {
    println!("deskmcp {}", env!("CARGO_PKG_VERSION"));

    let (x, y, w, h) = win::virtual_screen();
    println!("虚拟桌面: {w}×{h} @ ({x}, {y})");

    let windows = win::window::list_windows(false, None);
    println!("可见顶层窗口: {} 个", windows.len());
    for info in windows.iter().take(8) {
        println!(
            "  hwnd={} pid={} {} | {}",
            info.hwnd, info.pid, info.process, info.title
        );
    }

    match win::capture::capture_screen() {
        Ok(image) => println!("全屏截图: {}×{}", image.width(), image.height()),
        Err(error) => println!("全屏截图失败: {error:#}"),
    }

    // 把鼠标移到当前位置：验证 SendInput 通道没有被安全软件或 UIPI 拦截，又不打扰用户
    let mut point = windows::Win32::Foundation::POINT::default();
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut point);
    }
    match win::input::move_to(point.x, point.y) {
        Ok(()) => println!("输入通道: 可用（鼠标停在原位）"),
        Err(error) => println!("输入通道不可用: {error:#}"),
    }

    // 窗口级截图走 PrintWindow，和被遮挡时能否拿到画面有关，单独验一次
    if let Some(target) = windows.iter().find(|info| !info.minimized && info.rect.width > 0) {
        match win::capture::capture_window(target.hwnd) {
            Ok(image) => println!(
                "窗口截图: \"{}\" {}×{}",
                target.title,
                image.width(),
                image.height()
            ),
            Err(error) => println!("窗口截图失败: {error:#}"),
        }
    }

    match win::clipboard::read_text() {
        Ok(text) => println!("剪贴板可读: {} 个字符", text.chars().count()),
        Err(error) => println!("剪贴板读取失败: {error:#}"),
    }

    if let Some(path) = shot {
        match win::capture::capture_screen() {
            Ok(image) => match image.save(path) {
                Ok(()) => println!("截图已保存: {path}"),
                Err(error) => println!("截图保存失败: {error}"),
            },
            Err(error) => println!("截图失败: {error:#}"),
        }
    }

    // 协议自检：不看真实客户端，直接喂一条 initialize 和一条 tools/list
    let init = mcp::handle_line(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"self-test","version":"0"}}}"#,
    );
    println!("initialize 应答: {}", init.is_some());
    let list = mcp::handle_line(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#);
    if let Some(text) = list {
        let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        let count = value["result"]["tools"]
            .as_array()
            .map(|tools| tools.len())
            .unwrap_or(0);
        println!("tools/list 返回 {count} 个工具");
    }
}

fn main() {
    win::init_dpi();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("deskmcp {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("deskmcp {} — Windows 桌面能力 MCP 服务器", env!("CARGO_PKG_VERSION"));
        println!("用法:");
        println!("  deskmcp                     以 stdio 方式运行（给 MCP 客户端用）");
        println!("  deskmcp --self-test         本机自检，打印窗口/截图/协议结果");
        println!("  deskmcp --self-test --shot <path>  自检并保存一张全屏截图");
        println!("  deskmcp --version");
        return;
    }
    if args.iter().any(|a| a == "--print-config") {
        let exe = std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "deskmcp.exe".to_string());
        let exe = exe.replace('\\', "\\\\");
        println!("把下面这段合并进 MCP 客户端配置的 mcpServers 字段：");
        println!();
        println!(
            "{{\n  \"mcpServers\": {{\n    \"deskmcp\": {{\n      \"command\": \"{exe}\",\n      \"args\": []\n    }}\n  }}\n}}"
        );
        return;
    }
    if args.iter().any(|a| a == "--self-test") {
        let shot = args
            .iter()
            .position(|a| a == "--shot")
            .and_then(|index| args.get(index + 1))
            .map(|s| s.as_str());
        self_test(shot);
        return;
    }

    mcp::serve_stdio();
}
