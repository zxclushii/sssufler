use super::{launch, resolve, Launch};
use crate::{commands::emit, config::Config, error::AppError};
use alacritty_terminal::{
    event::{OnResize, WindowSize},
    tty::{self, ChildEvent, EventedPty, EventedReadWrite, Options, Shell},
};
use anyhow::{anyhow, bail, Context, Result};
use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::{Read, Write},
    num::NonZeroU32,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use tauri::{
    ipc::{Channel, InvokeResponseBody},
    AppHandle,
};
use tokio::sync::oneshot;

const TICK: Duration = Duration::from_millis(4);
fn size(cols: u16, rows: u16) -> WindowSize {
    WindowSize {
        num_cols: cols.max(20),
        num_lines: rows.max(5),
        cell_width: 0,
        cell_height: 0,
    }
}

enum Input {
    Write(Vec<u8>, oneshot::Sender<()>),
    Resize(u16, u16),
    Stop,
}
struct Live {
    id: u64,
    pid: Option<NonZeroU32>,
    send: mpsc::Sender<Input>,
    done: mpsc::Receiver<()>,
}
#[derive(Default)]
pub(crate) struct Sessions {
    next: u64,
    live: Option<Live>,
}

fn open(spec: Launch, work: &Path, cols: u16, rows: u16) -> Result<tty::Pty> {
    let opts = Options {
        shell: Some(Shell::new(spec.program, spec.args)),
        working_directory: Some(work.into()),
        drain_on_exit: true,
        env: HashMap::from([
            ("COLORTERM".into(), "truecolor".into()),
            ("FORCE_COLOR".into(), "1".into()),
        ]),
        escape_args: spec.escape,
    };
    tty::new(&opts, size(cols, rows), 0).context("не создал ConPTY")
}

struct WriteRequest {
    bytes: Vec<u8>,
    pos: usize,
    done: oneshot::Sender<()>,
}

fn io_loop(
    pty: &mut tty::Pty,
    rx: mpsc::Receiver<Input>,
    mut output: impl FnMut(Vec<u8>) -> Result<()>,
) -> Result<Option<i32>> {
    let mut buf = vec![0u8; 65536];
    let mut writes = VecDeque::<WriteRequest>::new();
    let mut exited = None;
    let mut last_output = Instant::now();
    loop {
        let n = pty.reader().read(&mut buf).context("не прочитал ConPTY")?;
        let mut busy = n != 0;
        if busy {
            output(buf[..n].to_vec())?;
            last_output = Instant::now();
        }
        if exited.is_none() {
            if let Some(ChildEvent::Exited(status)) = pty.next_child_event() {
                exited = Some(status.and_then(|s| s.code()));
                last_output = Instant::now();
                writes.clear();
            }
        }
        if let Some(code) = exited {
            // The child watcher and the pipe reader signal independently, even for a tiny echo.
            if last_output.elapsed() >= Duration::from_millis(80) {
                return Ok(code);
            }
        } else {
            for _ in 0..64 {
                match rx.try_recv() {
                    Ok(Input::Write(bytes, done)) => {
                        writes.push_back(WriteRequest {
                            bytes,
                            pos: 0,
                            done,
                        });
                        busy = true;
                    }
                    Ok(Input::Resize(cols, rows)) => {
                        pty.on_resize(size(cols, rows));
                        busy = true;
                    }
                    Ok(Input::Stop) | Err(mpsc::TryRecvError::Disconnected) => return Ok(None),
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
            if let Some(w) = writes.front_mut() {
                let count = pty
                    .writer()
                    .write(&w.bytes[w.pos..])
                    .context("не записал в ConPTY")?;
                w.pos += count;
                busy |= count > 0;
                if w.pos == w.bytes.len() {
                    if let Some(w) = writes.pop_front() {
                        if w.done.send(()).is_err() {
                            tracing::debug!("получатель ввода закрылся");
                        }
                    }
                }
            }
        }
        if !busy {
            thread::sleep(TICK);
        }
    }
}
impl Sessions {
    pub(crate) fn start(
        &mut self,
        app: AppHandle,
        dir: &Path,
        cfg: Config,
        dimensions: (u16, u16),
        output: Channel<InvokeResponseBody>,
        restart: bool,
    ) -> Result<u64> {
        if let Some(live) = &self.live {
            if matches!(live.done.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                if !restart {
                    return Ok(live.id);
                }
                self.stop()?;
            } else {
                self.live = None;
            }
        }
        let exe = resolve(&cfg.claude_command)?;
        let prompt = dir.join("claude-prompt.txt");
        fs::write(&prompt, &cfg.system_prompt)
            .with_context(|| format!("не записал {}", prompt.display()))?;
        let system = PathBuf::from(std::env::var_os("SystemRoot").context("нет SystemRoot")?);
        let spec = launch(&exe, &cfg, &prompt, &system)?;
        let work = dir.join("workspace");
        let (tx, rx) = mpsc::channel();
        let (init, ready) = mpsc::sync_channel(1);
        let (finish, done) = mpsc::sync_channel(1);
        self.next += 1;
        let id = self.next;
        thread::Builder::new()
            .name("claude-conpty".into())
            .spawn(move || {
                let mut pty = match open(spec, &work, dimensions.0, dimensions.1) {
                    Ok(p) => p,
                    Err(e) => {
                        if init.send(Err(e)).is_err() {
                            tracing::debug!("запуск отменён");
                        }
                        return;
                    }
                };
                if init.send(Ok(pty.child_watcher().pid())).is_err() {
                    return;
                }
                let code = match io_loop(&mut pty, rx, |bytes| {
                    output
                        .send(InvokeResponseBody::Raw(bytes))
                        .context("не отправил вывод в терминал")
                }) {
                    Ok(code) => code,
                    Err(e) => {
                        crate::commands::warn(&app, &format!("{e:#}"));
                        None
                    }
                };
                // A zero-byte frame follows every output chunk on the same ordered IPC channel.
                if let Err(err) = output.send(InvokeResponseBody::Raw(Vec::new())) {
                    tracing::debug!("окно закрылось до конца вывода: {err}");
                }
                drop(pty);
                emit(
                    &app,
                    "pty_exit",
                    serde_json::json!({"sessionId":id,"code":code}),
                );
                if finish.send(()).is_err() {
                    tracing::debug!("сессия уже забыта");
                }
            })
            .context("не создал поток ConPTY")?;
        let pid = ready.recv().context("прерван запуск ConPTY")??;
        self.live = Some(Live {
            id,
            pid,
            send: tx,
            done,
        });
        Ok(id)
    }
    pub(crate) fn write(&self, id: u64, data: String) -> Result<oneshot::Receiver<()>> {
        let l = self
            .live
            .as_ref()
            .filter(|l| l.id == id)
            .ok_or(AppError::NotRunning)?;
        let (tx, rx) = oneshot::channel();
        l.send
            .send(Input::Write(data.into_bytes(), tx))
            .map_err(|_| AppError::NotRunning)?;
        Ok(rx)
    }
    pub(crate) fn resize(&self, id: u64, cols: u16, rows: u16) -> Result<()> {
        let l = self
            .live
            .as_ref()
            .filter(|l| l.id == id)
            .ok_or(AppError::NotRunning)?;
        l.send
            .send(Input::Resize(cols, rows))
            .map_err(|_| AppError::NotRunning.into())
    }
    pub(crate) fn stop(&mut self) -> Result<()> {
        let Some(l) = self.live.take() else {
            return Ok(());
        };
        if l.send.send(Input::Stop).is_err() {
            tracing::debug!("ввод терминала уже закрыт");
        }
        if l.done.recv_timeout(Duration::from_secs(2)) == Err(mpsc::RecvTimeoutError::Timeout) {
            let pid = l.pid.context("не получил PID для остановки claude")?;
            let system = std::env::var_os("SystemRoot").ok_or_else(|| anyhow!("нет SystemRoot"))?;
            let status =
                std::process::Command::new(PathBuf::from(system).join("System32/taskkill.exe"))
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .creation_flags(0x0800_0000)
                    .output()
                    .context("не запустил taskkill")?;
            if !status.status.success()
                && matches!(l.done.try_recv(), Err(mpsc::TryRecvError::Empty))
            {
                bail!("не остановил claude PID {pid}");
            }
            if l.done.recv_timeout(Duration::from_secs(2)) == Err(mpsc::RecvTimeoutError::Timeout) {
                bail!("ConPTY PID {pid} не закрылся");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn capture(spec: Launch, dir: &Path) -> (Option<i32>, String) {
        let mut pty = open(spec, dir, 160, 32).unwrap();
        let (tx, rx) = mpsc::channel();
        let mut bytes = Vec::new();
        let timer = tx.clone();
        let (stop, watch) = mpsc::channel();
        let watchdog = thread::spawn(move || {
            if watch.recv_timeout(Duration::from_secs(30)).is_err()
                && timer.send(Input::Stop).is_err()
            {
                tracing::debug!("тестовый процесс завершён");
            }
        });
        let code = io_loop(&mut pty, rx, |chunk| {
            bytes.extend(chunk);
            Ok(())
        })
        .unwrap();
        stop.send(()).unwrap();
        watchdog.join().unwrap();
        drop(pty);
        (code, String::from_utf8(bytes).unwrap())
    }

    #[test]
    fn conpty_reports_cmd_output_and_exit() {
        let dir = crate::paths::test_dir();
        // The batch is ASCII. Its Unicode argument comes through the same CMD launcher as Claude.
        let shim = dir.join("console-output.cmd");
        fs::write(
            &shim,
            "@echo off\r\necho SUF-BEGIN-%~1-SUF-END\r\nexit /b 0\r\n",
        )
        .unwrap();
        let cfg = Config {
            claude_args: "привет".into(),
            ..Config::default()
        };
        let windows = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        let spec = launch(&shim, &cfg, &dir.join("prompt.txt"), &windows).unwrap();
        let (code, output) = capture(spec, &dir);
        assert_eq!(code, Some(0), "{output:?}");
        assert!(output.contains("SUF-BEGIN-привет-SUF-END"), "{output:?}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn conpty_preserves_100000_cyrillic_letters() {
        let dir = crate::paths::test_dir();
        let phrase = "абвгдежзий".repeat(8);
        let content = (0..1250)
            .map(|i| format!("{i:04} {phrase}\r\n"))
            .collect::<String>();
        fs::write(dir.join("russian.txt"), content).unwrap();
        // Specify both encodings so the producer's locale cannot change the fixture.
        let script = r#"$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$file = (Get-Item -LiteralPath './russian.txt').FullName
foreach ($line in [IO.File]::ReadLines($file, [Text.Encoding]::UTF8)) {
    [Console]::WriteLine($line)
}
[Console]::WriteLine('END-OF-STREAM')
[Console]::Out.Flush()"#;
        let powershell = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        let spec = Launch {
            program: format!("\"{}\"", powershell.display()),
            args: [
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                script,
            ]
            .map(String::from)
            .into(),
            escape: true,
        };
        let (code, output) = capture(spec, &dir);
        let first: String = output.chars().take(180).collect();
        let tail: String = output
            .chars()
            .rev()
            .take(180)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        assert_eq!(code, Some(0), "{first:?} … {tail:?}");
        assert!(output.contains("END-OF-STREAM"), "{first:?} … {tail:?}");
        assert!(!output.contains(char::REPLACEMENT_CHARACTER));
        assert_eq!(
            output.chars().filter(|c| ('а'..='я').contains(c)).count(),
            100_000,
            "{} bytes, {first:?} … {tail:?}",
            output.len()
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
