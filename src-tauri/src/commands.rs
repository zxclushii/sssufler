use crate::{
    audio::capture::{Audio, DeviceInfo},
    claude::Sessions,
    config::Config,
    error::{AppError, CmdError},
    hotkeys, paths,
    whisper::{engine::Engine, queue::Queue},
    window,
};
use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::{Map, Value};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};
use tauri::{
    ipc::{Channel, InvokeResponseBody},
    AppHandle, Emitter, Manager, State, WebviewWindow, WindowEvent,
};
type Reply<T> = std::result::Result<T, CmdError>;
pub(crate) struct Runtime {
    dir: PathBuf,
    cfg: Mutex<Config>,
    warnings: Mutex<Vec<String>>,
    engine: Engine,
    pub(crate) audio: Audio,
    sessions: Mutex<Sessions>,
    pinned: AtomicBool,
    ready: AtomicBool,
}
impl Runtime {
    pub(crate) fn config(&self) -> Config {
        self.cfg.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}
pub(crate) fn emit(app: &AppHandle, event: &str, payload: impl Serialize + Clone) {
    if let Err(e) = app.emit_to("main", event, payload) {
        tracing::warn!(event, "не доставил событие: {e}");
    }
}
pub(crate) fn warn(app: &AppHandle, message: &str) {
    tracing::warn!(message);
    emit(
        app,
        "status_warning",
        serde_json::json!({"message":message}),
    );
}

#[tauri::command]
fn config_get(s: State<'_, Runtime>) -> Reply<Value> {
    Ok(
        serde_json::json!({"config":s.config(),"warnings":s.warnings.lock().unwrap_or_else(|e|e.into_inner()).clone(),"dataDir":s.dir,"engine":s.engine.view.borrow().clone(),"pinned":s.pinned.load(Ordering::Relaxed)}),
    )
}
#[tauri::command]
fn config_set(app: AppHandle, s: State<'_, Runtime>, patch: Map<String, Value>) -> Reply<Config> {
    let mut guard = s.cfg.lock().unwrap_or_else(|e| e.into_inner());
    let old = &*guard;
    let (new, warnings) = old.patched(patch)?;
    let restart = old.whisper_model != new.whisper_model
        || old.whisper_threads != new.whisper_threads
        || old.whisper_url != new.whisper_url
        || old.language != new.language;
    if let Err(e) = new.save(&s.dir) {
        warn(&app, &format!("{e:#}"));
        return Err(e.into());
    }
    *guard = new.clone();
    if restart {
        s.engine.configure(new.clone())?;
    }
    for w in warnings {
        warn(&app, &w);
    }
    Ok(new)
}
async fn start(
    app: AppHandle,
    cols: u16,
    rows: u16,
    on_data: Channel<InvokeResponseBody>,
    restart: bool,
) -> Reply<u64> {
    tauri::async_runtime::spawn_blocking(move || {
        let s = app.state::<Runtime>();
        let cfg = s.config();
        let mut sessions = s.sessions.lock().unwrap_or_else(|p| p.into_inner());
        sessions.start(app.clone(), &s.dir, cfg, (cols, rows), on_data, restart)
    })
    .await
    .context("прерван запуск claude")?
    .map_err(Into::into)
}
#[tauri::command]
async fn pty_start(
    app: AppHandle,
    cols: u16,
    rows: u16,
    on_data: Channel<InvokeResponseBody>,
) -> Reply<u64> {
    start(app, cols, rows, on_data, false).await
}
#[tauri::command]
async fn pty_restart(
    app: AppHandle,
    cols: u16,
    rows: u16,
    on_data: Channel<InvokeResponseBody>,
) -> Reply<u64> {
    start(app, cols, rows, on_data, true).await
}
#[tauri::command]
async fn pty_write(s: State<'_, Runtime>, session_id: u64, data: String) -> Reply<()> {
    let ack = s
        .sessions
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .write(session_id, data)?;
    ack.await
        .map_err(|_| CmdError::from(anyhow::Error::from(AppError::NotRunning)))
}
#[tauri::command]
fn pty_resize(s: State<'_, Runtime>, session_id: u64, cols: u16, rows: u16) -> Reply<()> {
    s.sessions
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .resize(session_id, cols, rows)
        .map_err(Into::into)
}
#[tauri::command]
async fn listen_start(s: State<'_, Runtime>) -> Reply<()> {
    let cfg = s.config();
    s.audio.set(Some(true), cfg).await.map_err(Into::into)
}
#[tauri::command]
async fn listen_stop(s: State<'_, Runtime>) -> Reply<()> {
    let cfg = s.config();
    s.audio.set(Some(false), cfg).await.map_err(Into::into)
}
#[tauri::command]
async fn audio_devices(s: State<'_, Runtime>) -> Reply<Vec<DeviceInfo>> {
    s.audio.devices().await.map_err(Into::into)
}
#[tauri::command]
fn win_minimize(window: WebviewWindow) -> Reply<()> {
    window.minimize().context("не свернул окно")?;
    Ok(())
}
#[tauri::command]
fn win_close(window: WebviewWindow) -> Reply<()> {
    window.close().context("не закрыл окно")?;
    Ok(())
}
#[tauri::command]
fn win_set_pin(window: WebviewWindow, s: State<'_, Runtime>, value: bool) -> Reply<bool> {
    window
        .set_always_on_top(value)
        .context("не установил положение поверх окон")?;
    s.pinned.store(value, Ordering::Relaxed);
    Ok(value)
}
#[tauri::command]
async fn ui_ready(app: AppHandle, s: State<'_, Runtime>) -> Reply<()> {
    if !s.ready.swap(true, Ordering::AcqRel) {
        let engine = s.engine.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = engine.ensure().await {
                tracing::warn!("Whisper: {e:#}");
            }
        });
        let cfg = s.config();
        if let Err(e) = s.audio.set(Some(true), cfg).await {
            warn(&app, &format!("{e:#}"));
        }
    }
    Ok(())
}
fn setup(app: &mut tauri::App) -> Result<()> {
    let (dir, mut warnings) = paths::data_dir()?;
    paths::log(&dir)?;
    let (cfg, mut config_warnings) = Config::load(&dir)?;
    warnings.append(&mut config_warnings);
    let handle = app.handle().clone();
    let engine = Engine::new(handle.clone(), dir.clone(), cfg.clone())?;
    let queue = Queue::new(handle.clone(), engine.clone())?;
    let audio = Audio::new(handle.clone(), queue)?;
    let saved = cfg.bounds.clone();
    warnings.extend(hotkeys::register(&handle));
    for w in &warnings {
        tracing::warn!("{w}");
    }
    app.manage(Runtime {
        dir,
        cfg: Mutex::new(cfg),
        warnings: Mutex::new(warnings),
        engine,
        audio,
        sessions: Default::default(),
        pinned: AtomicBool::new(true),
        ready: AtomicBool::new(false),
    });
    let win = window::create(app, saved.as_ref())?;
    let events = win.clone();
    win.on_window_event(move |ev| {
        let state = handle.state::<Runtime>();
        match ev {
            WindowEvent::Focused(false) if state.pinned.load(Ordering::Relaxed) => {
                if let Err(e) = events.set_always_on_top(true) {
                    warn(&handle, &format!("не закрепил окно: {e}"));
                }
            }
            WindowEvent::CloseRequested { .. } => {
                let mut cfg = state.cfg.lock().unwrap_or_else(|e| e.into_inner());
                let result = window::bounds(&events).and_then(|b| {
                    cfg.bounds = Some(b);
                    cfg.save(&state.dir)
                });
                if let Err(e) = result {
                    warn(&handle, &format!("{e:#}"));
                }
            }
            _ => {}
        }
    });
    window::show(&win)?;
    Ok(())
}

pub(crate) fn run() -> Result<i32> {
    // Configure terminal inheritance before Tauri starts its worker threads.
    std::env::set_var("COLORTERM", "truecolor");
    std::env::set_var("FORCE_COLOR", "1");
    std::env::remove_var("TERM_PROGRAM");
    std::env::remove_var("TERM_PROGRAM_VERSION");
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Err(e) = window::main_window(app).and_then(|w| window::show(&w)) {
                tracing::warn!("не показал первое окно: {e:#}");
            }
        }))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            config_get,
            config_set,
            pty_start,
            pty_restart,
            pty_write,
            pty_resize,
            listen_start,
            listen_stop,
            audio_devices,
            win_minimize,
            win_close,
            win_set_pin,
            ui_ready
        ])
        .setup(|app| {
            // Tauri panics on a setup-hook error before run() can report it.
            if let Err(err) = setup(app) {
                paths::startup_error(&format!("{err:#}"));
                app.handle().exit(1);
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .map_err(|e| anyhow!("не запустил Суфлёр: {e}"))?;
    Ok(app.run_return(|app, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            if let Some(s) = app.try_state::<Runtime>() {
                s.audio.shutdown();
                s.engine.shutdown();
                if let Err(e) = s.sessions.lock().unwrap_or_else(|p| p.into_inner()).stop() {
                    tracing::warn!("не остановил Claude: {e:#}");
                }
            }
        }
    }))
}
