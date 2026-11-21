use anyhow::{Context, Result};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub(crate) fn remove_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(windows)]
pub(crate) fn data_dir() -> Result<(PathBuf, Vec<String>)> {
    let exe = std::env::current_exe().context("не получил путь к suflyor.exe")?;
    let parent = exe.parent().context("у exe нет родительской папки")?;
    let probe = parent.join(format!(".suflyor-write-{}", std::process::id()));
    let (dir, warnings) = if let Ok(file) = fs::File::create_new(&probe) {
        drop(file);
        remove_file(&probe).with_context(|| format!("не удалил {}", probe.display()))?;
        (parent.to_owned(), Vec::new())
    } else {
        let dir = PathBuf::from(std::env::var_os("LOCALAPPDATA").context("нет LOCALAPPDATA")?)
            .join("suflyor");
        let msg = format!("настройки и модели сохраняются в {}", dir.display());
        (dir, vec![msg])
    };
    for part in ["workspace", "logs", "whisper/bin", "whisper/models"] {
        fs::create_dir_all(dir.join(part))
            .with_context(|| format!("не создал {part} в {}", dir.display()))?;
    }
    Ok((dir, warnings))
}

#[cfg(windows)]
pub(crate) fn log(dir: &Path) -> Result<()> {
    let path = dir.join("logs/suflyor.log");
    // Truncate on startup and enforce the same cap throughout a long session.
    struct Capped {
        file: fs::File,
        bytes: u64,
    }
    impl io::Write for Capped {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            use io::Seek;
            if self.bytes + bytes.len() as u64 > 5 * 1024 * 1024 {
                self.file.set_len(0)?;
                self.file.rewind()?;
                self.bytes = 0;
            }
            let written = self.file.write(bytes)?;
            self.bytes += written as u64;
            Ok(written)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.file.flush()
        }
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("не открыл {}", path.display()))?;
    let mut bytes = file.metadata()?.len();
    if bytes >= 5 * 1024 * 1024 {
        file.set_len(0)?;
        bytes = 0;
    }
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(Capped { file, bytes }))
        .try_init()
        .map_err(|err| anyhow::anyhow!("не включил лог: {err}"))
}

// Windows 1809 includes msg.exe. A GUI-subsystem binary has no console on double click.
#[cfg(windows)]
pub(crate) fn startup_error(detail: &str) {
    use std::os::windows::process::CommandExt;
    let message=format!("Суфлёр не запустился.\n{detail}\nWebView2 Runtime: https://developer.microsoft.com/microsoft-edge/webview2/");
    tracing::error!(%message);
    let system = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let user = std::env::var_os("USERNAME").unwrap_or_else(|| "console".into());
    if let Err(err) = std::process::Command::new(PathBuf::from(system).join("System32/msg.exe"))
        .arg(user)
        .arg("/time:120")
        .arg(&message)
        .creation_flags(0x0800_0000)
        .spawn()
    {
        tracing::error!("не показал ошибку запуска: {err}");
    }
}

#[cfg(test)]
pub(crate) fn test_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static ID: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "suflyor-test-{}-{}",
        std::process::id(),
        ID.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}
