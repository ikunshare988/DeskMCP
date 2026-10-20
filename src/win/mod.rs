pub mod capture;
pub mod clipboard;
pub mod input;
pub mod uia;
pub mod window;

use anyhow::Result;

/// 进程级 DPI 感知：必须在任何窗口/坐标 API 之前调用，否则截图像素与鼠标坐标会错位。
pub fn init_dpi() {
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
}

/// 虚拟桌面的物理像素范围 (x, y, width, height)。
pub fn virtual_screen() -> (i32, i32, i32, i32) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

pub fn sleep_ms(ms: u64) {
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

/// 把 `0x0000000000XXXXXX` 形式的句柄转成 HWND。0 视为无效。
pub fn to_hwnd(value: i64) -> Result<windows::Win32::Foundation::HWND> {
    if value == 0 {
        anyhow::bail!("hwnd 不能为空");
    }
    Ok(windows::Win32::Foundation::HWND(value as *mut core::ffi::c_void))
}

pub fn hwnd_value(hwnd: windows::Win32::Foundation::HWND) -> i64 {
    hwnd.0 as isize as i64
}
