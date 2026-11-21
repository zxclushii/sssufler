use crate::config::Bounds;
use anyhow::{Context, Result};
use tauri::{
    webview::NewWindowResponse, App, AppHandle, LogicalPosition, LogicalSize, Manager, Monitor,
    WebviewWindow, WebviewWindowBuilder,
};

pub(crate) fn main_window(app: &AppHandle) -> Result<WebviewWindow> {
    app.get_webview_window("main")
        .context("окно Суфлёра закрыто")
}

fn area(m: &Monitor) -> (f64, f64, f64, f64) {
    let a = m.work_area();
    let p = a.position.to_logical::<f64>(m.scale_factor());
    let s = a.size.to_logical::<f64>(m.scale_factor());
    (p.x, p.y, s.width, s.height)
}
fn visible(b: &Bounds, m: &Monitor) -> bool {
    let (x, y, w, h) = area(m);
    let (bx, by) = (f64::from(b.x), f64::from(b.y));
    b.width >= 380
        && b.height >= 260
        && bx + 80. > x
        && by + 40. > y
        && bx < x + w - 80.
        && by < y + h - 40.
}
pub(crate) fn create(app: &App, saved: Option<&Bounds>) -> Result<WebviewWindow> {
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == "main")
        .context("нет конфигурации окна main")?;
    let win = WebviewWindowBuilder::from_config(app, config)?
        .on_navigation(|url| {
            matches!(
                (url.scheme(), url.host_str()),
                ("tauri", Some("localhost")) | ("http" | "https", Some("tauri.localhost"))
            )
        })
        .on_new_window(|_, _| NewWindowResponse::Deny)
        .build()
        .context("не создал окно WebView2")?;
    let monitors = win.available_monitors()?;
    let bounds = saved
        .filter(|b| monitors.iter().any(|m| visible(b, m)))
        .cloned()
        .or_else(|| {
            let primary = win
                .primary_monitor()
                .ok()
                .flatten()
                .or_else(|| monitors.first().cloned())?;
            let (x, y, w, h) = area(&primary);
            Some(Bounds {
                x: (x + (w - 640. - 24.).max(0.)) as i32,
                y: (y + (h - 660. - 24.).max(0.)) as i32,
                width: 640,
                height: 660,
            })
        });
    if let Some(b) = bounds {
        // Electron writes DIP bounds. Target the selected monitor before setting the DPI-dependent size.
        let scale = monitors
            .iter()
            .find(|m| visible(&b, m))
            .map_or(1., Monitor::scale_factor);
        win.set_position(LogicalPosition::new(b.x, b.y).to_physical::<i32>(scale))?;
        win.set_size(LogicalSize::new(b.width, b.height).to_physical::<u32>(scale))?;
    }
    Ok(win)
}
pub(crate) fn bounds(win: &WebviewWindow) -> Result<Bounds> {
    let scale = win.scale_factor()?;
    let pos = win.outer_position()?.to_logical::<i32>(scale);
    let size = win.inner_size()?.to_logical::<u32>(scale);
    Ok(Bounds {
        x: pos.x,
        y: pos.y,
        width: size.width,
        height: size.height,
    })
}
pub(crate) fn show(win: &WebviewWindow) -> Result<()> {
    win.set_focusable(false)?;
    let shown = win.unminimize().and_then(|()| win.show());
    let restored = win.set_focusable(true);
    shown?;
    restored?;
    Ok(())
}
