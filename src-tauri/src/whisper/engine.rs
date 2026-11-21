use serde::Serialize;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) enum Phase {
    #[serde(rename = "idle")]
    Idle,
    #[serde(rename = "download")]
    Downloading,
    #[serde(rename = "loading")]
    Loading,
    #[serde(rename = "ready")]
    Ready,
    #[serde(rename = "error")]
    Failed,
    #[serde(rename = "external")]
    External,
}
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Snapshot {
    pub state: Phase,
    pub text: String,
    pub progress: Option<u64>,
}
impl Snapshot {
    pub(crate) fn ready(&self) -> bool {
        matches!(self.state, Phase::Ready | Phase::External)
    }
}
#[derive(Clone, Debug)]
struct State {
    gen: u64,
    view: Snapshot,
    url: String,
    retry: Option<Instant>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            gen: 0,
            view: Snapshot {
                state: Phase::Idle,
                text: String::new(),
                progress: None,
            },
            url: String::new(),
            retry: None,
        }
    }
}
enum Event {
    Ensure(Instant),
    Reconfigure(String),
    Progress(u64, Phase, String, Option<u64>),
    BootDone(u64, String),
    Failed(u64, String, Instant),
    Shutdown,
}
#[derive(Debug, PartialEq, Eq)]
enum Effect {
    Cancel,
    Start(u64),
    KillStale(u64),
    Resolve(String),
    Reject(String),
    Publish,
    Exit,
}
fn step(mut s: State, event: Event) -> (State, Vec<Effect>) {
    let mut effects = Vec::new();
    match event {
        Event::Ensure(now) => match s.view.state {
            Phase::Ready | Phase::External => effects.push(Effect::Resolve(s.url.clone())),
            Phase::Failed if s.retry.is_some_and(|t| now < t) => {
                effects.push(Effect::Reject(s.view.text.clone()))
            }
            Phase::Idle | Phase::Failed => {
                s.gen += 1;
                s.retry = None;
                s.view = Snapshot {
                    state: Phase::Downloading,
                    text: "проверяю файлы Whisper…".into(),
                    progress: None,
                };
                effects.extend([Effect::Publish, Effect::Start(s.gen)]);
            }
            Phase::Downloading | Phase::Loading => {}
        },
        Event::Reconfigure(url) => {
            s.gen += 1;
            s.retry = None;
            s.url = url.trim().trim_end_matches('/').into();
            effects.push(Effect::Cancel);
            if s.url.is_empty() {
                s.view = Snapshot {
                    state: Phase::Idle,
                    text: String::new(),
                    progress: None,
                };
            } else {
                s.view = Snapshot {
                    state: Phase::External,
                    text: "внешний Whisper".into(),
                    progress: None,
                };
                effects.push(Effect::Resolve(s.url.clone()));
            }
            effects.push(Effect::Publish);
        }
        Event::Progress(g, phase, text, progress) if g == s.gen => {
            s.view = Snapshot {
                state: phase,
                text,
                progress,
            };
            effects.push(Effect::Publish);
        }
        Event::BootDone(g, url) => {
            if g == s.gen {
                s.url = url.clone();
                s.view = Snapshot {
                    state: Phase::Ready,
                    text: "Whisper готов".into(),
                    progress: None,
                };
                effects.extend([Effect::Publish, Effect::Resolve(url)]);
            } else {
                effects.push(Effect::KillStale(g));
            }
        }
        Event::Failed(g, text, t) if g == s.gen => {
            s.retry = Some(t + Duration::from_secs(20));
            s.url.clear();
            s.view = Snapshot {
                state: Phase::Failed,
                text: text.clone(),
                progress: None,
            };
            effects.extend([Effect::Cancel, Effect::Publish, Effect::Reject(text)]);
        }
        Event::Shutdown => {
            s.gen += 1;
            effects.extend([
                Effect::Cancel,
                Effect::Reject("Whisper остановлен".into()),
                Effect::Exit,
            ]);
        }
        _ => {}
    }
    (s, effects)
}

#[cfg(windows)]
mod runtime;
#[cfg(windows)]
pub(crate) use runtime::{job, Engine};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reconfigure_cancels_boot_and_disposes_old_result() {
        let (s, _) = step(State::default(), Event::Ensure(Instant::now()));
        let old = s.gen;
        let (s, e) = step(s, Event::Reconfigure(String::new()));
        assert!(e.contains(&Effect::Cancel));
        let (s, _) = step(s, Event::Ensure(Instant::now()));
        let new = s.gen;
        let (s, e) = step(s, Event::BootDone(old, "old".into()));
        assert_eq!(s.gen, new);
        assert_eq!(s.view.state, Phase::Downloading);
        assert_eq!(e, [Effect::KillStale(old)]);
    }
    #[test]
    fn crash_has_cooldown_and_settings_reset_it() {
        let now = Instant::now();
        let (s, _) = step(State::default(), Event::Ensure(now));
        let g = s.gen;
        let (s, _) = step(s, Event::BootDone(g, "url".into()));
        let (s, _) = step(s, Event::Failed(g, "crash".into(), now));
        assert_eq!(s.view.state, Phase::Failed);
        let (s, e) = step(s, Event::Ensure(now + Duration::from_secs(19)));
        assert!(matches!(&e[..], [Effect::Reject(_)]));
        let (s, _) = step(s, Event::Reconfigure(String::new()));
        let (s, e) = step(s, Event::Ensure(now));
        assert!(e.contains(&Effect::Start(s.gen)));
    }
}
