use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};

use openh264::{
    OpenH264API,
    encoder::{Encoder, EncoderConfig},
    formats::{RgbSliceU8, YUVBuffer},
};
use tokio_util::sync::CancellationToken;
use webrtc::{
    api::{
        APIBuilder, interceptor_registry::register_default_interceptors,
        media_engine::MediaEngine,
    },
    ice_transport::{
        ice_candidate::RTCIceCandidateInit, ice_gathering_state::RTCIceGatheringState,
        ice_server::RTCIceServer,
    },
    interceptor::registry::Registry,
    peer_connection::{
        RTCPeerConnection,
        configuration::RTCConfiguration,
        sdp::session_description::RTCSessionDescription,
    },
    rtp_transceiver::rtp_codec::RTCRtpCodecCapability,
    track::track_local::{
        TrackLocal, track_local_static_sample::TrackLocalStaticSample,
    },
};

use crate::{
    commands::{Ctx, net},
    protocol::{self, Obj},
};

struct MediaSession {
    pc: Arc<RTCPeerConnection>,
    cancel: CancellationToken,
    kind: String,
}

static SESSIONS: LazyLock<Mutex<HashMap<String, MediaSession>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn build_api() -> Result<webrtc::api::API, String> {
    let mut media_engine = MediaEngine::default();
    media_engine
        .register_default_codecs()
        .map_err(|error| error.to_string())?;
    let mut registry = Registry::new();
    registry = register_default_interceptors(registry, &mut media_engine)
        .map_err(|error| error.to_string())?;
    Ok(APIBuilder::new()
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .build())
}

fn ice_servers(payload: &protocol::Value) -> Vec<RTCIceServer> {
    let mut servers = Vec::new();
    for entry in protocol::array_field(payload, "iceServers") {
        let urls = protocol::field(entry, "urls")
            .map(|value| match value {
                protocol::Value::String(url) => vec![url.to_string()],
                protocol::Value::Array(items) => items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect(),
                _ => Vec::new(),
            })
            .unwrap_or_default();
        if urls.is_empty() {
            continue;
        }
        servers.push(RTCIceServer {
            urls,
            username: protocol::string_field(entry, "username").unwrap_or("").to_owned(),
            credential: protocol::string_field(entry, "credential").unwrap_or("").to_owned(),
            ..Default::default()
        });
        if servers.len() >= 8 {
            break;
        }
    }
    servers
}

fn video_track() -> Arc<TrackLocalStaticSample> {
    Arc::new(TrackLocalStaticSample::new(
        RTCRtpCodecCapability {
            mime_type: webrtc::api::media_engine::MIME_TYPE_H264.to_owned(),
            clock_rate: 90_000,
            ..Default::default()
        },
        "video".to_owned(),
        "overlord".to_owned(),
    ))
}

fn start_video_pump(
    track: Arc<TrackLocalStaticSample>,
    cancel: CancellationToken,
    display: usize,
    fps: u32,
    bitrate_bps: u32,
) {
    tokio::task::spawn_blocking(move || {
        let handle = tokio::runtime::Handle::current();
        let config = EncoderConfig::new()
            .set_bitrate_bps(bitrate_bps)
            .max_frame_rate(fps as f32)
            .enable_skip_frame(true);
        let api = OpenH264API::from_source();
        let mut encoder = match Encoder::with_api_config(api, config) {
            Ok(encoder) => encoder,
            Err(_) => return,
        };
        let frame_time = Duration::from_millis(1000 / u64::from(fps.max(1)));
        while !cancel.is_cancelled() {
            let started = std::time::Instant::now();
            #[cfg(windows)]
            {
                let Some(capture) = crate::win::screen::capture(display, false) else {
                    continue;
                };
                let (width, height, rgb) = crate::win::screen::resize_rgb(
                    &capture.rgb,
                    capture.width,
                    capture.height,
                    1080,
                );
                if width <= 0 || height <= 0 {
                    continue;
                }
                let source = RgbSliceU8::new(&rgb, (width as usize, height as usize));
                let yuv = YUVBuffer::from_rgb_source(source);
                if let Ok(bitstream) = encoder.encode(&yuv) {
                    let data = bitstream.to_vec();
                    if !data.is_empty() {
                        let sample = webrtc::media::Sample {
                            data: data.into(),
                            duration: frame_time,
                            ..Default::default()
                        };
                        let _ = handle.block_on(track.write_sample(&sample));
                    }
                }
            }
            #[cfg(not(windows))]
            {
                let _ = (&track, &handle);
                let _ = frame_time;
                break;
            }
            let elapsed = started.elapsed();
            if elapsed < frame_time {
                std::thread::sleep(frame_time - elapsed);
            }
        }
    });
}

fn register_session(
    session_id: &str,
    pc: Arc<RTCPeerConnection>,
    cancel: CancellationToken,
    kind: &str,
) {
    if let Some(previous) = SESSIONS
        .lock()
        .unwrap()
        .insert(
            session_id.to_owned(),
            MediaSession {
                pc,
                cancel,
                kind: kind.to_owned(),
            },
        )
    {
        previous.cancel.cancel();
    }
}

pub async fn handle(ctx: Ctx) {
    match ctx.command.name.as_str() {
        "webrtc_publish" => publish(ctx).await,
        "webrtc_stop" => stop(ctx).await,
        "webrtc_p2p_offer" => p2p_offer(ctx).await,
        "webrtc_p2p_ice" => p2p_ice(ctx).await,
        "webrtc_p2p_stop" => p2p_stop(ctx).await,
        _ => {}
    }
}

async fn publish(ctx: Ctx) {
    let whip_path = ctx.string("whipPath").unwrap_or("").to_owned();
    let token = ctx.string("token").unwrap_or("").to_owned();
    if whip_path.is_empty() || token.is_empty() {
        ctx.out
            .result(&ctx.command, false, Some("missing whipPath/token"))
            .await;
        return;
    }
    let kind = ctx.string_or("kind", "desktop");
    let display = ctx
        .state
        .desktop_settings
        .display
        .load(std::sync::atomic::Ordering::Relaxed);
    let fps = ctx
        .state
        .desktop_settings
        .fps
        .load(std::sync::atomic::Ordering::Relaxed);
    let servers = ice_servers(ctx.payload());
    let out = ctx.out.clone();
    let command = ctx.command.clone();
    let server = ctx
        .state
        .config
        .servers
        .first()
        .map(|server| server.as_str().to_owned())
        .unwrap_or_default();
    let config = ctx.state.config.clone();

    let result: Result<(), String> = async {
        let api = build_api()?;
        let pc = Arc::new(
            api.new_peer_connection(RTCConfiguration {
                ice_servers: servers,
                ..Default::default()
            })
            .await
            .map_err(|error| error.to_string())?,
        );
        let track = video_track();
        pc.add_track(track.clone())
            .await
            .map_err(|error| error.to_string())?;

        let offer = pc.create_offer(None).await.map_err(|error| error.to_string())?;
        pc.set_local_description(offer)
            .await
            .map_err(|error| error.to_string())?;
        // WHIP accepts non-trickle SDP; wait briefly for host candidates.
        for _ in 0..20 {
            if pc.ice_gathering_state() == RTCIceGatheringState::Complete {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let local = pc
            .local_description()
            .await
            .ok_or_else(|| "missing local description".to_owned())?;
        let url = net::resolve_url(&server, &whip_path);
        let (status, body) = net::post(
            &url,
            "application/sdp",
            local.sdp.as_bytes(),
            Some(&token),
            config.insecure_tls,
            &config.tls_spki_pins,
        )
        .await?;
        if status != 200 && status != 201 {
            return Err(format!("whip publish failed with status {status}"));
        }
        let answer = String::from_utf8_lossy(&body).into_owned();
        let answer = RTCSessionDescription::answer(answer).map_err(|error| error.to_string())?;
        pc.set_remote_description(answer)
            .await
            .map_err(|error| error.to_string())?;

        let cancel = CancellationToken::new();
        register_session(&command.id, pc, cancel.clone(), &kind);
        start_video_pump(track, cancel, display, fps.max(1), 2_000_000);
        Ok(())
    }
    .await;

    match result {
        Ok(()) => out.result(&command, true, None).await,
        Err(error) => out.result(&command, false, Some(&error)).await,
    }
}

async fn p2p_offer(ctx: Ctx) {
    let session_id = ctx.string("sessionId").unwrap_or("").to_owned();
    let sdp = ctx.string("sdp").unwrap_or("").to_owned();
    if session_id.is_empty() || sdp.is_empty() {
        ctx.out
            .result(&ctx.command, false, Some("missing sessionId/sdp"))
            .await;
        return;
    }
    let kind = ctx.string_or("kind", "desktop");
    let has_video = ctx.boolean_or("hasVideo", true);
    let servers = ice_servers(ctx.payload());
    let display = ctx
        .state
        .desktop_settings
        .display
        .load(std::sync::atomic::Ordering::Relaxed);
    let fps = ctx
        .state
        .desktop_settings
        .fps
        .load(std::sync::atomic::Ordering::Relaxed);
    let out = ctx.out.clone();
    let command = ctx.command.clone();
    let session_for_ice = session_id.clone();
    let kind_for_ice = kind.clone();

    let result: Result<(), String> = async {
        let api = build_api()?;
        let pc = Arc::new(
            api.new_peer_connection(RTCConfiguration {
                ice_servers: servers,
                ..Default::default()
            })
            .await
            .map_err(|error| error.to_string())?,
        );
        let track = video_track();
        if has_video {
            pc.add_track(track.clone())
                .await
                .map_err(|error| error.to_string())?;
        }

        let out_ice = out.clone();
        pc.on_ice_candidate(Box::new(move |candidate| {
            let out = out_ice.clone();
            let session = session_for_ice.clone();
            let kind = kind_for_ice.clone();
            Box::pin(async move {
                if let Some(candidate) = candidate {
                    if let Ok(init) = candidate.to_json() {
                        out.send(
                            Obj::new()
                                .put(protocol::key_type(), protocol::s("webrtc_p2p_ice"))
                                .put(protocol::key_session_id(), protocol::s(&session))
                                .put(protocol::key_kind(), protocol::s(&kind))
                                .put(protocol::key_candidate(), protocol::s(&init.candidate))
                                .put(
                                    protocol::key_sdp_mid(),
                                    protocol::s(init.sdp_mid.as_deref().unwrap_or("")),
                                )
                                .put(
                                    protocol::key_sdp_mline_index(),
                                    protocol::n(init.sdp_mline_index.unwrap_or(0) as i64),
                                )
                                .value(),
                        )
                        .await;
                    }
                }
            })
        }));

        let offer = RTCSessionDescription::offer(sdp).map_err(|error| error.to_string())?;
        pc.set_remote_description(offer)
            .await
            .map_err(|error| error.to_string())?;
        let answer = pc.create_answer(None).await.map_err(|error| error.to_string())?;
        pc.set_local_description(answer.clone())
            .await
            .map_err(|error| error.to_string())?;
        out.send(
            Obj::new()
                .put(protocol::key_type(), protocol::s("webrtc_p2p_answer"))
                .put(protocol::key_session_id(), protocol::s(&session_id))
                .put(protocol::key_kind(), protocol::s(&kind))
                .put(protocol::key_sdp(), protocol::s(&answer.sdp))
                .value(),
        )
        .await;

        let cancel = CancellationToken::new();
        register_session(&session_id, pc, cancel.clone(), &kind);
        if has_video {
            start_video_pump(track, cancel, display, fps.max(1), 2_000_000);
        }
        Ok(())
    }
    .await;

    match result {
        Ok(()) => ctx.out.result(&command, true, None).await,
        Err(error) => ctx.out.result(&command, false, Some(&error)).await,
    }
}

async fn p2p_ice(ctx: Ctx) {
    let session_id = ctx.string("sessionId").unwrap_or("").to_owned();
    let candidate = ctx.string("candidate").unwrap_or("").to_owned();
    if session_id.is_empty() || candidate.is_empty() {
        return;
    }
    let pc = SESSIONS
        .lock()
        .unwrap()
        .get(&session_id)
        .map(|session| session.pc.clone());
    if let Some(pc) = pc {
        let init = RTCIceCandidateInit {
            candidate,
            sdp_mid: ctx.string("sdpMid").map(str::to_owned),
            sdp_mline_index: ctx.integer("sdpMLineIndex").map(|value| value as u16),
            username_fragment: None,
        };
        let _ = pc.add_ice_candidate(init).await;
    }
}

async fn p2p_stop(ctx: Ctx) {
    let session_id = ctx.string("sessionId").unwrap_or("").to_owned();
    let drained: Vec<MediaSession> = {
        let mut sessions = SESSIONS.lock().unwrap();
        if session_id.is_empty() {
            sessions.drain().map(|(_, session)| session).collect()
        } else {
            sessions.remove(&session_id).into_iter().collect()
        }
    };
    for session in drained {
        session.cancel.cancel();
        let _ = session.pc.close().await;
    }
    ctx.out.result(&ctx.command, true, None).await;
}

async fn stop(ctx: Ctx) {
    let kind = ctx.string("kind").map(str::to_owned);
    let drained = {
        let mut sessions = SESSIONS.lock().unwrap();
        if let Some(kind) = &kind {
            let ids: Vec<String> = sessions
                .iter()
                .filter(|(_, session)| &session.kind == kind)
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| sessions.remove(&id))
                .collect::<Vec<_>>()
        } else {
            sessions.drain().map(|(_, session)| session).collect()
        }
    };
    for session in drained {
        session.cancel.cancel();
        let _ = session.pc.close().await;
    }
    ctx.out.result(&ctx.command, true, None).await;
}

#[allow(dead_code)]
fn _unused(_: &dyn TrackLocal) {}
