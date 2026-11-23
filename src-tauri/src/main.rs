#![forbid(unsafe_code)]
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(any(windows, test))]
mod audio;
#[cfg(any(windows, test))]
mod claude;
#[cfg(windows)]
mod commands;
#[cfg(any(windows, test))]
mod config;
#[cfg(any(windows, test))]
mod error;
#[cfg(windows)]
mod hotkeys;
#[cfg(any(windows, test))]
mod paths;
#[cfg(any(windows, test))]
mod whisper;
#[cfg(windows)]
mod window;

#[cfg(windows)]
fn main() {
    let status = match commands::run() {
        Ok(status) => status,
        Err(err) => {
            paths::startup_error(&format!("{err:#}"));
            1
        }
    };
    std::process::exit(status);
}

#[cfg(not(windows))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("Сборка рабочего окна Суфлёра поддерживается на Windows 10/11")
}
