use super::{step, Effect, Event, Phase, Snapshot, State};
use crate::{commands::emit, config::Config, whisper::download};
use anyhow::{anyhow, bail, Context, Result};
use process_wrap::tokio::{ChildWrapper, CommandWrap, CreationFlags, JobObject, KillOnDrop};
use std::{
    collections::VecDeque,
    net::TcpListener,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tauri::AppHandle;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::{mpsc, oneshot, watch, OwnedSemaphorePermit, Semaphore},
    task::JoinHandle,
};

pub(crate) fn job(command: Command) -> Result<Box<dyn ChildWrapper>> {
    let mut creation = CreationFlags(Default::default());
    creation.0 .0 = 0x0800_0000; // CREATE_NO_WINDOW, carried through JobObject
    Ok(CommandWrap::from(command)
        .wrap(JobObject)
        .wrap(KillOnDrop)
        .wrap(creation)
        .spawn()?)
}
type Logs = Arc<Mutex<VecDeque<u8>>>;
struct Server {
    child: Box<dyn ChildWrapper>,
    url: String,
    log: Logs,
    _permit: OwnedSemaphorePermit,
}
impl Server {
    fn tail(&self) -> String {
        let guard = self.log.lock().unwrap_or_else(|e| e.into_inner());
        let bytes: Vec<_> = guard.iter().copied().collect();
        let log = String::from_utf8_lossy(&bytes);
        log.chars()
            .rev()
            .take(3000)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }
}
async fn log_pipe(mut r: impl AsyncRead + Unpin, log: Logs) {
    let mut buf = [0u8; 4096];
    loop {
        match r.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let mut l = log.lock().unwrap_or_else(|e| e.into_inner());
                l.extend(&buf[..n]);
                let excess = l.len().saturating_sub(12000);
                l.drain(..excess);
            }
            Err(e) => {
                tracing::warn!("не прочитал лог Whisper: {e}");
                break;
            }
        }
    }
}
enum Msg {
    Ensure(oneshot::Sender<Result<String>>),
    Configure(Config),
    Update(u64, Phase, String, Option<u64>),
    Done(u64, Result<Server>),
    Shutdown,
}
#[derive(Clone)]
pub(crate) struct Engine {
    tx: mpsc::UnboundedSender<Msg>,
    pub view: watch::Receiver<Snapshot>,
}
impl Engine {
    pub(crate) fn new(app: AppHandle, dir: PathBuf, cfg: Config) -> Result<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        let (state, _) = step(
            State::default(),
            Event::Reconfigure(cfg.whisper_url.clone()),
        );
        let (view, snapshot) = watch::channel(state.view.clone());
        let client = download::client(Duration::from_secs(30))?;
        let health = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_millis(1500))
            .build()?;
        let actor = Actor {
            app,
            dir,
            cfg,
            tx: tx.clone(),
            view,
            state,
            client,
            health,
            boot: None,
            server: None,
            waiters: Vec::new(),
            gate: Arc::new(Semaphore::new(1)),
        };
        tauri::async_runtime::spawn(actor.run(rx));
        Ok(Self { tx, view: snapshot })
    }
    pub(crate) async fn ensure(&self) -> Result<String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Msg::Ensure(tx))
            .map_err(|_| anyhow!("движок Whisper завершён"))?;
        rx.await.context("потерян ответ Whisper")?
    }
    pub(crate) fn configure(&self, c: Config) -> Result<()> {
        self.tx
            .send(Msg::Configure(c))
            .map_err(|_| anyhow!("движок Whisper завершён"))
    }
    pub(crate) fn shutdown(&self) {
        if self.tx.send(Msg::Shutdown).is_err() {
            tracing::debug!("Whisper уже остановлен");
        }
    }
}
struct Actor {
    app: AppHandle,
    dir: PathBuf,
    cfg: Config,
    tx: mpsc::UnboundedSender<Msg>,
    view: watch::Sender<Snapshot>,
    state: State,
    client: reqwest::Client,
    health: reqwest::Client,
    boot: Option<JoinHandle<()>>,
    server: Option<Server>,
    waiters: Vec<oneshot::Sender<Result<String>>>,
    gate: Arc<Semaphore>,
}
impl Actor {
    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Msg>) {
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        loop {
            let mut result = None;
            let mut restart = false;
            let event = tokio::select! {
                msg=rx.recv()=>match msg{
                    Some(Msg::Ensure(reply))=>{self.waiters.push(reply);Event::Ensure(Instant::now())}
                    Some(Msg::Configure(cfg))=>{let url=cfg.whisper_url.clone();self.cfg=cfg;restart=true;Event::Reconfigure(url)}
                    Some(Msg::Update(g,p,t,n))=>Event::Progress(g,p,t,n),
                    Some(Msg::Done(g,Ok(s)))=>{let url=s.url.clone();result=Some(s);Event::BootDone(g,url)}
                    Some(Msg::Done(g,Err(e)))=>Event::Failed(g,format!("{e:#}"),Instant::now()),
                    Some(Msg::Shutdown)|None=>Event::Shutdown,
                },
                _=tick.tick(),if self.server.is_some()=>{
                    let Some(s)=self.server.as_mut()else{continue};
                    let msg=match s.child.try_wait(){Ok(None)=>continue,Ok(Some(code))=>format!("Whisper остановился ({code}): {}",s.tail()),Err(e)=>format!("не проверил whisper-server: {e}")};
                    Event::Failed(self.state.gen,msg,Instant::now())
                }
            };
            let (state, mut effects) = step(self.state.clone(), event);
            self.state = state;
            if restart && self.state.view.state == Phase::Idle {
                let (state, more) = step(self.state.clone(), Event::Ensure(Instant::now()));
                self.state = state;
                effects.extend(more);
            }
            for effect in effects {
                match effect {
                    Effect::Cancel => {
                        if let Some(h) = self.boot.take() {
                            h.abort();
                            if let Err(e) = h.await {
                                if !e.is_cancelled() {
                                    tracing::error!("запуск Whisper завершился аварийно: {e}");
                                }
                            }
                        }
                        self.server.take();
                    }
                    Effect::Start(g) => {
                        let dir = self.dir.clone();
                        let cfg = self.cfg.clone();
                        let client = self.client.clone();
                        let health = self.health.clone();
                        let tx = self.tx.clone();
                        let gate = self.gate.clone();
                        self.boot = Some(tokio::spawn(async move {
                            let server = boot(&dir, cfg, &client, &health, gate, |p, t, n| {
                                if tx.send(Msg::Update(g, p, t, n)).is_err() {
                                    tracing::debug!("прогресс больше не нужен");
                                }
                            })
                            .await;
                            if tx.send(Msg::Done(g, server)).is_err() {
                                tracing::debug!("движок закрыли во время запуска");
                            }
                        }));
                    }
                    Effect::KillStale(_) => {
                        drop(result.take());
                    }
                    Effect::Publish => {
                        self.view.send_replace(self.state.view.clone());
                        emit(&self.app, "engine", self.state.view.clone());
                    }
                    Effect::Resolve(url) => {
                        if let Some(server) = result.take() {
                            self.server = Some(server);
                            self.boot.take();
                        }
                        for reply in self.waiters.drain(..) {
                            if reply.send(Ok(url.clone())).is_err() {
                                tracing::debug!("запрос готовности отменён");
                            }
                        }
                    }
                    Effect::Reject(msg) => {
                        tracing::warn!(%msg);
                        for reply in self.waiters.drain(..) {
                            if reply.send(Err(anyhow!(msg.clone()))).is_err() {
                                tracing::debug!("запрос готовности отменён");
                            }
                        }
                    }
                    Effect::Exit => return,
                }
            }
        }
    }
}
async fn boot(
    dir: &Path,
    cfg: Config,
    client: &reqwest::Client,
    health: &reqwest::Client,
    gate: Arc<Semaphore>,
    progress: impl Fn(Phase, String, Option<u64>),
) -> Result<Server> {
    let (exe, model, vad) = download::assets(dir, &cfg.whisper_model, client, |label, n| {
        progress(
            Phase::Downloading,
            n.map_or_else(|| label.into(), |v| format!("{label} {v}%")),
            n,
        )
    })
    .await?;
    progress(Phase::Loading, "запускаю Whisper…".into(), None);
    let threads = if cfg.whisper_threads == 0 {
        std::thread::available_parallelism()
            .map_or(4, usize::from)
            .saturating_sub(2)
            .clamp(2, 8) as u32
    } else {
        cfg.whisper_threads
    };
    for attempt in 0..2 {
        // A stale BootDone can still own the old job in the mailbox. Release that job before spawning.
        let permit = gate.clone().acquire_owned().await?;
        let port = TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port();
        let url = format!("http://127.0.0.1:{port}");
        let mut cmd = Command::new(&exe);
        cmd.current_dir(exe.parent().context("нет папки whisper-server")?)
            .arg("-m")
            .arg(&model)
            .args([
                "-l",
                &cfg.language,
                "-t",
                &threads.to_string(),
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "-nt",
                "-sns",
                "--vad",
                "-vm",
            ])
            .arg(&vad)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = job(cmd).with_context(|| format!("не запустил {}", exe.display()))?;
        let log = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(r) = child.stdout().take() {
            tokio::spawn(log_pipe(r, log.clone()));
        }
        if let Some(r) = child.stderr().take() {
            tokio::spawn(log_pipe(r, log.clone()));
        }
        let mut s = Server {
            child,
            url,
            log,
            _permit: permit,
        };
        let start = Instant::now();
        let endpoint = format!("{}/health", s.url);
        let health_check = async {
            loop {
                if health
                    .get(&endpoint)
                    .timeout(Duration::from_millis(1500))
                    .send()
                    .await
                    .is_ok_and(|r| r.status().is_success())
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
        };
        let exit = tokio::select! {
            r=s.child.wait()=>Some(r?),
            res=tokio::time::timeout(Duration::from_secs(120),health_check)=>{if res.is_err(){bail!("Whisper не запустился за {} с: {}",start.elapsed().as_secs(),s.tail());}s.child.try_wait()?}
        };
        if let Some(status) = exit {
            // The last line may still be buffered by the two pipe readers.
            tokio::time::sleep(Duration::from_millis(30)).await;
            let log = s.tail();
            let lower = log.to_lowercase();
            if attempt == 0
                && ["bind", "listen", "address already in use"]
                    .iter()
                    .any(|s| lower.contains(s))
            {
                continue;
            }
            bail!("whisper-server закрылся ({status}): {log}");
        }
        return Ok(s);
    }
    bail!("whisper-server дважды не смог занять локальный порт")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn drop_terminates_job() {
        let sys = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
        let mut cmd = Command::new(sys.join("ping.exe"));
        cmd.args(["-n", "30", "127.0.0.1"]).stdout(Stdio::null());
        let child = job(cmd).unwrap();
        let pid = child.id().unwrap();
        drop(child);
        let until = Instant::now() + Duration::from_secs(4);
        loop {
            let out = Command::new(sys.join("tasklist.exe"))
                .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
                .output()
                .await
                .unwrap();
            assert!(out.status.success());
            if !String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\"")) {
                break;
            }
            assert!(
                Instant::now() < until,
                "процесс {pid} пережил сброс Job Object"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}
