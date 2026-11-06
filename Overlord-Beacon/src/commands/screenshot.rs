use crate::{
    commands::Ctx,
    protocol::{self, Obj},
};

pub async fn handle(ctx: Ctx) {
    let mode = ctx.string("mode").unwrap_or("");
    let thumbnail = mode == "thumbnail";
    let all_displays = ctx.boolean_or("allDisplays", false)
        || mode == "notification"
        || mode == "thumbnail"
        || cfg!(windows);

    #[cfg(windows)]
    {
        let captured = tokio::task::spawn_blocking(move || {
            crate::win::screen::capture(0, all_displays)
        })
        .await
        .ok()
        .flatten();
        match captured {
            Some(capture) => {
                let quality = if thumbnail { 85 } else { 92 };
                let encoded = crate::media::encode_jpeg(
                    &capture.rgb,
                    capture.width,
                    capture.height,
                    quality,
                );
                match encoded {
                    Some(data) => {
                        ctx.out
                            .send(
                                Obj::new()
                                    .put(protocol::key_type(), protocol::s("screenshot_result"))
                                    .put(protocol::key_command_id(), protocol::s(&ctx.command.id))
                                    .put(protocol::key_format(), protocol::s("jpeg"))
                                    .put(protocol::key_width(), protocol::n(capture.width as i64))
                                    .put(protocol::key_height(), protocol::n(capture.height as i64))
                                    .put(protocol::key_data(), protocol::bin(data))
                                    .value(),
                            )
                            .await;
                        ctx.out.result(&ctx.command, true, None).await;
                    }
                    None => {
                        ctx.out
                            .result(&ctx.command, false, Some("jpeg encoding failed"))
                            .await
                    }
                }
            }
            None => {
                ctx.out
                    .result(&ctx.command, false, Some("screen capture failed"))
                    .await
            }
        }
    }

    #[cfg(not(windows))]
    {
        let _ = (thumbnail, all_displays);
        ctx.out
            .result(&ctx.command, false, Some("screen capture is only available on Windows"))
            .await;
    }
}
