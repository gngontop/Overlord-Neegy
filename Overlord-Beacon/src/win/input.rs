use windows_sys::Win32::{
    UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
        KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSEINPUT, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
        MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
        MOUSEEVENTF_WHEEL,
    },
    UI::WindowsAndMessaging::SetCursorPos,
};

pub fn move_cursor(x: i32, y: i32) {
    unsafe {
        SetCursorPos(x, y);
    }
}

fn send(input: &INPUT) {
    unsafe {
        SendInput(1, input, std::mem::size_of::<INPUT>() as i32);
    }
}

pub fn mouse_button(button: i64, down: bool) {
    let (down_flag, up_flag) = match button {
        1 => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
        2 => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
        _ => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
    };
    let mut input: INPUT = unsafe { std::mem::zeroed() };
    input.r#type = INPUT_MOUSE;
    input.Anonymous.mi = MOUSEINPUT {
        dx: 0,
        dy: 0,
        mouseData: 0,
        dwFlags: if down { down_flag } else { up_flag },
        time: 0,
        dwExtraInfo: 0,
    };
    send(&input);
}

pub fn mouse_wheel(delta: i32) {
    let mut input: INPUT = unsafe { std::mem::zeroed() };
    input.r#type = INPUT_MOUSE;
    input.Anonymous.mi = MOUSEINPUT {
        dx: 0,
        dy: 0,
        mouseData: delta as u32,
        dwFlags: MOUSEEVENTF_WHEEL,
        time: 0,
        dwExtraInfo: 0,
    };
    send(&input);
}

pub fn key(vk: u16, down: bool) {
    if vk == 0 {
        return;
    }
    let flags: KEYBD_EVENT_FLAGS = if down { 0 } else { KEYEVENTF_KEYUP };
    let mut input: INPUT = unsafe { std::mem::zeroed() };
    input.r#type = INPUT_KEYBOARD;
    input.Anonymous.ki = KEYBDINPUT {
        wVk: vk,
        wScan: 0,
        dwFlags: flags,
        time: 0,
        dwExtraInfo: 0,
    };
    send(&input);
}

pub fn unicode_char(character: u16, down: bool) {
    let flags: KEYBD_EVENT_FLAGS = KEYEVENTF_UNICODE | if down { 0 } else { KEYEVENTF_KEYUP };
    let mut input: INPUT = unsafe { std::mem::zeroed() };
    input.r#type = INPUT_KEYBOARD;
    input.Anonymous.ki = KEYBDINPUT {
        wVk: 0,
        wScan: character,
        dwFlags: flags,
        time: 0,
        dwExtraInfo: 0,
    };
    send(&input);
}

pub fn type_text(text: &str) {
    for character in text.encode_utf16() {
        if character == b'\r' as u16 {
            unicode_char(0x0D, true);
            unicode_char(0x0D, false);
        } else if character == b'\n' as u16 {
            unicode_char(0x0D, true);
            unicode_char(0x0D, false);
        } else if character == b'\t' as u16 {
            unicode_char(0x09, true);
            unicode_char(0x09, false);
        } else if character == 0x08 {
            unicode_char(0x08, true);
            unicode_char(0x08, false);
        } else {
            unicode_char(character, true);
            unicode_char(character, false);
        }
    }
}

/// Maps DOM `KeyboardEvent.code` values to Windows virtual-key codes.
pub fn dom_code_to_vk(code: &str) -> u16 {
    if let Some(hex) = code.strip_prefix("VK_") {
        if let Ok(value) = u16::from_str_radix(hex, 16) {
            return value;
        }
    }
    if let Some(rest) = code.strip_prefix("Key") {
        let mut chars = rest.chars();
        if let (Some(letter), None) = (chars.next(), chars.next()) {
            let upper = letter.to_ascii_uppercase();
            if upper.is_ascii_uppercase() {
                return upper as u16;
            }
        }
    }
    if let Some(rest) = code.strip_prefix("Digit") {
        if let Ok(digit) = rest.parse::<u16>() {
            if digit <= 9 {
                return 0x30 + digit;
            }
        }
    }
    if let Some(rest) = code.strip_prefix("Numpad") {
        if let Ok(digit) = rest.parse::<u16>() {
            if digit <= 9 {
                return 0x60 + digit;
            }
        }
        return match rest {
            "Add" => 0x6B,
            "Subtract" => 0x6D,
            "Multiply" => 0x6A,
            "Divide" => 0x6F,
            "Decimal" => 0x6E,
            "Enter" => 0x0D,
            _ => 0,
        };
    }
    if let Some(rest) = code.strip_prefix('F') {
        if let Ok(number) = rest.parse::<u16>() {
            if (1..=24).contains(&number) {
                return 0x70 + number - 1;
            }
        }
    }
    match code {
        "Backspace" => 0x08,
        "Tab" => 0x09,
        "Enter" | "NumpadEnter" => 0x0D,
        "ShiftLeft" => 0xA0,
        "ShiftRight" => 0xA1,
        "ControlLeft" => 0xA2,
        "ControlRight" => 0xA3,
        "AltLeft" => 0xA4,
        "AltRight" => 0xA5,
        "MetaLeft" => 0x5B,
        "MetaRight" => 0x5C,
        "Pause" => 0x13,
        "CapsLock" => 0x14,
        "Escape" => 0x1B,
        "Space" => 0x20,
        "PageUp" => 0x21,
        "PageDown" => 0x22,
        "End" => 0x23,
        "Home" => 0x24,
        "ArrowLeft" => 0x25,
        "ArrowUp" => 0x26,
        "ArrowRight" => 0x27,
        "ArrowDown" => 0x28,
        "PrintScreen" => 0x2C,
        "Insert" => 0x2D,
        "Delete" => 0x2E,
        "NumLock" => 0x90,
        "ScrollLock" => 0x91,
        "Semicolon" => 0xBA,
        "Equal" => 0xBB,
        "Comma" => 0xBC,
        "Minus" => 0xBD,
        "Period" => 0xBE,
        "Slash" => 0xBF,
        "Backquote" => 0xC0,
        "BracketLeft" => 0xDB,
        "Backslash" => 0xDC,
        "BracketRight" => 0xDD,
        "Quote" => 0xDE,
        "ContextMenu" => 0x5D,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_dom_codes() {
        assert_eq!(dom_code_to_vk("KeyA"), 0x41);
        assert_eq!(dom_code_to_vk("Digit1"), 0x31);
        assert_eq!(dom_code_to_vk("F12"), 0x7B);
        assert_eq!(dom_code_to_vk("ArrowLeft"), 0x25);
        assert_eq!(dom_code_to_vk("Unknown"), 0);
    }
}
