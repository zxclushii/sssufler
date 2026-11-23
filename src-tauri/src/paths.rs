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
    use io::{Seek, SeekFrom};
    const LIMIT: usize = 5 * 1024 * 1024;
    let path = dir.join("logs/suflyor.log");
    struct Capped {
        file: fs::File,
        bytes: u64,
    }
    impl io::Write for Capped {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.bytes.saturating_add(bytes.len() as u64) > LIMIT as u64 {
                self.file.set_len(0)?;
                self.file.rewind()?;
                self.bytes = 0;
            }
            let skipped = bytes.len().saturating_sub(LIMIT);
            let written = self.file.write(&bytes[skipped..])?;
            self.bytes += written as u64;
            Ok(skipped + written)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.file.flush()
        }
    }
    // An append-only Windows handle cannot shorten the file when it hits the cap.
    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("не открыл {}", path.display()))?;
    if file.metadata()?.len() >= LIMIT as u64 {
        file.set_len(0)
            .with_context(|| format!("не очистил {}", path.display()))?;
    }
    let bytes = file.seek(SeekFrom::End(0))?;
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(Capped { file, bytes }))
        .try_init()
        .map_err(|err| anyhow::anyhow!("не включил лог: {err}"))
}

#[cfg(windows)]
pub(crate) fn startup_error(detail: &str) {
    use std::{io::Write, os::windows::process::CommandExt, process::Command};
    let message = format!(
        "Суфлёр не запустился.\r\n{detail}\r\nWebView2 Runtime: https://developer.microsoft.com/microsoft-edge/webview2/"
    );
    tracing::error!(%message);
    let system =
        PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
            .join("System32");
    // Show the failure independently of the WebView and of the failed process's lifetime.
    let show_note = || -> io::Result<()> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let name = format!("suflyor-startup-{}-{stamp}.txt", std::process::id());
        let path = std::env::temp_dir().join(name);
        let mut file = fs::File::create_new(&path)?;
        file.write_all(b"\xef\xbb\xbf")?;
        file.write_all(message.as_bytes())?;
        drop(file);
        drop(Command::new(system.join("notepad.exe")).arg(path).spawn()?);
        Ok(())
    };
    if let Err(err) = show_note() {
        tracing::warn!("не открыл сообщение в Блокноте: {err}");
        let user = std::env::var_os("USERNAME").unwrap_or_else(|| "console".into());
        if let Err(err) = Command::new(system.join("msg.exe"))
            .arg(user)
            .arg("/time:120")
            .arg(&message)
            .creation_flags(0x0800_0000)
            .spawn()
        {
            tracing::error!("не показал ошибку запуска: {err}");
        }
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
