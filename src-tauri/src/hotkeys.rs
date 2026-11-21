use crate::{
    commands::{emit, warn, Runtime},
    window,
};
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
pub(crate) fn register(app: &AppHandle) -> Vec<String> {
    let mut warnings = Vec::new();
    for (key, action) in [
        ("Ctrl+Alt+L", "listen"),
        ("Ctrl+Alt+Space", "send"),
        ("Ctrl+Alt+X", "clear"),
        ("Ctrl+Alt+H", "hide"),
    ] {
        let result = app
            .global_shortcut()
            .on_shortcut(key, move |app, _, event| {
                if event.state != ShortcutState::Pressed {
                    return;
                }
                match action {
                    "listen" => {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            if let Some(state) = app.try_state::<Runtime>() {
                                let cfg = state.config();
                                if let Err(e) = state.audio.set(None, cfg).await {
                                    warn(&app, &format!("{e:#}"));
                                }
                            }
                        });
                    }
                    "hide" => {
                        let result = window::main_window(app).and_then(|win| {
                            if win.is_visible()? {
                                win.hide()?;
                            } else {
                                window::show(&win)?;
                            }
                            Ok(())
                        });
                        if let Err(e) = result {
                            warn(app, &format!("{e:#}"));
                        }
                    }
                    _ => emit(app, "hotkey", action),
                }
            });
        if let Err(e) = result {
            tracing::warn!(key, "не зарегистрировал хоткей: {e}");
            warnings.push(format!("занят хоткей {key}"));
        }
    }
    warnings
}
