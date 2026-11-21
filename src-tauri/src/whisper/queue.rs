use crate::audio::segmenter::Segment;
use std::collections::VecDeque;

struct Entry {
    segment: Segment,
    during_boot: bool,
}
impl Entry {
    fn stale(&self, now: u64) -> bool {
        self.during_boot || now.saturating_sub(self.segment.ended_at) > 20000
    }
}
#[derive(Default)]
struct Backlog(VecDeque<Entry>);
impl Backlog {
    fn push(&mut self, segment: Segment, ready: bool) {
        let capacity = if ready { 8 } else { 2 };
        while self.0.len() >= capacity {
            self.0.pop_front();
        }
        self.0.push_back(Entry {
            segment,
            during_boot: !ready,
        });
    }
}

#[cfg(windows)]
mod worker {
    use super::{Backlog, Segment};
    use crate::{
        audio::{capture::now_ms, dsp},
        commands::{emit, Runtime},
        whisper::engine::Engine,
    };
    use anyhow::{bail, Context, Result};
    use reqwest::{
        multipart::{Form, Part},
        Client,
    };
    use serde::Deserialize;
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };
    use tauri::{AppHandle, Manager};
    use tokio::sync::Notify;

    #[derive(Clone)]
    pub(crate) struct Queue {
        backlog: Arc<Mutex<Backlog>>,
        notify: Arc<Notify>,
        engine: Engine,
    }
    impl Queue {
        pub(crate) fn new(app: AppHandle, engine: Engine) -> Result<Self> {
            let q = Self {
                backlog: Default::default(),
                notify: Default::default(),
                engine,
            };
            let worker = q.clone();
            let client = Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .build()?;
            tauri::async_runtime::spawn(async move { worker.run(app, client).await });
            Ok(q)
        }
        pub(crate) fn push(&self, segment: Segment) {
            tracing::debug!(reason=?segment.reason, samples=segment.samples.len(),"готов сегмент");
            self.backlog
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(segment, self.engine.view.borrow().ready());
            self.notify.notify_one();
        }
        async fn run(self, app: AppHandle, client: Client) {
            loop {
                if self
                    .backlog
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .0
                    .is_empty()
                {
                    self.notify.notified().await;
                    continue;
                }
                // Leave the front in the bounded queue until the server is ready.
                let url = match self.engine.ensure().await {
                    Ok(url) => url,
                    Err(e) => {
                        self.backlog
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .0
                            .clear();
                        report(&app, e);
                        continue;
                    }
                };
                let e = self
                    .backlog
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .0
                    .pop_front();
                let Some(entry) = e else { continue };
                let Some(wav) = dsp::wav(&entry.segment.samples, entry.segment.rate) else {
                    continue;
                };
                let Some(state) = app.try_state::<Runtime>() else {
                    continue;
                };
                let language = state.config().language;
                match inference(&client, &url, &language, wav).await {
                    Ok(text) => emit(
                        &app,
                        "transcript",
                        serde_json::json!({"text":text,"stale":entry.stale(now_ms()),"endedAt":entry.segment.ended_at}),
                    ),
                    Err(err) => report(&app, err),
                }
            }
        }
    }
    fn report(app: &AppHandle, err: anyhow::Error) {
        tracing::warn!("распознавание: {err:#}");
        emit(
            app,
            "transcript_error",
            serde_json::json!({"message":format!("{err:#}")}),
        );
    }
    #[derive(Deserialize)]
    struct Reply {
        #[serde(default)]
        text: String,
        error: Option<serde_json::Value>,
    }
    async fn inference(client: &Client, url: &str, language: &str, wav: Vec<u8>) -> Result<String> {
        let form = Form::new()
            .part(
                "file",
                Part::bytes(wav)
                    .file_name("speech.wav")
                    .mime_str("audio/wav")?,
            )
            .text("response_format", "json")
            .text("temperature", "0")
            .text("language", language.to_owned());
        let response = client
            .post(format!("{url}/inference"))
            .multipart(form)
            .timeout(Duration::from_secs(90))
            .send()
            .await
            .context("Whisper не ответил")?
            .error_for_status()
            .context("Whisper отклонил фразу")?;
        let reply: Reply = response.json().await.context("не прочитал ответ Whisper")?;
        if let Some(err) = reply.error {
            bail!("Whisper: {err}");
        }
        Ok(reply.text.trim().into())
    }
}
#[cfg(windows)]
pub(crate) use worker::Queue;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::segmenter::Reason;
    fn sample(n: u64) -> Segment {
        Segment {
            samples: vec![],
            rate: 16000,
            ended_at: n,
            reason: Reason::Silence,
        }
    }
    #[test]
    fn overflow_drops_oldest_in_order() {
        let mut q = Backlog::default();
        for i in 0..11 {
            q.push(sample(i), true);
        }
        assert_eq!(
            q.0.iter().map(|e| e.segment.ended_at).collect::<Vec<_>>(),
            (3..11).collect::<Vec<_>>()
        );
    }
    #[test]
    fn warmup_keeps_two_and_no_auto_send() {
        let mut q = Backlog::default();
        for i in 0..20 {
            q.push(sample(i), false);
        }
        assert_eq!(q.0.len(), 2);
        assert_eq!(q.0[0].segment.ended_at, 18);
        assert!(q.0[1].stale(20));
        q.push(sample(20), true);
        assert!(!q.0[2].stale(20020));
        assert!(q.0[2].stale(20021));
    }
}
