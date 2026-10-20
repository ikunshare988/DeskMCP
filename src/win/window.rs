use std::ffi::c_void;

use anyhow::Result;
use serde::Serialize;
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, POINT, RECT, TRUE};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GetClassNameW, GetForegroundWindow,
    GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    SetForegroundWindow, ShowWindow, SW_RESTORE,
};

use super::{hwnd_value, to_hwnd};

// DWM 属性值；这里用本地声明避免不同 windows crate 版本对参数类型的差异。
const DWMWA_EXTENDED_FRAME_BOUNDS: u32 = 9;
const DWMWA_CLOAKED: u32 = 14;

#[link(name = "dwmapi")]
extern "system" {
    fn DwmGetWindowAttribute(hwnd: HWND, attr: u32, pv: *mut c_void, cb: u32) -> i32;
}

#[derive(Debug, Clone, Serialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct WindowInfo {
    pub hwnd: i64,
    pub title: String,
    pub class: String,
    pub pid: u32,
    pub process: String,
    pub rect: Rect,
    pub visible: bool,
    pub minimized: bool,
    pub foreground: bool,
}

/// 该窗口当前可以走哪几层能力。模型据此决定用语义操作还是坐标操作。
#[derive(Debug, Clone, Serialize)]
pub struct Capabilities {
    /// 窗口层：枚举、聚焦、移动、抓屏
    pub window: bool,
    /// 输入层：合成鼠标键盘事件
    pub input: bool,
    /// 视觉层：截图 + 坐标定位
    pub vision: bool,
    /// 语义层：UI Automation 控件树（尚未实现）
    pub uia: bool,
    pub notes: Vec<String>,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            window: true,
            input: true,
            vision: true,
            uia: false,
            notes: Vec::new(),
        }
    }
}

fn window_text(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

fn process_name(pid: u32) -> String {
    if pid == 0 {
        return String::new();
    }
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut size);
        let _ = CloseHandle(handle);
        if ok.is_err() {
            return String::new();
        }
        let full = String::from_utf16_lossy(&buf[..size as usize]);
        full.rsplit(['\\', '/']).next().unwrap_or("").to_string()
    }
}

fn window_rect(hwnd: HWND) -> Option<Rect> {
    unsafe {
        let mut frame = RECT::default();
        let hr = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut frame as *mut RECT as *mut c_void,
            std::mem::size_of::<RECT>() as u32,
        );
        if hr == 0 && frame.right > frame.left && frame.bottom > frame.top {
            return Some(Rect {
                x: frame.left,
                y: frame.top,
                width: frame.right - frame.left,
                height: frame.bottom - frame.top,
            });
        }
        let mut raw = RECT::default();
        if GetWindowRect(hwnd, &mut raw).is_err() {
            return None;
        }
        Some(Rect {
            x: raw.left,
            y: raw.top,
            width: raw.right - raw.left,
            height: raw.bottom - raw.top,
        })
    }
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    unsafe {
        let hr = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut c_void,
            std::mem::size_of::<u32>() as u32,
        );
        hr == 0 && cloaked != 0
    }
}

unsafe fn describe(hwnd: HWND, foreground: HWND) -> Option<WindowInfo> {
    if is_cloaked(hwnd) {
        return None;
    }
    let title = window_text(hwnd);
    let visible = IsWindowVisible(hwnd).as_bool();
    if title.trim().is_empty() && !visible {
        return None;
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    Some(WindowInfo {
        hwnd: hwnd_value(hwnd),
        title,
        class: class_name(hwnd),
        pid,
        process: process_name(pid),
        rect: window_rect(hwnd).unwrap_or(Rect { x: 0, y: 0, width: 0, height: 0 }),
        visible,
        minimized: IsIconic(hwnd).as_bool(),
        foreground: hwnd == foreground,
    })
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let (list, foreground) = &mut *(lparam.0 as *mut (Vec<WindowInfo>, HWND));
    if let Some(info) = describe(hwnd, *foreground) {
        list.push(info);
    }
    TRUE
}

/// 枚举顶层窗口。默认只返回可见窗口，并过滤掉被 DWM cloak 的幽灵窗口。
pub fn list_windows(include_invisible: bool, title_contains: Option<&str>) -> Vec<WindowInfo> {
    let mut state: (Vec<WindowInfo>, HWND) = (Vec::new(), unsafe { GetForegroundWindow() });
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut state as *mut _ as isize));
    }
    let (list, _) = state;
    let needle = title_contains.map(|s| s.to_lowercase());
    list.into_iter()
        .filter(|w| include_invisible || (w.visible && !w.title.trim().is_empty()))
        .filter(|w| match &needle {
            Some(n) => w.title.to_lowercase().contains(n) || w.process.to_lowercase().contains(n),
            None => true,
        })
        .collect()
}

pub fn get(hwnd_value: i64) -> Result<WindowInfo> {
    let hwnd = to_hwnd(hwnd_value)?;
    let foreground = unsafe { GetForegroundWindow() };
    unsafe { describe(hwnd, foreground) }.ok_or_else(|| anyhow::anyhow!("窗口不可见或已关闭"))
}

pub fn capabilities(hwnd_value: i64) -> Result<Capabilities> {
    let info = get(hwnd_value)?;
    let mut caps = Capabilities::default();
    caps.uia = super::uia::probe(hwnd_value);
    if info.minimized {
        caps.vision = false;
        caps.notes.push("窗口已最小化，截图会失败；先调用 focus_window 恢复".into());
    }
    if info.rect.width <= 0 || info.rect.height <= 0 {
        caps.vision = false;
        caps.notes.push("窗口尺寸为 0，无法截图".into());
    }
    if caps.uia {
        caps.notes
            .push("支持 observe 按控件名定位，优先用它，不要靠坐标".into());
    } else {
        caps.notes.push(
            "该窗口没有 UI Automation 树（常见于游戏、canvas 应用），只能用截图 + 坐标".into(),
        );
    }
    Ok(caps)
}

/// 把目标窗口切到前台。Windows 对后台进程抢焦点有限制，这里用 AttachThreadInput 绕过。
pub fn focus(hwnd_value: i64) -> Result<WindowInfo> {
    let hwnd = to_hwnd(hwnd_value)?;
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let foreground = GetForegroundWindow();
        let target_thread = GetWindowThreadProcessId(hwnd, None);
        let foreground_thread = GetWindowThreadProcessId(foreground, None);
        let current_thread = GetCurrentThreadId();
        let attached = foreground_thread != 0
            && foreground_thread != current_thread
            && AttachThreadInput(current_thread, foreground_thread, TRUE).as_bool();
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        if attached {
            let _ = AttachThreadInput(current_thread, foreground_thread, BOOL(0));
        }
        let _ = target_thread;
        super::sleep_ms(60);
    }
    get(hwnd_value)
}

/// 客户区左上角在屏幕上的物理像素坐标，用于窗口内相对点击。
pub fn client_origin(hwnd_value: i64) -> Result<(i32, i32)> {
    let hwnd = to_hwnd(hwnd_value)?;
    unsafe {
        let mut point = POINT { x: 0, y: 0 };
        ClientToScreen(hwnd, &mut point).ok()?;
        Ok((point.x, point.y))
    }
}
