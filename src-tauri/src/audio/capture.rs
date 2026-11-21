use super::segmenter::{Reason, Segmenter, SilenceClock};
use crate::{
    commands::{emit, warn},
    config::Config,
    whisper::queue::Queue,
};
use anyhow::{anyhow, bail, Context, Result};
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    Device, DeviceId, Host, SampleFormat, Stream,
};
use serde::Serialize;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::AppHandle;
use tokio::sync::oneshot;

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Serialize)]
pub(crate) struct DeviceInfo {
    id: String,
    name: String,
    kind: &'static str,
}
enum Control {
    Set(Option<bool>, Box<Config>, oneshot::Sender<Result<()>>),
    Devices(oneshot::Sender<Result<Vec<DeviceInfo>>>),
    Exit,
}
#[derive(Clone)]
pub(crate) struct Audio(mpsc::Sender<Control>);
impl Audio {
    pub(crate) fn new(app: AppHandle, queue: Queue) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("wasapi".into())
            .spawn(move || run(app, queue, rx))
            .context("не запустил захват звука")?;
        Ok(Self(tx))
    }
    pub(crate) async fn set(&self, on: Option<bool>, cfg: Config) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.0
            .send(Control::Set(on, Box::new(cfg), tx))
            .map_err(|_| anyhow!("захват звука остановлен"))?;
        rx.await.context("захват звука завершился")?
    }
    pub(crate) async fn devices(&self) -> Result<Vec<DeviceInfo>> {
        let (tx, rx) = oneshot::channel();
        self.0
            .send(Control::Devices(tx))
            .map_err(|_| anyhow!("захват звука остановлен"))?;
        rx.await.context("список устройств недоступен")?
    }
    pub(crate) fn shutdown(&self) {
        if self.0.send(Control::Exit).is_err() {
            tracing::debug!("захват уже завершён");
        }
    }
}
struct Capture {
    stream: Stream,
    rx: mpsc::Receiver<Vec<f32>>,
    samples: Vec<f32>,
    rate: u32,
    seg: Segmenter,
    clock: SilenceClock,
    last_level: Instant,
    probe: Instant,
    last_log: Instant,
    lost: Arc<AtomicU64>,
    failed: Arc<AtomicBool>,
    id: DeviceId,
    cfg: Config,
}

fn list(host: &Host) -> Result<Vec<DeviceInfo>> {
    host.input_devices()?
        .map(|device| {
            Ok(DeviceInfo {
                id: device.id()?.to_string(),
                name: device.description()?.name().to_owned(),
                kind: "audioinput",
            })
        })
        .collect()
}

fn select(host: &Host, source: &str, app: &AppHandle) -> Result<Device> {
    if source == "system" {
        return host
            .default_output_device()
            .context("нет устройства вывода для системного звука");
    }
    if source != "default" && !source.is_empty() {
        if let Some(device) = source
            .parse::<DeviceId>()
            .ok()
            .and_then(|id| host.device_by_id(&id))
        {
            return Ok(device);
        }
        warn(
            app,
            "сохранённый микрофон недоступен, выбран микрофон по умолчанию",
        );
    }
    host.default_input_device()
        .context("нет микрофона по умолчанию")
}

impl Capture {
    fn open(host: &Host, cfg: Config, app: &AppHandle) -> Result<Self> {
        let device = select(host, &cfg.source, app)?;
        let id = device.id()?;
        let fmt = if cfg.source == "system" {
            device.default_output_config()?
        } else {
            device.default_input_config()?
        };
        let channels = usize::from(fmt.channels());
        let rate = fmt.sample_rate();
        if channels == 0 || rate == 0 {
            bail!("устройство вернуло пустой формат звука");
        }
        let (tx, rx) = mpsc::sync_channel(64);
        let lost = Arc::new(AtomicU64::new(0));
        let failed = Arc::new(AtomicBool::new(false));
        let dropped = lost.clone();
        let error = failed.clone();
        let send = move |frame| {
            if let Err(mpsc::TrySendError::Full(_)) = tx.try_send(frame) {
                dropped.fetch_add(1, Ordering::Relaxed);
            }
        };
        let on_error = move |err| {
            error.store(true, Ordering::Release);
            tracing::warn!("WASAPI: {err}");
        };
        let stream = match fmt.sample_format() {
            SampleFormat::F32 => device.build_input_stream(
                fmt.config(),
                move |data: &[f32], _| {
                    send(
                        data.chunks_exact(channels)
                            .map(|c| c.iter().sum::<f32>() / channels as f32)
                            .collect(),
                    )
                },
                on_error,
                None,
            ),
            SampleFormat::I16 => device.build_input_stream(
                fmt.config(),
                move |data: &[i16], _| {
                    send(
                        data.chunks_exact(channels)
                            .map(|c| {
                                c.iter().map(|&s| s as f32 / 32768.).sum::<f32>() / channels as f32
                            })
                            .collect(),
                    )
                },
                on_error,
                None,
            ),
            other => bail!("неподдерживаемый формат звука {other:?}, нужны f32 или i16"),
        }
        .context("не открыл поток WASAPI")?;
        stream.play().context("не запустил поток WASAPI")?;
        let t = Instant::now();
        Ok(Self {
            stream,
            rx,
            samples: Vec::with_capacity(4096),
            rate,
            seg: Segmenter::new(rate, cfg.silence_ms, cfg.sensitivity),
            clock: SilenceClock::new(t),
            last_level: t,
            probe: t,
            last_log: t,
            lost,
            failed,
            id,
            cfg,
        })
    }
    fn frame(&mut self, frame: Vec<f32>, app: &AppHandle, queue: &Queue) {
        let (meter, segment) = self.seg.push(frame, now_ms());
        if self.last_level.elapsed() >= Duration::from_millis(50) {
            emit(
                app,
                "level",
                serde_json::json!({"level":meter.level,"speaking":meter.speaking}),
            );
            self.last_level = Instant::now();
        }
        if let Some(s) = segment {
            queue.push(s);
        }
    }
    fn pump(&mut self, app: &AppHandle, queue: &Queue) {
        for _ in 0..64 {
            let Ok(data) = self.rx.try_recv() else { break };
            self.samples.extend(data);
            self.clock.input(Instant::now());
        }
        let missing = self.clock.missing(Instant::now(), self.rate);
        self.samples.resize(self.samples.len() + missing, 0.);
        while self.samples.len() >= 2048 {
            let frame = self.samples.drain(..2048).collect();
            self.frame(frame, app, queue);
        }
        if self.last_log.elapsed() >= Duration::from_secs(1) {
            let lost = self.lost.swap(0, Ordering::Relaxed);
            if lost > 0 {
                tracing::warn!(lost, "пропущены звуковые кадры");
            }
            self.last_log = Instant::now();
        }
    }
    fn finish(mut self, app: &AppHandle, queue: &Queue) {
        if let Err(e) = self.stream.pause() {
            tracing::debug!("поток при остановке уже недоступен: {e}");
        }
        self.pump(app, queue);
        if !self.samples.is_empty() {
            let rest = std::mem::take(&mut self.samples);
            self.frame(rest, app, queue);
        }
        if let Some(s) = self.seg.finish(Reason::Stop, now_ms()) {
            queue.push(s);
        }
    }
}

fn set_state(app: &AppHandle, state: &str, message: Option<String>) {
    emit(
        app,
        "listen_state",
        serde_json::json!({"state":state,"message":message}),
    );
}
fn run(app: AppHandle, queue: Queue, controls: mpsc::Receiver<Control>) {
    let host = cpal::default_host();
    let mut active: Option<Capture> = None;
    loop {
        let incoming = if active.is_some() {
            controls.recv_timeout(Duration::from_millis(10))
        } else {
            controls
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        };
        match incoming {
            Ok(Control::Set(on, cfg, reply)) => {
                let want = on.unwrap_or(active.is_none());
                let result = if want && active.is_none() {
                    set_state(&app, "starting", None);
                    Capture::open(&host, *cfg, &app).map(|c| {
                        active = Some(c);
                        set_state(&app, "on", None);
                    })
                } else {
                    if !want {
                        if let Some(c) = active.take() {
                            c.finish(&app, &queue);
                        }
                        set_state(&app, "off", None);
                    }
                    Ok(())
                };
                if let Err(err) = &result {
                    set_state(&app, "error", Some(format!("{err:#}")));
                }
                if reply.send(result).is_err() {
                    tracing::debug!("окно закрылось до ответа WASAPI");
                }
            }
            Ok(Control::Devices(reply)) => {
                if reply.send(list(&host)).is_err() {
                    tracing::debug!("список устройств больше не нужен");
                }
            }
            Ok(Control::Exit) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                if let Some(c) = active.take() {
                    c.finish(&app, &queue);
                }
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some(c) = active.as_mut() {
            c.pump(&app, &queue);
            let failed = c.failed.load(Ordering::Acquire);
            let switched = if c.cfg.source == "system"
                && (failed || c.probe.elapsed() >= Duration::from_secs(2))
            {
                c.probe = Instant::now();
                host.default_output_device()
                    .and_then(|d| d.id().ok())
                    .is_some_and(|id| id != c.id)
            } else {
                false
            };
            if switched || failed {
                let cfg = c.cfg.clone();
                if let Some(old) = active.take() {
                    old.finish(&app, &queue);
                }
                if switched {
                    match Capture::open(&host, cfg, &app) {
                        Ok(c) => active = Some(c),
                        Err(e) => set_state(&app, "error", Some(format!("{e:#}"))),
                    }
                } else {
                    set_state(
                        &app,
                        "error",
                        Some("поток звука оборвался, включи заново".into()),
                    );
                }
            }
        }
    }
}
