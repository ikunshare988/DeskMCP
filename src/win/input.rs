use anyhow::{anyhow, bail, Result};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSEINPUT, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL,
    VIRTUAL_KEY,
};

// 滚轮一格等于 120，windows crate 没有导出这个常量
const WHEEL_DELTA: i32 = 120;

use super::{sleep_ms, virtual_screen};

fn send(inputs: &[INPUT]) -> Result<()> {
    if inputs.is_empty() {
        return Ok(());
    }
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent == 0 {
        // 常见原因：目标窗口以管理员权限运行（UIPI），或输入被安全软件拦截
        bail!("SendInput 被拒绝：目标进程可能以管理员权限运行，或输入被拦截");
    }
    Ok(())
}

fn mouse_event(flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn key_event(vk: u16, scan: u16, flags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// 物理像素坐标 → SendInput 需要的 0..65535 归一化坐标。
fn normalize(x: i32, y: i32) -> (i32, i32) {
    let (vx, vy, vw, vh) = virtual_screen();
    let span_x = (vw.max(2) - 1) as f64;
    let span_y = (vh.max(2) - 1) as f64;
    let nx = (((x - vx) as f64 / span_x) * 65535.0).round().clamp(0.0, 65535.0) as i32;
    let ny = (((y - vy) as f64 / span_y) * 65535.0).round().clamp(0.0, 65535.0) as i32;
    (nx, ny)
}

pub fn move_to(x: i32, y: i32) -> Result<()> {
    let (nx, ny) = normalize(x, y);
    send(&[mouse_event(
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        nx,
        ny,
        0,
    )])
}

pub fn click(x: i32, y: i32, button: &str, count: u32) -> Result<()> {
    move_to(x, y)?;
    sleep_ms(20);
    let (down, up) = match button {
        "left" => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
        "right" => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
        "middle" => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
        other => bail!("不支持的按键：{other}（可选 left / right / middle）"),
    };
    for _ in 0..count.max(1) {
        send(&[mouse_event(down, 0, 0, 0)])?;
        sleep_ms(15);
        send(&[mouse_event(up, 0, 0, 0)])?;
        sleep_ms(35);
    }
    Ok(())
}

pub fn drag(x1: i32, y1: i32, x2: i32, y2: i32, duration_ms: u32) -> Result<()> {
    move_to(x1, y1)?;
    sleep_ms(30);
    send(&[mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0)])?;
    let steps = (duration_ms / 16).max(4);
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        let x = x1 + ((x2 - x1) as f64 * t) as i32;
        let y = y1 + ((y2 - y1) as f64 * t) as i32;
        move_to(x, y)?;
        sleep_ms((duration_ms / steps) as u64);
    }
    send(&[mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0)])?;
    Ok(())
}

/// amount 以「格」为单位，正数向上滚。
pub fn scroll(x: i32, y: i32, amount: i32) -> Result<()> {
    move_to(x, y)?;
    sleep_ms(20);
    send(&[mouse_event(MOUSEEVENTF_WHEEL, 0, 0, amount * WHEEL_DELTA as i32)])
}

/// 逐字符输入 Unicode 文本。不经过剪贴板，也不依赖键盘布局。
pub fn type_text(text: &str) -> Result<()> {
    for unit in text.encode_utf16() {
        send(&[key_event(0, unit, KEYEVENTF_UNICODE)])?;
        send(&[key_event(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP)])?;
    }
    Ok(())
}

fn parse_key(token: &str) -> Result<(u16, bool)> {
    let raw = token.trim().to_lowercase();
    let name = match raw.as_str() {
        "ctrl" | "control" => return Ok((0x11, false)),
        "shift" => return Ok((0x10, false)),
        "alt" | "option" => return Ok((0x12, false)),
        "win" | "meta" | "cmd" | "super" => return Ok((0x5B, false)),
        "enter" | "return" => return Ok((0x0D, false)),
        "tab" => return Ok((0x09, false)),
        "esc" | "escape" => return Ok((0x1B, false)),
        "space" => return Ok((0x20, false)),
        "backspace" => return Ok((0x08, false)),
        "delete" | "del" => return Ok((0x2E, true)),
        "insert" | "ins" => return Ok((0x2D, true)),
        "home" => return Ok((0x24, true)),
        "end" => return Ok((0x23, true)),
        "pageup" | "pgup" => return Ok((0x21, true)),
        "pagedown" | "pgdn" => return Ok((0x22, true)),
        "up" => return Ok((0x26, true)),
        "down" => return Ok((0x28, true)),
        "left" => return Ok((0x25, true)),
        "right" => return Ok((0x27, true)),
        "plus" => return Ok((0xBB, false)),
        "minus" => return Ok((0xBD, false)),
        "comma" => return Ok((0xBC, false)),
        "period" | "dot" => return Ok((0xBE, false)),
        "slash" => return Ok((0xBF, false)),
        "backslash" => return Ok((0xDC, false)),
        "semicolon" => return Ok((0xBA, false)),
        "quote" => return Ok((0xDE, false)),
        "bracketleft" => return Ok((0xDB, false)),
        "bracketright" => return Ok((0xDD, false)),
        "grave" => return Ok((0xC0, false)),
        other => other,
    };
    if let Some(rest) = name.strip_prefix('f') {
        if let Ok(n) = rest.parse::<u16>() {
            if (1..=24).contains(&n) {
                return Ok((0x6F + n, false));
            }
        }
    }
    if name.len() == 1 {
        let ch = name.chars().next().unwrap();
        if ch.is_ascii_alphabetic() {
            return Ok((ch.to_ascii_uppercase() as u16, false));
        }
        if ch.is_ascii_digit() {
            return Ok((ch as u16, false));
        }
        let vk = match ch {
            '+' | '=' => 0xBB,
            '-' | '_' => 0xBD,
            ',' | '<' => 0xBC,
            '.' | '>' => 0xBE,
            '/' | '?' => 0xBF,
            '\\' | '|' => 0xDC,
            ';' | ':' => 0xBA,
            '\'' | '"' => 0xDE,
            '[' | '{' => 0xDB,
            ']' | '}' => 0xDD,
            '`' | '~' => 0xC0,
            _ => return Err(anyhow!("无法识别的按键：{token}")),
        };
        return Ok((vk, false));
    }
    Err(anyhow!("无法识别的按键：{token}"))
}

/// 解析 "ctrl+shift+s" 这类组合键并发送。
pub fn press_keys(combo: &str) -> Result<()> {
    let normalized = if combo.ends_with("++") {
        format!("{}+plus", &combo[..combo.len() - 2])
    } else {
        combo.to_string()
    };
    let tokens: Vec<&str> = normalized.split('+').filter(|s| !s.is_empty()).collect();
    if tokens.is_empty() {
        bail!("组合键为空");
    }
    let mut parsed = Vec::with_capacity(tokens.len());
    for token in tokens {
        parsed.push(parse_key(token)?);
    }
    let mut downs = Vec::new();
    for (vk, extended) in &parsed {
        let mut flags = windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS(0);
        if *extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        downs.push(key_event(*vk, 0, flags));
    }
    send(&downs)?;
    sleep_ms(20);
    let mut ups = Vec::new();
    for (vk, extended) in parsed.iter().rev() {
        let mut flags = KEYEVENTF_KEYUP;
        if *extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        ups.push(key_event(*vk, 0, flags));
    }
    send(&ups)
}
