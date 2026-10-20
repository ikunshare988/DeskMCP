use std::ffi::c_void;

use anyhow::{anyhow, Result};
use windows::Win32::Foundation::{HGLOBAL, HANDLE, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

use super::sleep_ms;

// CF_UNICODETEXT；用字面量避免不同版本里常量所在模块的差异
const CF_UNICODETEXT: u32 = 13;

fn open(retries: u32) -> Result<()> {
    for attempt in 0..retries {
        unsafe {
            if OpenClipboard(HWND(std::ptr::null_mut())).is_ok() {
                return Ok(());
            }
        }
        if attempt + 1 < retries {
            sleep_ms(20);
        }
    }
    Err(anyhow!("剪贴板被其它程序占用，打开失败"))
}

pub fn read_text() -> Result<String> {
    open(8)?;
    let result = (|| unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() {
            return Ok(String::new());
        }
        let handle = GetClipboardData(CF_UNICODETEXT)?;
        if handle.is_invalid() {
            return Ok(String::new());
        }
        let memory = HGLOBAL(handle.0);
        let ptr = GlobalLock(memory);
        if ptr.is_null() {
            return Err(anyhow!("GlobalLock 失败"));
        }
        let mut len = 0usize;
        let units = ptr as *const u16;
        while *units.add(len) != 0 && len < 4 * 1024 * 1024 {
            len += 1;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(units, len));
        let _ = GlobalUnlock(memory);
        Ok(text)
    })();
    unsafe {
        let _ = CloseClipboard();
    }
    result
}

pub fn write_text(text: &str) -> Result<()> {
    let mut units: Vec<u16> = text.encode_utf16().collect();
    units.push(0);
    let bytes = units.len() * std::mem::size_of::<u16>();
    open(8)?;
    let result = (|| unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes)?;
        if memory.is_invalid() {
            return Err(anyhow!("GlobalAlloc 失败"));
        }
        let ptr = GlobalLock(memory);
        if ptr.is_null() {
            return Err(anyhow!("GlobalLock 失败"));
        }
        std::ptr::copy_nonoverlapping(units.as_ptr() as *const c_void, ptr, bytes);
        let _ = GlobalUnlock(memory);
        EmptyClipboard()?;
        // 写入成功后内存所有权归系统，不能再释放
        SetClipboardData(CF_UNICODETEXT, HANDLE(memory.0))?;
        Ok(())
    })();
    unsafe {
        let _ = CloseClipboard();
    }
    result
}
