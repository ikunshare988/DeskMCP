use std::ffi::c_void;

use anyhow::{anyhow, Context, Result};
use image::RgbaImage;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS,
    HGDIOBJ, SRCCOPY,
};

use super::{to_hwnd, window};

// PW_RENDERFULLCONTENT = 2：让 DWM 合成内容也能被 PrintWindow 抓到
const PW_RENDERFULLCONTENT: u32 = 2;

#[link(name = "user32")]
extern "system" {
    fn PrintWindow(hwnd: HWND, hdc: windows::Win32::Graphics::Gdi::HDC, flags: u32) -> i32;
}

/// 从源 DC 的 (src_x, src_y) 起抓取 w×h 区域，转成 RGBA。
fn blit(source: windows::Win32::Graphics::Gdi::HDC, src_x: i32, src_y: i32, w: i32, h: i32) -> Result<RgbaImage> {
    if w <= 0 || h <= 0 {
        return Err(anyhow!("抓取区域为空（{w}×{h}）"));
    }
    unsafe {
        let mem = CreateCompatibleDC(source);
        if mem.is_invalid() {
            return Err(anyhow!("CreateCompatibleDC 失败"));
        }
        let bitmap = CreateCompatibleBitmap(source, w, h);
        if bitmap.is_invalid() {
            let _ = DeleteDC(mem);
            return Err(anyhow!("CreateCompatibleBitmap 失败（{w}×{h}）"));
        }
        let old = SelectObject(mem, HGDIOBJ(bitmap.0));
        let result = (|| -> Result<RgbaImage> {
            BitBlt(mem, 0, 0, w, h, source, src_x, src_y, SRCCOPY | CAPTUREBLT)
                .map_err(|e| anyhow!("BitBlt 失败: {e}"))?;
            let mut info = BITMAPINFO::default();
            info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = w;
            // 负高度 = 自上而下，省掉逐行翻转
            info.bmiHeader.biHeight = -h;
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            info.bmiHeader.biCompression = BI_RGB.0;

            let mut buffer = vec![0u8; (w as usize) * (h as usize) * 4];
            let lines = GetDIBits(
                mem,
                bitmap,
                0,
                h as u32,
                Some(buffer.as_mut_ptr() as *mut c_void),
                &mut info,
                DIB_RGB_COLORS,
            );
            if lines == 0 {
                return Err(anyhow!("GetDIBits 没有返回任何扫描行"));
            }
            for pixel in buffer.chunks_exact_mut(4) {
                pixel.swap(0, 2);
                pixel[3] = 255;
            }
            RgbaImage::from_raw(w as u32, h as u32, buffer).ok_or_else(|| anyhow!("像素缓冲区尺寸不匹配"))
        })();
        SelectObject(mem, old);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(mem);
        result
    }
}

/// 抓取屏幕上的任意矩形（物理像素）。
pub fn capture_region(rect: RECT) -> Result<RgbaImage> {
    let w = rect.right - rect.left;
    let h = rect.bottom - rect.top;
    unsafe {
        let screen = GetDC(HWND(std::ptr::null_mut()));
        if screen.is_invalid() {
            return Err(anyhow!("GetDC 失败"));
        }
        let result = blit(screen, rect.left, rect.top, w, h);
        ReleaseDC(HWND(std::ptr::null_mut()), screen);
        result
    }
}

/// 抓取整个虚拟桌面（多显示器会拼成一张）。
pub fn capture_screen() -> Result<RgbaImage> {
    let (x, y, w, h) = super::virtual_screen();
    capture_region(RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    })
}

/// 抓取单个窗口。先用 PrintWindow 拿 DWM 合成内容（被遮挡也能拿到），失败再退回屏幕区域。
pub fn capture_window(hwnd_value: i64) -> Result<RgbaImage> {
    let info = window::get(hwnd_value)?;
    if info.minimized {
        return Err(anyhow!("窗口已最小化，无法截图；先调用 focus_window"));
    }
    let hwnd = to_hwnd(hwnd_value)?;
    let (w, h) = (info.rect.width, info.rect.height);
    if w <= 0 || h <= 0 {
        return Err(anyhow!("窗口尺寸为 0，无法截图"));
    }
    unsafe {
        let window_dc = GetDC(hwnd);
        if !window_dc.is_invalid() {
            let mem = CreateCompatibleDC(window_dc);
            let bitmap = CreateCompatibleBitmap(window_dc, w, h);
            if !mem.is_invalid() && !bitmap.is_invalid() {
                let old = SelectObject(mem, HGDIOBJ(bitmap.0));
                let printed = PrintWindow(hwnd, mem, PW_RENDERFULLCONTENT) != 0;
                let mut info_header = BITMAPINFO::default();
                info_header.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
                info_header.bmiHeader.biWidth = w;
                info_header.bmiHeader.biHeight = -h;
                info_header.bmiHeader.biPlanes = 1;
                info_header.bmiHeader.biBitCount = 32;
                info_header.bmiHeader.biCompression = BI_RGB.0;
                let mut buffer = vec![0u8; (w as usize) * (h as usize) * 4];
                let lines = if printed {
                    GetDIBits(
                        mem,
                        bitmap,
                        0,
                        h as u32,
                        Some(buffer.as_mut_ptr() as *mut c_void),
                        &mut info_header,
                        DIB_RGB_COLORS,
                    )
                } else {
                    0
                };
                SelectObject(mem, old);
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
                let _ = DeleteDC(mem);
                if printed && lines > 0 {
                    ReleaseDC(hwnd, window_dc);
                    for pixel in buffer.chunks_exact_mut(4) {
                        pixel.swap(0, 2);
                        pixel[3] = 255;
                    }
                    return RgbaImage::from_raw(w as u32, h as u32, buffer)
                        .ok_or_else(|| anyhow!("像素缓冲区尺寸不匹配"));
                }
            } else {
                if !mem.is_invalid() {
                    let _ = DeleteDC(mem);
                }
                if !bitmap.is_invalid() {
                    let _ = DeleteObject(HGDIOBJ(bitmap.0));
                }
            }
            ReleaseDC(hwnd, window_dc);
        }
    }
    capture_region(RECT {
        left: info.rect.x,
        top: info.rect.y,
        right: info.rect.x + w,
        bottom: info.rect.y + h,
    })
    .context("窗口截图失败")
}

/// 按最大宽度等比缩小，避免把 4K 原图塞进模型的上下文。
pub fn downscale(image: RgbaImage, max_width: u32) -> RgbaImage {
    if max_width == 0 || image.width() <= max_width {
        return image;
    }
    let ratio = max_width as f32 / image.width() as f32;
    let height = ((image.height() as f32 * ratio).round() as u32).max(1);
    image::imageops::resize(&image, max_width, height, image::imageops::FilterType::Triangle)
}
