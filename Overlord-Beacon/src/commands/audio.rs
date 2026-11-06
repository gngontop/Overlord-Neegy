use std::{
    collections::VecDeque,
    sync::{Arc, LazyLock, Mutex, mpsc},
    time::Duration,
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::{
    commands::Ctx,
    protocol::{self, Obj},
    state::Outbound,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Voice,
    Desktop,
}

enum AudioCommand {
    Start {
        out: Outbound,
        session_id: String,
        kind: Kind,
        source: String,
    },
    Stop,
    Downlink(Vec<u8>),
}

struct Shared {
    session: Mutex<Option<(String, Outbound, Kind)>>,
    playback: Arc<Mutex<VecDeque<f32>>>,
    desktop_phase: Mutex<u32>,
}

static AUDIO_TX: LazyLock<Mutex<Option<mpsc::Sender<AudioCommand>>>> =
    LazyLock::new(|| Mutex::new(None));

fn ensure_thread() -> Option<mpsc::Sender<AudioCommand>> {
    let mut guard = AUDIO_TX.lock().unwrap();
    if let Some(sender) = guard.as_ref() {
        return Some(sender.clone());
    }
    let (tx, rx) = mpsc::channel::<AudioCommand>();
    let thread_tx = tx.clone();
    std::thread::Builder::new()
        .name("audio".to_owned())
        .spawn(move || audio_thread(rx, thread_tx))
        .ok()?;
    *guard = Some(tx.clone());
    Some(tx)
}

fn audio_thread(rx: mpsc::Receiver<AudioCommand>, _tx: mpsc::Sender<AudioCommand>) {
    let host = cpal::default_host();
    let shared = Arc::new(Shared {
        session: Mutex::new(None),
        playback: Arc::new(Mutex::new(VecDeque::new())),
        desktop_phase: Mutex::new(0),
    });

    let input_stream = build_input(&host, shared.clone());
    #[allow(unused_mut)]
    let mut output_stream = build_output(&host, shared.clone());

    if let Some(stream) = input_stream.as_ref() {
        let _ = stream.play();
    }
    if let Some(stream) = output_stream.as_mut() {
        let _ = stream.play();
    }

    while let Ok(command) = rx.recv() {
        match command {
            AudioCommand::Start {
                out,
                session_id,
                kind,
                source,
            } => {
                let _ = source;
                *shared.session.lock().unwrap() = Some((session_id, out, kind));
            }
            AudioCommand::Stop => {
                *shared.session.lock().unwrap() = None;
                shared.playback.lock().unwrap().clear();
            }
            AudioCommand::Downlink(data) => {
                let mut buffer = shared.playback.lock().unwrap();
                for chunk in data.chunks_exact(2) {
                    let sample = i16::from_le_bytes([chunk[0], chunk[1]]) as f32 / 32768.0;
                    buffer.push_back(sample);
                    if buffer.len() > 48_000 * 4 {
                        buffer.pop_front();
                    }
                }
            }
        }
    }
    drop(input_stream);
    drop(output_stream);
    let _ = Duration::from_millis(1);
}

fn build_input(host: &cpal::Host, shared: Arc<Shared>) -> Option<cpal::Stream> {
    let device = host.default_input_device()?;
    let supported = device.default_input_config().ok()?;
    let config = supported.config();
    let channels = config.channels as usize;
    let rate = config.sample_rate.0;
    let sample_format = supported.sample_format();

    macro_rules! stream_for {
        ($ty:ty, $convert:expr) => {{
            let shared = shared.clone();
            let callback = move |data: &[$ty], _: &cpal::InputCallbackInfo| {
                let (session, out, kind) = {
                    let guard = shared.session.lock().unwrap();
                    let Some((session, out, kind)) = guard.as_ref() else {
                        return;
                    };
                    (session.clone(), out.clone(), *kind)
                };
                let mut mono = Vec::with_capacity(data.len() / channels.max(1));
                for frame in data.chunks(channels.max(1)) {
                    let sum: f32 = frame.iter().map(|sample| $convert(*sample)).sum();
                    let value = (sum / channels.max(1) as f32).clamp(-1.0, 1.0);
                    mono.push((value * 32767.0) as i16);
                }
                if kind == Kind::Desktop {
                    let mut phase = shared.desktop_phase.lock().unwrap();
                    let step = (rate / 16_000).max(1);
                    let mut output = Vec::with_capacity(mono.len() / step as usize + 1);
                    for sample in mono {
                        if *phase == 0 {
                            output.push(sample);
                        }
                        *phase = (*phase + 1) % step;
                    }
                    mono = output;
                }
                if mono.is_empty() {
                    return;
                }
                let mut bytes = Vec::with_capacity(mono.len() * 2);
                for sample in &mono {
                    bytes.extend_from_slice(&sample.to_le_bytes());
                }
                out.blocking_send(
                    Obj::new()
                        .put(
                            protocol::key_type(),
                            protocol::s(if kind == Kind::Voice {
                                "voice_uplink"
                            } else {
                                "desktop_audio_uplink"
                            }),
                        )
                        .put(protocol::key_session_id(), protocol::s(&session))
                        .put(protocol::key_data(), protocol::bin(bytes))
                        .value(),
                );
            };
            device
                .build_input_stream(&config, callback, |_| {}, None)
                .ok()
        }};
    }

    match sample_format {
        cpal::SampleFormat::F32 => stream_for!(f32, |value: f32| value),
        cpal::SampleFormat::I16 => stream_for!(i16, |value: i16| value as f32 / 32768.0),
        cpal::SampleFormat::U16 => {
            stream_for!(u16, |value: u16| (value as f32 - 32768.0) / 32768.0)
        }
        _ => None,
    }
}

fn build_output(host: &cpal::Host, shared: Arc<Shared>) -> Option<cpal::Stream> {
    let device = host.default_output_device()?;
    let supported = device.default_output_config().ok()?;
    let config = supported.config();
    let channels = config.channels as usize;
    let sample_format = supported.sample_format();

    macro_rules! stream_for {
        ($ty:ty, $convert:expr) => {{
            let shared = shared.clone();
            let callback = move |data: &mut [$ty], _: &cpal::OutputCallbackInfo| {
                let mut buffer = shared.playback.lock().unwrap();
                for frame in data.chunks_mut(channels.max(1)) {
                    let sample = buffer.pop_front().unwrap_or(0.0);
                    for out in frame.iter_mut() {
                        *out = $convert(sample);
                    }
                }
            };
            device
                .build_output_stream(&config, callback, |_| {}, None)
                .ok()
        }};
    }

    match sample_format {
        cpal::SampleFormat::F32 => stream_for!(f32, |value: f32| value),
        cpal::SampleFormat::I16 => {
            stream_for!(i16, |value: f32| (value * 32767.0) as i16)
        }
        cpal::SampleFormat::U16 => {
            stream_for!(u16, |value: f32| ((value * 32767.0) + 32768.0) as u16)
        }
        _ => None,
    }
}

pub async fn handle(ctx: Ctx) {
    match ctx.command.name.as_str() {
        "voice_capabilities" => {
            let available = tokio::task::spawn_blocking(|| {
                let host = cpal::default_host();
                host.default_input_device().is_some()
            })
            .await
            .unwrap_or(false);
            let message = serde_json::json!({
                "available": available,
                "requiresCgo": false,
                "sources": if available { vec!["default", "system"] } else { Vec::<&str>::new() },
                "defaultSource": "default",
            });
            ctx.out
                .result(&ctx.command, available, Some(&message.to_string()))
                .await;
        }
        "voice_session_start" => {
            let session_id = ctx.string("sessionId").unwrap_or("").to_owned();
            if session_id.is_empty() {
                ctx.out
                    .result(&ctx.command, false, Some("missing voice session id"))
                    .await;
                return;
            }
            let source = ctx.string_or("source", "default");
            match ensure_thread() {
                Some(sender) => {
                    let _ = sender.send(AudioCommand::Start {
                        out: ctx.out.clone(),
                        session_id,
                        kind: Kind::Voice,
                        source,
                    });
                    ctx.out.result(&ctx.command, true, None).await;
                }
                None => {
                    ctx.out
                        .result(&ctx.command, false, Some("audio device unavailable"))
                        .await
                }
            }
        }
        "voice_session_stop" | "desktop_audio_stop" => {
            if let Some(sender) = ensure_thread() {
                let _ = sender.send(AudioCommand::Stop);
            }
            ctx.out.result(&ctx.command, true, None).await;
        }
        "voice_downlink" => {
            let data = ctx.bytes("data").unwrap_or(&[]).to_vec();
            if data.is_empty() {
                ctx.out
                    .result(&ctx.command, false, Some("missing payload"))
                    .await;
                return;
            }
            if let Some(sender) = ensure_thread() {
                let _ = sender.send(AudioCommand::Downlink(data));
            }
            ctx.out.result(&ctx.command, true, None).await;
        }
        "desktop_audio_start" => {
            let session_id = ctx.string("sessionId").unwrap_or("").to_owned();
            if session_id.is_empty() {
                ctx.out
                    .result(&ctx.command, false, Some("missing desktop audio session id"))
                    .await;
                return;
            }
            let source = ctx.string_or("source", "system");
            match ensure_thread() {
                Some(sender) => {
                    let _ = sender.send(AudioCommand::Start {
                        out: ctx.out.clone(),
                        session_id,
                        kind: Kind::Desktop,
                        source,
                    });
                    ctx.out.result(&ctx.command, true, None).await;
                }
                None => {
                    ctx.out
                        .result(&ctx.command, false, Some("audio device unavailable"))
                        .await
                }
            }
        }
        _ => {
            ctx.out
                .result(&ctx.command, false, Some("unknown audio command"))
                .await;
        }
    }
}
